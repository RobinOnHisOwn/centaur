//! Atomic, retry-safe turns for personally owned sessions. No sandbox I/O.
use centaur_session_core::{MessageRole, SessionMessageInput, ThreadKey};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};

use crate::{
    CreateExecutionResult, PgSessionStore, SessionEventRow, SessionExecutionRow, SessionStoreError,
    prefixed_id,
};

pub struct DelegatedTurn<'a> {
    /// Constructed by server authorization, never from tool arguments.
    pub owner_prefix: &'a str,
    pub request_id: &'a str,
    pub message: SessionMessageInput,
    pub request: Value,
    pub metadata: Value,
    pub max_active: i64,
    pub submissions_per_minute: i64,
}

async fn lock_session(
    tx: &mut Transaction<'_, Postgres>,
    key: &ThreadKey,
) -> Result<(), SessionStoreError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT thread_key FROM sessions WHERE thread_key=$1 FOR UPDATE")
            .bind(key.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    if found.is_none() {
        return Err(SessionStoreError::NotFound {
            thread_key: key.to_string(),
        });
    }
    Ok(())
}

async fn reject_busy_turn(
    tx: &mut Transaction<'_, Postgres>,
    key: &ThreadKey,
) -> Result<(), SessionStoreError> {
    let active: Option<String> = sqlx::query_scalar("SELECT execution_id FROM session_executions WHERE thread_key=$1 AND status IN ('queued','running') LIMIT 1")
        .bind(key.as_str()).fetch_optional(&mut **tx).await?;
    if let Some(execution_id) = active {
        return Err(SessionStoreError::ExecutionActive {
            thread_key: key.to_string(),
            execution_id,
        });
    }
    Ok(())
}

