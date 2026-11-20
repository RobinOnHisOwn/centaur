use std::collections::BTreeMap;

use centaur_session_core::{HarnessType, MessageRole, SessionMessageInput, ThreadKey};
use centaur_session_sqlx::{DelegatedTurn, PgSessionStore, SessionStoreError, TextSearchBackend};
use serde_json::json;
use uuid::Uuid;

async fn store() -> PgSessionStore {
    let url = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("SESSION_SQLX_TEST_DATABASE_URL"))
        .expect("disposable session database required");
    let store = PgSessionStore::connect(&url).await.unwrap();
    store
        .run_migrations(TextSearchBackend::Postgres)
        .await
        .unwrap();
    store
}

fn turn<'a>(owner: &'a str, request_id: &'a str, text: &str) -> DelegatedTurn<'a> {
    DelegatedTurn {
        owner_prefix: owner,
        request_id,
        message: SessionMessageInput {
            client_message_id: Some(request_id.into()),
            role: MessageRole::User,
            parts: vec![json!({"type":"text", "text":text})],
            metadata: json!({}),
        },
        request: json!({"input_lines": [text], "idempotency_key":request_id}),
        metadata: json!({}),
        max_active: 4,
        submissions_per_minute: 20,
    }
}

async fn session(store: &PgSessionStore, owner: &str, name: &str) -> ThreadKey {
    let key = ThreadKey::parse(format!("{owner}{name}")).unwrap();
    store
        .create_or_get_session(&key, &HarnessType::Codex, None, json!({}), BTreeMap::new())
        .await
        .unwrap();
    key
}

#[tokio::test]
async fn legacy_creator_cannot_bypass_delegated_owner_admission_on_idle_session() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "legacy").await;
    assert!(
        store
            .create_execution_with_request(&key, Some("bypass"), json!({}), json!({}))
            .await
            .is_err()
    );
    assert!(
        store
            .active_execution_for_thread(&key)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .append_messages(&key, &[turn(&owner, "bypass", "bypass").message])
            .await
            .is_err()
    );
    assert!(store.list_messages(&key).await.unwrap().is_empty());
}

#[tokio::test]
async fn atomic_turn_replays_conflicts_and_never_appends_to_busy_work() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "one").await;
    let first = store
        .submit_delegated_turn(&key, turn(&owner, "request-1", "hello"))
        .await
        .unwrap();
    assert!(first.created);
    let replay = store
        .submit_delegated_turn(&key, turn(&owner, "request-1", "hello"))
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(first.execution.execution_id, replay.execution.execution_id);
    assert!(matches!(
        store
            .submit_delegated_turn(&key, turn(&owner, "request-1", "changed"))
            .await,
        Err(SessionStoreError::RequestConflict { .. })
    ));
    assert!(matches!(
        store
            .submit_delegated_turn(&key, turn(&owner, "request-2", "steer"))
            .await,
        Err(SessionStoreError::ExecutionActive { .. })
    ));
    assert_eq!(store.list_messages(&key).await.unwrap().len(), 1);
    assert!(
        store
            .create_execution_with_request(&key, Some("legacy"), json!({}), json!({}))
            .await
            .is_err()
    );
    assert!(
        store
            .submit_delegated_turn(&key, turn("personal:other:", "request-3", "foreign"))
            .await
            .is_err()
    );
    // The same persisted request is recoverable by a new store handle.
    let recovered = PgSessionStore::new(store.pool().clone());
    assert_eq!(
        recovered
            .execution_request(&first.execution.execution_id)
            .await
            .unwrap()["input_lines"],
        json!(["hello"])
    );
}

#[tokio::test]
async fn duplicate_parallel_requests_commit_exactly_one_message_and_execution() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "retry").await;
    let (a, b) = tokio::join!(
        store.submit_delegated_turn(&key, turn(&owner, "same", "same")),
        store.submit_delegated_turn(&key, turn(&owner, "same", "same"))
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.created, b.created);
    assert_eq!(a.execution.execution_id, b.execution.execution_id);
    assert_eq!(store.list_messages(&key).await.unwrap().len(), 1);
}

