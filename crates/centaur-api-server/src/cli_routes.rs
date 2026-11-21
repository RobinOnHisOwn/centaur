//! Human adapter over durable runtime operations; no machine/service impersonation.
use crate::{
    ApiError,
    cli_auth::{self, Human},
    routes::AppState,
    types::{
        CreateSessionRequest, CreateSessionResponse, EventsQuery, ExecuteSessionResponse,
        OnHarnessConflict, SessionSseEvent,
    },
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    middleware,
    response::Sse,
    response::sse::{Event, KeepAlive},
    routing::{delete, get, post},
};
use centaur_session_core::{MessageRole, SessionMessageInput, ThreadKey};
use centaur_session_runtime::{ExecuteSessionInput, HarnessConflictPolicy};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;

pub(crate) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/cli/v1/me", get(me))
        .route("/api/cli/v1/me/sessions", get(list))
        .route("/api/cli/v1/sessions", get(list))
        .route(
            "/api/cli/v1/session/{key}",
            post(create).get(status).delete(remove),
        )
        .route("/api/cli/v1/session/{key}/state", get(status))
        .route("/api/cli/v1/session/{key}/turn", post(turn))
        .route("/api/cli/v1/session/{key}/archive", post(archive))
        .route("/api/cli/v1/session/{key}/prune", delete(prune))
        .route("/api/cli/v1/session/{key}/sandbox", delete(release))
        .route("/api/cli/v1/session/{key}/interrupt", post(interrupt))
        .route("/api/cli/v1/session/{key}/events", get(events))
        .route("/api/cli/v1/mcp/servers", get(integrations).post(connect))
        .route("/api/cli/v1/mcp/servers/{name}/login", post(consent))
        .route_layer(middleware::from_fn_with_state(state, cli_auth::authorize))
}
async fn me(Extension(h): Extension<Human>) -> Json<Value> {
    Json(
        json!({"user":h.user,"principal":h.principal,"thread_key_prefix":h.prefix(),"scope":h.claims.scope,"session_ready":h.principal.is_some(),"protocol_version":1}),
    )
}
#[derive(Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    after_thread_key: String,
    #[serde(default)]
    include_archived: bool,
    #[serde(default)]
    prunable_only: bool,
}
async fn list(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = s
        .runtime()?
        .list_visible_client_sessions(
            &h.prefix(),
            &q.after_thread_key,
            q.include_archived || q.prunable_only,
            q.prunable_only,
            Some(h.principal()?),
        )
        .await?;
    let next = (rows.len() == 100)
        .then(|| rows.last().and_then(|r| r["thread_key"].as_str()))
        .flatten();
    Ok(Json(
        json!({"items":rows,"next_after_thread_key":next,"prune_older_than_days":q.prunable_only.then_some(30)}),
    ))
}
async fn create(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Path(key): Path<String>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    if req.principal.is_some()
        || req
            .metadata
            .as_ref()
            .is_some_and(|m| m.as_object().is_none_or(|o| !o.is_empty()))
    {
        return Err(ApiError::BadRequest(
            "human session identity and metadata are server-owned".into(),
        ));
    }
    let key = ThreadKey::parse(key)?;
    let mut metadata = json!({"console_user_id":h.user});
    crate::personal_mcp::snapshot_owner(&s, &h.personal_identity()?, &mut metadata).await?;
    let runtime = s.runtime()?;
    let outcome = runtime
        .create_or_get_session_with_principal(
            &key,
            &req.harness_type,
            req.persona_id.as_deref(),
            Some(metadata),
            match req.on_harness_conflict {
                Some(OnHarnessConflict::Restart) => HarnessConflictPolicy::Restart,
                _ => HarnessConflictPolicy::Reject,
            },
            Some(h.principal()?),
        )
        .await?;
    let persona_model = runtime.persona_model(&outcome.session).map(str::to_owned);
    Ok(Json(CreateSessionResponse {
        session: outcome.session,
        harness_switched: outcome.harness_switched,
        unavailable_requested_persona_id: outcome.unavailable_requested_persona_id,
        persona_model,
    }))
}
async fn status(
    State(s): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.runtime()?.session_state(&ThreadKey::parse(key)?).await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    message: String,
    request_id: String,
    max_duration_ms: u64,
}
async fn turn(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Path(key): Path<String>,
    Json(req): Json<Turn>,
) -> Result<Json<ExecuteSessionResponse>, ApiError> {
    if req.message.trim().is_empty()
        || req.message.len() > 65536
        || req.request_id.is_empty()
        || req.request_id.len() > 128
        || req.max_duration_ms == 0
        || req.max_duration_ms > 3_600_000
    {
        return Err(ApiError::BadRequest(
            "invalid prompt, request ID or duration".into(),
        ));
    }
    let key = ThreadKey::parse(key)?;
    let input = json!({"type":"user","message":{"content":[{"type":"text","text":req.message}]}})
        .to_string();
    let execution = s
        .runtime()?
        .enqueue_delegated_turn(
            &key,
            &h.prefix(),
            SessionMessageInput {
                client_message_id: Some(req.request_id.clone()),
                role: MessageRole::User,
                parts: vec![json!({"type":"text","text":req.message})],
                metadata: json!({}),
            },
            ExecuteSessionInput {
                idempotency_key: Some(req.request_id),
                metadata: Some(json!({"source":"human_cli"})),
                input_lines: vec![input],
                idle_timeout_ms: None,
                max_duration_ms: Some(req.max_duration_ms),
            },
            4,
            20,
        )
        .await?;
    Ok(Json(ExecuteSessionResponse {
        ok: true,
        execution_id: execution.execution_id,
        thread_key: key,
        status: execution.status.to_string(),
    }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    archived: bool,
}
async fn archive(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(req): Json<Archive>,
) -> Result<Json<Value>, ApiError> {
    s.runtime()?
        .set_session_archived(&ThreadKey::parse(k)?, req.archived)
        .await?;
    Ok(Json(json!({"archived":req.archived})))
}
async fn remove(State(s): State<AppState>, Path(k): Path<String>) -> Result<Json<Value>, ApiError> {
    s.runtime()?.delete_session(&ThreadKey::parse(k)?).await?;
    Ok(Json(json!({"deleted": true})))
}
async fn prune(State(s): State<AppState>, Path(k): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"deleted":s.runtime()?.prune_session(&ThreadKey::parse(k)?).await?}),
    ))
}
async fn release(
    State(s): State<AppState>,
    Path(k): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = s
        .runtime()?
        .release_session_sandbox(&ThreadKey::parse(k)?)
        .await?;
    Ok(Json(json!({"released":id.is_some(),"sandbox_id":id})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Interrupt {
    execution_id: String,
}
async fn interrupt(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(req): Json<Interrupt>,
) -> Result<Json<Value>, ApiError> {
    let o = s
        .runtime()?
        .interrupt_execution(
            &ThreadKey::parse(k)?,
            &req.execution_id,
            "Interrupted from terminal",
        )
        .await?;
    Ok(Json(
        json!({"ok":true,"requested":o.requested,"interrupted":o.interrupted,"execution_id":o.execution_id}),
    ))
}
async fn events(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Extension(raw): Extension<String>,
    Path(k): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let stream = s
        .runtime()?
        .stream_events(
            &ThreadKey::parse(k)?,
            q.after_event_id.unwrap_or(0),
            q.execution_id.as_deref(),
        )
        .await?;
    let stream = stream.map(|e| {
        Ok(e.ok()
            .and_then(|e| SessionSseEvent::try_from(e).ok())
            .map(Event::from)
            .unwrap_or_else(|| {
                Event::default()
                    .event("session.stream_error")
                    .data("event stream failed")
            }))
    });
    Ok(Sse::new(crate::api_clients::end_events_when(
        stream,
        cli_auth::access_ended(s, raw, h.principal),
    ))
    .keep_alive(KeepAlive::default()))
}
async fn integrations(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
) -> Result<Json<Value>, ApiError> {
    crate::personal_mcp::list_owner(&s, &h.personal_identity()?).await
}
async fn connect(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Json(server): Json<centaur_session_core::mcp::McpServer>,
) -> Result<Json<Value>, ApiError> {
    crate::personal_mcp::connect_owner(&s, &h.personal_identity()?, server).await
}
async fn consent(
    State(s): State<AppState>,
    Extension(h): Extension<Human>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    crate::personal_mcp::login_owner(&s, &h.personal_identity()?, name).await
}
