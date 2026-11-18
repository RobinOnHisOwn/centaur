use super::*;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::{AtomicBool, Ordering};

async fn call(
    app: &axum::Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn personal_mcp_http_reuses_console_connections_without_a_registry() {
    let Some(store) = test_store().await else {
        return;
    };
    let schema = format!("mcp_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(store.pool())
        .await
        .unwrap();
    let scoped_schema = schema.clone();
    let console = PgPoolOptions::new()
        .max_connections(3)
        .after_connect(move |connection, _| {
            let sql = format!("SET search_path TO {scoped_schema}");
            Box::pin(async move {
                sqlx::query(&sql).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::raw_sql(r#"
        CREATE TABLE users (id bigint PRIMARY KEY,status text);
        CREATE TABLE principals (id bigint PRIMARY KEY,foreign_id text,kind text,console_user_id bigint);
        CREATE TABLE api_clients (id bigint PRIMARY KEY,name text,secret_digest text,scopes jsonb,disabled_at timestamptz,created_by_id bigint);
        CREATE TABLE api_client_principals (id bigint PRIMARY KEY,api_client_id bigint,principal_id bigint,"default" boolean);
        INSERT INTO users VALUES (1,'active'),(2,'active');
        INSERT INTO principals VALUES (101,'personal-a','console_user',1),(102,'personal-b','console_user',2),(103,'shared','service',NULL);
        INSERT INTO api_client_principals VALUES (1,201,101,true),(2,202,102,true),(3,203,103,true),(4,204,102,true),(5,205,101,true);
    "#).execute(&console).await.unwrap();
    for (id, user, scopes) in [
        (201, 1, json!(["sessions:read", "sessions:write"])),
        (202, 2, json!(["sessions:read", "sessions:write"])),
        (203, 1, json!(["sessions:read", "sessions:write"])),
        (204, 1, json!(["sessions:read", "sessions:write"])),
        (205, 1, json!(["sessions:read"])),
    ] {
        sqlx::query("INSERT INTO api_clients VALUES ($1,'fixture',$2,$3,NULL,$4)")
            .bind(id as i64)
            .bind(hex::encode(Sha256::digest(b"fixture-secret")))
            .bind(scopes)
            .bind(user as i64)
            .execute(&console)
            .await
            .unwrap();
    }
    // Only the Console HTTP boundary is synthetic here. Native model/grant
    // behavior is tested against Rails in personal_mcp_native_test.rb.
    let connected = Arc::new(AtomicBool::new(true));
    let current = connected.clone();
    let native = axum::Router::new()
        .route("/api/v1/personal_mcp_connections/{user_id}",axum::routing::get(move |axum::extract::Path(user):axum::extract::Path<i64>,axum::extract::Query(query):axum::extract::Query<std::collections::HashMap<String,String>>,headers:axum::http::HeaderMap| {
            let connected = current.clone(); async move {
                assert_eq!(headers[header::AUTHORIZATION],"Bearer iak_fixture");
                assert_eq!(query["principal"],centaur_console_db::oid("prn",if user==1 {101} else {102}));
                let available = user==1 && connected.load(Ordering::SeqCst);
                axum::Json(json!({"data":{"items":[{"id":"oap_docs","name":"docs","url":"https://mcp.example.com/mcp","status":if available {"connected"} else {"not_connected"},"available":available,"authorization_url":"https://console.example.com/oauth/docs/start"}]}}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control = centaur_iron_control::IronControlClient::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "iak_fixture",
    );
    let server = tokio::spawn(async move { axum::serve(listener, native).await.unwrap() });
    let runtime = centaur_session_runtime::SessionRuntime::new(
        store.clone(),
        SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        TestSessionPrincipalRegistrar,
    );
    let app = build_router_with_app_state(
        AppState::ready(runtime, None, test_auth())
            .with_console_db(console.clone())
            .with_console_control(Some(control)),
    );
    let mut tokens = Vec::new();
    for id in 201..=205 {
        let client = centaur_console_db::oid("acl", id);
        let response=app.clone().oneshot(Request::builder().method("POST").uri("/oauth/token").header(header::CONTENT_TYPE,"application/x-www-form-urlencoded")
            .body(Body::from(format!("grant_type=client_credentials&client_id={client}&client_secret=fixture-secret"))).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        tokens.push(value["access_token"].as_str().unwrap().to_owned());
    }
    // No add/settings write is needed: the existing Console connection suffices.
    let (status, list) = call(&app, &tokens[0], "GET", "/api/mcp/servers", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["configuration_source"], "console_integrations");
    assert_eq!(list["items"][0]["available"], true);
    let (_, other) = call(&app, &tokens[1], "GET", "/api/mcp/servers", Value::Null).await;
    assert_eq!(other["items"][0]["available"], false);
    assert_eq!(
        call(
            &app,
            &tokens[1],
            "DELETE",
            "/api/mcp/servers/docs",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for token in [
        &tokens[2],
        &tokens[3],
        &console_token(),
        &principal_token("prn_test"),
    ] {
        assert_eq!(
            call(&app, token, "GET", "/api/mcp/servers", Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let settings = json!({"name":"docs","url":"https://mcp.example.com/mcp"});
    let (status, recognized) = call(
        &app,
        &tokens[0],
        "POST",
        "/api/mcp/servers",
        settings.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recognized}");
    assert_eq!(
        recognized["authorization_url"],
        "https://console.example.com/oauth/docs/start"
    );
    let (status, login) = call(
        &app,
        &tokens[0],
        "POST",
        "/api/mcp/servers/docs/login",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(login["authorization_url"], recognized["authorization_url"]);
    assert_eq!(
        call(
            &app,
            &tokens[4],
            "POST",
            "/api/mcp/servers",
            settings.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            &tokens[0],
            "POST",
            "/api/mcp/servers",
            json!({"name":"bad","url":"https://other.example.com/mcp"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            &console_token(),
            "GET",
            "/api/console/mcp/users/1/servers",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "parallel Console bridge is gone"
    );
    let client = centaur_console_db::oid("acl", 201);
    let key = format!("api:{client}:mcp-{}", uuid::Uuid::new_v4().simple());
    let path = format!("/api/session/{}", urlencoding::encode(&key));
    for token in [&tokens[0], &console_token()] {
        let (status, _) = call(
            &app,
            token,
            "POST",
            &path,
            json!({"harness_type":"codex","metadata":{"personal_mcp_servers":null}}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "reserved metadata must be rejected regardless of caller"
        );
    }
    let (status, created) = call(
        &app,
        &tokens[0],
        "POST",
        &path,
        json!({"harness_type":"codex","metadata":{"source":"terminal"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(created.get("metadata").is_none());
    let thread = centaur_session_core::ThreadKey::parse(key.clone()).unwrap();
    let snapshot = store.session_mcp_metadata(&thread).await.unwrap();
    assert_eq!(
        snapshot["personal_mcp_servers"]["servers"],
        json!([{"name":"oap_docs","url":"https://mcp.example.com/mcp"}])
    );
    assert!(snapshot["personal_mcp_servers"].get("spoofed").is_none());
    assert_eq!(
        call(
            &app,
            &tokens[0],
            "POST",
            &format!("{path}-pi"),
            json!({"harness_type":"pi"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            &tokens[0],
            "DELETE",
            "/api/mcp/servers/docs",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(connected.load(Ordering::SeqCst));
    // Simulate a subsequent browser-authorized disconnect in Console.
    connected.store(false, Ordering::SeqCst);
    assert_eq!(
        call(
            &app,
            &tokens[0],
            "POST",
            &path,
            json!({"harness_type":"codex"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        store.session_mcp_metadata(&thread).await.unwrap(),
        snapshot,
        "resume retains its execution snapshot"
    );
    let (status, fresh) = call(
        &app,
        &tokens[0],
        "POST",
        &format!("{path}-fresh"),
        json!({"harness_type":"codex"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    let fresh_key =
        centaur_session_core::ThreadKey::parse(fresh["thread_key"].as_str().unwrap()).unwrap();
    assert_eq!(
        store.session_mcp_metadata(&fresh_key).await.unwrap(),
        json!({})
    );
    // A whole page of hidden rows must not hide the allowed row after them.
    let prefix = format!("{key}-page-");
    for n in 0..101 {
        let candidate = centaur_session_core::ThreadKey::parse(format!("{prefix}{n:03}")).unwrap();
        store
            .create_or_get_session(
                &candidate,
                &centaur_session_core::HarnessType::Codex,
                None,
                if n < 100 { snapshot.clone() } else { json!({}) },
                Default::default(),
            )
            .await
            .unwrap();
    }
    sqlx::query("UPDATE sessions SET iron_control_principal=$1, created_at=now()-interval '40 days', updated_at=now()-interval '40 days' WHERE starts_with(thread_key,$2)")
        .bind(centaur_console_db::oid("prn",101)).bind(&prefix).execute(store.pool()).await.unwrap();
    let listing = format!(
        "/api/sessions?after_thread_key={}",
        urlencoding::encode(&prefix)
    );
    let (status, visible) = call(&app, &tokens[0], "GET", &listing, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visible["items"][0]["thread_key"], format!("{prefix}000"));
    sqlx::query("DELETE FROM api_client_principals WHERE api_client_id=201")
        .execute(&console)
        .await
        .unwrap();
    for suffix in ["", "&include_archived=true", "&prunable_only=true"] {
        let (status, page) = call(
            &app,
            &tokens[0],
            "GET",
            &format!("{listing}{suffix}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            page["items"][0]["thread_key"],
            format!("{prefix}100"),
            "hidden rows must not consume a page"
        );
        assert!(!page.to_string().contains(&format!("{prefix}000")));
    }
    sqlx::query("INSERT INTO api_client_principals VALUES (1,201,101,true)")
        .execute(&console)
        .await
        .unwrap();
    let stream = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{path}/events"))
                .header(header::AUTHORIZATION, format!("Bearer {}", tokens[0]))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    sqlx::query("UPDATE users SET status='disabled' WHERE id=1")
        .execute(&console)
        .await
        .unwrap();
    assert_eq!(
        call(&app, &tokens[0], "GET", "/api/mcp/servers", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            &tokens[0],
            "GET",
            &format!("{path}/state"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(35),
        to_bytes(stream.into_body(), 1024 * 1024),
    )
    .await
    .expect("disabled owner loses open stream")
    .unwrap();
    assert!(String::from_utf8_lossy(&closed).contains("API client access ended"));
    server.abort();
    console.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(store.pool())
        .await
        .unwrap();
}