#[tokio::test]
async fn owner_admission_serializes_parallel_sessions_and_retry_does_not_consume_quota() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let a = session(&store, &owner, "a").await;
    let b = session(&store, &owner, "b").await;
    let mut ta = turn(&owner, "a", "a");
    ta.max_active = 1;
    let mut tb = turn(&owner, "b", "b");
    tb.max_active = 1;
    let (a_result, b_result) = tokio::join!(
        store.submit_delegated_turn(&a, ta),
        store.submit_delegated_turn(&b, tb)
    );
    assert_eq!(
        usize::from(a_result.is_ok()) + usize::from(b_result.is_ok()),
        1
    );
    let (key, request) = if a_result.is_ok() {
        (&a, "a")
    } else {
        (&b, "b")
    };
    let mut retry = turn(&owner, request, request);
    retry.max_active = 1;
    assert!(
        !store
            .submit_delegated_turn(key, retry)
            .await
            .unwrap()
            .created
    );
    assert_eq!(
        store.list_messages(&a).await.unwrap().len() + store.list_messages(&b).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn queued_cancellation_is_durable_and_never_cancels_a_running_successor() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "interrupt").await;
    let first = store
        .submit_delegated_turn(&key, turn(&owner, "one", "one"))
        .await
        .unwrap()
        .execution;
    assert!(
        store
            .cancel_queued_execution(&key, &first.execution_id)
            .await
            .unwrap()
    );
    assert!(
        !store
            .cancel_queued_execution(&key, &first.execution_id)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .execution_snapshot(&key, &first.execution_id)
            .await
            .unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        store
            .list_events_after(&key, 0, Some(&first.execution_id), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let second = store
        .submit_delegated_turn(&key, turn(&owner, "two", "two"))
        .await
        .unwrap()
        .execution;
    store
        .mark_execution_running(&second.execution_id)
        .await
        .unwrap();
    assert!(
        !store
            .cancel_queued_execution(&key, &first.execution_id)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .execution_snapshot(&key, &second.execution_id)
            .await
            .unwrap()["status"],
        "running"
    );
    assert!(
        !store
            .cancel_queued_execution(&key, &second.execution_id)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn results_are_execution_specific_bounded_and_oversized_events_advance() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "results").await;
    let first = store
        .submit_delegated_turn(&key, turn(&owner, "one", "one"))
        .await
        .unwrap()
        .execution;
    store
        .append_event(
            &key,
            Some(&first.execution_id),
            "session.output.line",
            json!({"line":"🦀".repeat(40_000)}),
        )
        .await
        .unwrap();
    for index in 0..20 {
        store
            .append_event(
                &key,
                Some(&first.execution_id),
                "session.output.line",
                json!({"line":if index%2==0 {"x".repeat(8_000)} else {"\\".repeat(3_800)}}),
            )
            .await
            .unwrap();
    }
    sqlx::query("UPDATE session_executions SET status='failed', error=$2, completed_at=now() WHERE execution_id=$1").bind(&first.execution_id).bind("\u{0001}".repeat(2048)).execute(store.pool()).await.unwrap();
    let second = store
        .submit_delegated_turn(&key, turn(&owner, "two", "two"))
        .await
        .unwrap()
        .execution;
    store
        .append_event(
            &key,
            Some(&second.execution_id),
            "session.output.line",
            json!({"line":"successor"}),
        )
        .await
        .unwrap();
    let mut cursor = 0;
    let mut count = 0;
    loop {
        let page = store
            .execution_result_page(&key, &first.execution_id, cursor, 100)
            .await
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 16 * 1024);
        let mcp_result =
            json!({"structuredContent":page,"content":[{"type":"text","text":page.to_string()}]});
        assert!(serde_json::to_vec(&mcp_result).unwrap().len() < 64 * 1024);
        assert_eq!(page["execution"]["status"], "failed");
        assert_eq!(page["execution"]["error_truncated"], true);
        assert_eq!(page["retained_only"], true);
        for event in page["events"].as_array().unwrap() {
            assert_eq!(event["execution_id"], first.execution_id);
            if count == 0 {
                assert_eq!(event["payload"]["omitted"], true);
            }
            count += 1;
        }
        let next = page["next_after_event_id"].as_i64().unwrap();
        assert!(next > cursor);
        cursor = next;
        if !page["has_more"].as_bool().unwrap() {
            break;
        }
    }
    assert_eq!(count, 21);
    assert!(
        store
            .execution_result_page(&key, &second.execution_id, cursor, 10)
            .await
            .is_err()
    );
    let other = session(&store, &owner, "other").await;
    assert!(
        store
            .execution_result_page(&other, &first.execution_id, 0, 10)
            .await
            .is_err()
    );
    assert!(
        store
            .execution_result_page(&key, &first.execution_id, 0, 101)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_message_insert_rolls_back_execution_and_rate_limit_is_durable() {
    let store = store().await;
    let owner = format!("personal:{}:", Uuid::new_v4());
    let key = session(&store, &owner, "one").await;
    // Force the MESSAGE insert to fail after the execution insert succeeded.
    // Invalid JSON would fail earlier, proving nothing about atomic rollback.
    sqlx::query("INSERT INTO session_messages (message_id,thread_key,client_message_id,role,parts,metadata) VALUES ($1,$2,'bad','user','[]','{}')")
        .bind(format!("msg_{}",Uuid::new_v4())).bind(key.as_str()).execute(store.pool()).await.unwrap();
    let error = store
        .submit_delegated_turn(&key, turn(&owner, "bad", "bad"))
        .await
        .unwrap_err();
    assert!(
        matches!(error,SessionStoreError::Sqlx(ref error) if error.as_database_error().and_then(|error| error.code()).as_deref()==Some("23505"))
    );
    assert!(
        store
            .active_execution_for_thread(&key)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(store.list_messages(&key).await.unwrap().len(), 1);
    sqlx::query("DELETE FROM session_messages WHERE thread_key=$1")
        .bind(key.as_str())
        .execute(store.pool())
        .await
        .unwrap();
    let first = store
        .submit_delegated_turn(&key, turn(&owner, "ok", "ok"))
        .await
        .unwrap();
    sqlx::query("UPDATE session_executions SET status='completed', completed_at=now() WHERE execution_id=$1").bind(&first.execution.execution_id).execute(store.pool()).await.unwrap();
    let mut limited = turn(&owner, "next", "next");
    limited.submissions_per_minute = 1;
    assert!(matches!(
        store.submit_delegated_turn(&key, limited).await,
        Err(SessionStoreError::AdmissionLimited { .. })
    ));
    assert_eq!(store.list_messages(&key).await.unwrap().len(), 1);
}
