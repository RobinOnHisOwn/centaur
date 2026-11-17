//! HTTP contract tests, not a real model-provider/Kubernetes acceptance test.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use centaur_session_core::ThreadKey;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;

use super::client::CentaurClient;

#[derive(Clone, Default)]
struct ServerState {
    tokens: Arc<AtomicUsize>,
    streams: Arc<AtomicUsize>,
    cursors: Arc<Mutex<Vec<i64>>>,
}

#[derive(Deserialize)]
struct Cursor {
    after_event_id: i64,
}

async fn token(State(state): State<ServerState>, headers: HeaderMap) -> Json<serde_json::Value> {
    assert!(
        headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Basic ")
    );
    let index = state.tokens.fetch_add(1, Ordering::SeqCst) + 1;
    Json(json!({"access_token":format!("test-token-{index}"), "expires_in":3600}))
}

async fn events(
    State(state): State<ServerState>,
    Query(cursor): Query<Cursor>,
    headers: HeaderMap,
) -> Response {
    state.cursors.lock().unwrap().push(cursor.after_event_id);
    let index = state.streams.fetch_add(1, Ordering::SeqCst);
    if index == 1 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let expected = if index == 0 {
        "Bearer test-token-1"
    } else {
        "Bearer test-token-2"
    };
    assert_eq!(headers["authorization"], expected);
    let body = if index == 0 {
        "id: 1\nevent: session.output.line\ndata: {\"type\":\"first\"}\n\nevent: session.stream_error\ndata: {}\n\n"
    } else {
        // Deliberately replay a duplicate: the client must not display it twice.
        "id: 1\nevent: session.output.line\ndata: {\"type\":\"first\"}\n\nid: 2\nevent: session.execution_completed\ndata: {}\n\n"
    };
    ([("content-type", "text/event-stream")], body).into_response()
}

#[tokio::test]
async fn reconnects_from_last_id_renews_auth_and_deduplicates_replay() {
    let state = ServerState::default();
    let app = Router::new()
        .route("/oauth/token", post(token))
        .route("/api/session/{key}/events", get(events))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = CentaurClient::with_client_credentials(
        &url,
        "acl_test".to_owned(),
        "fixture-secret".to_owned(),
    )
    .unwrap();
    let key = ThreadKey::parse("api:acl_test:one").unwrap();
    let mut stream = client.reconnecting_events(&key, 0);
    let ids = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut ids = Vec::new();
        while ids.len() < 2 {
            let event = stream.next().await.unwrap().unwrap();
            if !event.id.is_empty() {
                ids.push(event.id);
            }
        }
        ids
    })
    .await
    .unwrap();
    assert_eq!(ids, ["1", "2"]);
    assert_eq!(state.tokens.load(Ordering::SeqCst), 2);
    assert_eq!(*state.cursors.lock().unwrap(), vec![0, 1, 1]);
    // Dropping an event reader performs no cancellation request.
    drop(stream);
    assert_eq!(state.streams.load(Ordering::SeqCst), 3);
    server.abort();
}

#[tokio::test]
async fn forbidden_is_not_retried_as_a_transient_disconnect() {
    let app = Router::new().route(
        "/api/session/{key}/events",
        get(|| async { StatusCode::FORBIDDEN }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = CentaurClient::new(url);
    let key = ThreadKey::parse("api:acl_test:one").unwrap();
    let mut stream = client.reconnecting_events(&key, 0);
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(!error.is_retryable());
    server.abort();
}
