use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_session_runtime::SessionRuntime;
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Stdio},
};

fn openssl(args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new("openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("OpenSSL test prerequisite");
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success(), "synthetic RSA fixture generation");
    result.stdout
}
struct Environment(
    Vec<(&'static str, Option<std::ffi::OsString>)>,
    std::path::PathBuf,
);
impl Drop for Environment {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        let _ = std::fs::remove_file(&self.1);
    }
}
async fn request(
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
async fn native_cli_jwt_is_live_owner_scoped_and_cannot_enter_legacy_routes() {
    // CI and local acceptance must explicitly supply this disposable database.
    let Some(store) = test_store().await else {
        return;
    };
    let private = openssl(&["genrsa", "-F4", "2048"], None);
    let der = openssl(&["rsa", "-traditional", "-outform", "DER"], Some(&private));
    let modulus =
        String::from_utf8(openssl(&["rsa", "-noout", "-modulus"], Some(&private))).unwrap();
    let modulus = hex::decode(modulus.trim().strip_prefix("Modulus=").unwrap()).unwrap();
    let path =
        std::env::temp_dir().join(format!("centaur-cli-public-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &path,
        json!({"cli-v1":{"n":URL_SAFE_NO_PAD.encode(modulus),"e":"AQAB"}}).to_string(),
    )
    .unwrap();
    let vars = HashMap::from([
        ("CENTAUR_CLI_AUTH_ENABLED", "true".to_owned()),
        (
            "CENTAUR_CLI_SESSION_RESOURCE",
            "https://api.example.test/api/cli/v1".to_owned(),
        ),
        (
            "CENTAUR_CONSOLE_PUBLIC_URL",
            "https://console.example.test".to_owned(),
        ),
        (
            "CENTAUR_CLI_VERIFICATION_KEYS_FILE",
            path.to_str().unwrap().to_owned(),
        ),
    ]);
    let _environment = Environment(
        vars.keys().map(|k| (*k, std::env::var_os(k))).collect(),
        path,
    );
    // These CLI-specific settings are read only by this test's new route path.
    for (k, v) in &vars {
        unsafe { std::env::set_var(k, v) }
    }
    let schema = format!("cli_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(store.pool())
        .await
        .unwrap();
    let scoped = schema.clone();
    let console = PgPoolOptions::new()
        .max_connections(3)
        .after_connect(move |connection, _| {
            let sql = format!("SET search_path TO {scoped}");
            Box::pin(async move {
                sqlx::query(&sql).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let user_id =
        i64::from_str_radix(&uuid::Uuid::new_v4().simple().to_string()[..12], 16).unwrap();
    let other_user = user_id + 1;
    let principal_id = user_id + 2;
    sqlx::raw_sql(&format!("CREATE TABLE users (id bigint PRIMARY KEY,status text); CREATE TABLE principals(id bigint PRIMARY KEY,foreign_id text,kind text,console_user_id bigint); CREATE TABLE cli_oauth_refresh_tokens (connection_id uuid,user_id bigint,scopes jsonb,resources jsonb,used_at timestamp,revoked_at timestamp,expires_at timestamp,connection_expires_at timestamp); INSERT INTO users VALUES({user_id},'active'),({other_user},'active'); INSERT INTO principals VALUES({principal_id},'first','console_user',{user_id}),({},'second','console_user',{other_user});",principal_id+1)).execute(&console).await.unwrap();
    let scopes = "cli:identity cli:sessions:read cli:sessions:write cli:integrations";
    let connection = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO cli_oauth_refresh_tokens VALUES($1::text::uuid,$2,$3,$4,NULL,NULL,now()+interval '1 day',now()+interval '30 days')")
        .bind(&connection).bind(user_id).bind(json!(scopes.split_whitespace().collect::<Vec<_>>())).bind(json!(["https://api.example.test/api/cli/v1"])).execute(&console).await.unwrap();
    let native = axum::Router::new().route(
        "/api/v1/personal_mcp_connections/{user_id}",
        axum::routing::get(|| async { axum::Json(json!({"data":{"items":[]}})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control = centaur_iron_control::IronControlClient::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "iak_fixture",
    );
    let server = tokio::spawn(async move { axum::serve(listener, native).await.unwrap() });
    let runtime = SessionRuntime::new(
        store.clone(),
        SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        TestSessionPrincipalRegistrar,
    );
    let app = build_router_with_app_state(
        AppState::ready(runtime, None, test_auth())
            .with_console_db(console.clone())
            .with_console_control(Some(control)),
    );
    let user = centaur_console_db::oid("usr", user_id);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut header = Header::new(Algorithm::RS256);
    header.typ = Some("centaur-cli+jwt".into());
    header.kid = Some("cli-v1".into());
    let claims = json!({"iss":"https://console.example.test/cli","aud":"https://api.example.test/api/cli/v1","sub":user,"client_id":"centaur-cli","token_use":"cli_user","connection_id":connection,"scope":scopes,"iat":now,"nbf":now,"exp":now+300,"jti":"synthetic-jti"});
    let token = encode(&header, &claims, &EncodingKey::from_rsa_der(&der)).unwrap();
    let (status, me) = request(&app, &token, "GET", "/api/cli/v1/me", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["user"], user);
    let key = format!("personal:{user}:{}", uuid::Uuid::new_v4().simple());
    let path = format!("/api/cli/v1/session/{key}");
    let (status, body) =
        request(&app, &token, "POST", &path, json!({"harness_type":"codex"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["iron_control_principal"],
        centaur_console_db::oid("prn", principal_id)
    );
    let (status, list) = request(&app, &token, "GET", "/api/cli/v1/sessions", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["thread_key"] == key)
    );
    let foreign = format!(
        "/api/cli/v1/session/personal:{}:foreign/state",
        centaur_console_db::oid("usr", other_user)
    );
    assert_eq!(
        request(&app, &token, "GET", &foreign, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            &token,
            "GET",
            &format!("/api/session/{key}/state"),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let mut wrong = claims.clone();
    wrong["aud"] = json!("https://console.example.test/api/cli/v1");
    let wrong = encode(&header, &wrong, &EncodingKey::from_rsa_der(&der)).unwrap();
    assert_eq!(
        request(&app, &wrong, "GET", "/api/cli/v1/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let turn = json!({"message":"synthetic prompt","request_id":"synthetic-request","max_duration_ms":1000});
    let (status, first) =
        request(&app, &token, "POST", &format!("{path}/turn"), turn.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = request(&app, &token, "POST", &format!("{path}/turn"), turn).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(first["execution_id"], second["execution_id"]);
    sqlx::query("UPDATE cli_oauth_refresh_tokens SET revoked_at=now()")
        .execute(&console)
        .await
        .unwrap();
    assert_eq!(
        request(&app, &token, "GET", &format!("{path}/state"), Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    server.abort();
    console.close().await;
    // The runtime owns already-accepted work; revocation deliberately did not cancel it.
}