impl PgSessionStore {
    /// Capability is scoped to an execution, not inferred from configured image tags.
    pub async fn supports_execution_interrupt(
        &self,
        key: &ThreadKey,
        execution_id: &str,
        instance_id: &str,
    ) -> Result<bool, SessionStoreError> {
        Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM session_events WHERE thread_key=$1 AND execution_id=$2 AND event_type='session.interrupt_capability' AND payload->'version'='1'::jsonb AND payload->>'sandbox_instance_id'=$3)")
            .bind(key.as_str()).bind(execution_id).bind(instance_id).fetch_one(&self.pool).await?)
    }

    /// Cancel only this queued execution. The driver claims with a conditional
    /// queued->running update, so it cannot start a successfully cancelled turn.
    pub async fn cancel_queued_execution(
        &self,
        key: &ThreadKey,
        execution_id: &str,
    ) -> Result<bool, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        lock_session(&mut tx, key).await?;
        let cancelled: Option<String> = sqlx::query_scalar("UPDATE session_executions SET status='cancelled',completed_at=now(),updated_at=now() WHERE thread_key=$1 AND execution_id=$2 AND status='queued' RETURNING execution_id")
            .bind(key.as_str()).bind(execution_id).fetch_optional(&mut *tx).await?;
        if cancelled.is_none() {
            return Ok(false);
        }
        sqlx::query("UPDATE sessions SET status='idle',updated_at=now() WHERE thread_key=$1")
            .bind(key.as_str())
            .execute(&mut *tx)
            .await?;
        crate::insert_session_event(
            &mut *tx,
            key,
            Some(execution_id),
            "session.execution_cancelled",
            json!({"reason":"explicit interruption before dispatch"}),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// A public execution snapshot deliberately excludes request/metadata and
    /// always binds the execution to its session (never to latest/current).
    pub async fn execution_snapshot(
        &self,
        key: &ThreadKey,
        execution_id: &str,
    ) -> Result<Value, SessionStoreError> {
        sqlx::query_scalar("SELECT jsonb_build_object('execution_id',execution_id,'thread_key',thread_key,'status',status,'error',left(error,512),'error_truncated',coalesce(length(error)>512,false),'created_at',created_at,'started_at',started_at,'completed_at',completed_at) FROM session_executions WHERE thread_key=$1 AND execution_id=$2")
            .bind(key.as_str()).bind(execution_id).fetch_optional(&self.pool).await?
            .ok_or_else(|| SessionStoreError::ExecutionNotFound { execution_id:execution_id.into() })
    }

    /// Retained events, not a claim of complete historical output. Payloads
    /// over 8 KiB are replaced in SQL before crossing the database boundary.
    /// Their event ID still advances the cursor, preventing infinite polling.
    /// The 16-KiB page leaves room for MCP's structured and escaped text copies.
    pub async fn execution_result_page(
        &self,
        key: &ThreadKey,
        execution_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Value, SessionStoreError> {
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(SessionStoreError::InvalidTurn(
                "invalid result cursor or page size".into(),
            ));
        }
        let execution = self.execution_snapshot(key, execution_id).await?;
        if after != 0 {
            let valid: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM session_events WHERE thread_key=$1 AND execution_id=$2 AND event_id=$3)")
                .bind(key.as_str()).bind(execution_id).bind(after).fetch_one(&self.pool).await?;
            if !valid {
                return Err(SessionStoreError::InvalidTurn(
                    "event cursor is unknown or expired".into(),
                ));
            }
        }
        let rows: Vec<SessionEventRow> = sqlx::query_as("SELECT event_id,thread_key,execution_id,left(event_type,128) AS event_type,created_at, CASE WHEN octet_length(payload::text)>8192 THEN jsonb_build_object('omitted',true,'reason','event exceeds byte limit','bytes',octet_length(payload::text)) ELSE payload END AS payload FROM session_events WHERE thread_key=$1 AND execution_id=$2 AND event_id>$3 ORDER BY event_id LIMIT $4")
            .bind(key.as_str()).bind(execution_id).bind(after).bind(limit+1).fetch_all(&self.pool).await?;
        let mut page = json!({"execution":execution,"events":[],"next_after_event_id":after,"has_more":rows.len()>limit as usize,"retained_only":true});
        // Reserve cursor/envelope growth; serialize each bounded event once.
        let mut bytes = page.to_string().len() + 128;
        for row in rows.into_iter().take(limit as usize) {
            let event: centaur_session_core::SessionEvent = row.try_into()?;
            let value = json!(event);
            bytes += value.to_string().len() + 1;
            if bytes > 16 * 1024 {
                page["has_more"] = json!(true);
                break;
            }
            page["next_after_event_id"] = json!(event.event_id);
            page["events"]
                .as_array_mut()
                .expect("constructed array")
                .push(value);
        }
        Ok(page)
    }

    /// Persist one message and its queued execution together. A rejected/replayed
    /// request cannot append or steer. Dispatch happens only after this commits.
    pub async fn submit_delegated_turn(
        &self,
        key: &ThreadKey,
        turn: DelegatedTurn<'_>,
    ) -> Result<CreateExecutionResult, SessionStoreError> {
        if !turn.owner_prefix.starts_with("personal:")
            || !turn.owner_prefix.ends_with(':')
            || !key.as_str().starts_with(turn.owner_prefix)
        {
            return Err(SessionStoreError::NotFound {
                thread_key: key.to_string(),
            });
        }
        if turn.request_id.is_empty()
            || turn.request_id.len() > 128
            || turn.message.client_message_id.as_deref() != Some(turn.request_id)
            || turn.message.role != MessageRole::User
            || !turn.request.is_object()
            || turn.request.get("centaur.delegation").is_some()
            || turn.max_active < 1
            || turn.submissions_per_minute < 1
        {
            return Err(SessionStoreError::InvalidTurn(
                "invalid delegated turn".into(),
            ));
        }
        let mut request = turn.request;
        request["centaur.delegation"] = json!({"version":1,"message":turn.message});
        let mut tx = self.pool.begin().await?;
        // Serialize owner-wide quotas across processes, then lock this session
        // against concurrent targeted interruption. Legacy enqueue/append refuse
        // this namespace, so they cannot bypass quotas or implicitly steer.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('centaur:delegation:' || $1::text,0))",
        )
        .bind(turn.owner_prefix)
        .execute(&mut *tx)
        .await?;
        lock_session(&mut tx, key).await?;
        let existing: Option<(String, Value)> = sqlx::query_as("SELECT execution_id, request FROM session_executions WHERE thread_key=$1 AND idempotency_key=$2")
            .bind(key.as_str()).bind(turn.request_id).fetch_optional(&mut *tx).await?;
        if let Some((id, previous)) = existing {
            if previous != request {
                return Err(SessionStoreError::RequestConflict {
                    request_id: turn.request_id.into(),
                });
            }
            let execution: SessionExecutionRow =
                sqlx::query_as("SELECT * FROM session_executions WHERE execution_id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            tx.commit().await?;
            return Ok(CreateExecutionResult {
                execution: execution.try_into()?,
                created: false,
            });
        }
        reject_busy_turn(&mut tx, key).await?;
        let (active, recent): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE status IN ('queued','running')), count(*) FILTER (WHERE created_at > now()-interval '1 minute') FROM session_executions WHERE starts_with(thread_key,$1)")
            .bind(turn.owner_prefix).fetch_one(&mut *tx).await?;
        if active >= turn.max_active || recent >= turn.submissions_per_minute {
            return Err(SessionStoreError::AdmissionLimited {
                owner: turn.owner_prefix.into(),
            });
        }
        let execution: SessionExecutionRow = sqlx::query_as("INSERT INTO session_executions (execution_id,thread_key,idempotency_key,status,metadata,request) VALUES ($1,$2,$3,'queued',$4,$5) RETURNING *")
            .bind(prefixed_id("exe")).bind(key.as_str()).bind(turn.request_id)
            .bind(turn.metadata).bind(request).fetch_one(&mut *tx).await?;
        // Deliberately no ON CONFLICT: an unrelated preexisting message with
        // this client ID must roll back the execution, not be silently reused.
        sqlx::query("INSERT INTO session_messages (message_id,thread_key,client_message_id,role,parts,metadata) VALUES ($1,$2,$3,'user',$4,$5)")
            .bind(prefixed_id("msg")).bind(key.as_str()).bind(turn.request_id)
            .bind(Value::Array(turn.message.parts)).bind(turn.message.metadata)
            .execute(&mut *tx).await?;
        let execution = execution.try_into()?;
        tx.commit().await?;
        Ok(CreateExecutionResult {
            execution,
            created: true,
        })
    }
}
