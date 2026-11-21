//! Thin API-client adapter over Console's native integration registry.
//! No configuration writes or provider-token handling occur in this service.
use crate::{ApiError, api_clients::ApiClientContext, routes::AppState};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use centaur_console_db::{PersonalMcpIdentity, oid};
use centaur_session_core::mcp::{MCP_SNAPSHOT_KEY, McpServer, valid_mcp_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, Serialize)]
struct Connection {
    id: String,
    name: String,
    url: String,
    status: String,
    available: bool,
    authorization_url: String,
}

async fn identity(
    state: &AppState,
    client: Option<Extension<ApiClientContext>>,
) -> Result<PersonalMcpIdentity, ApiError> {
    let client = client
        .ok_or_else(|| ApiError::Forbidden("personal MCP requires an API client".into()))?
        .0;
    client.personal_mcp_identity(&state.console_db()?).await?.filter(|owner| owner.active).ok_or_else(|| ApiError::Forbidden(
        "personal MCP requires an active Console owner and an explicitly attached personal default principal belonging to that owner".into()))
}

async fn connections(
    state: &AppState,
    owner: &PersonalMcpIdentity,
) -> Result<Vec<Connection>, ApiError> {
    let principal = oid("prn", owner.principal_id);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        state
            .console_control()?
            .personal_mcp_connections(owner.user_id, &principal),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Console integration lookup timed out".into()))?
    .map_err(|_| ApiError::ServiceUnavailable("Console integration lookup failed".into()))?;
    serde_json::from_value(response["items"].clone())
        .map_err(|_| ApiError::Internal("invalid Console integration metadata".into()))
}

fn consent_url(connection: &Connection) -> Result<&str, ApiError> {
    let url = reqwest::Url::parse(&connection.authorization_url)
        .map_err(|_| ApiError::Internal("invalid Console consent URL".into()))?;
    if !valid_mcp_name(&connection.name)
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != format!("/oauth/{}/start", connection.name)
    {
        return Err(ApiError::ServiceUnavailable(
            "Console must provide its canonical public HTTPS consent URL".into(),
        ));
    }
    Ok(&connection.authorization_url)
}

pub(crate) async fn list(
    State(state): State<AppState>,
    client: Option<Extension<ApiClientContext>>,
) -> Result<Json<Value>, ApiError> {
    let owner = identity(&state, client).await?;
    list_owner(&state, &owner).await
}

pub(crate) async fn list_owner(
    state: &AppState,
    owner: &PersonalMcpIdentity,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"items":connections(state,owner).await?,"principal":oid("prn",owner.principal_id),"configuration_source":"console_integrations"}),
    ))
}

/// Recognize an existing operator registration. Personal configuration becomes
/// durable only through the existing Console consent/credential/grant flow.
pub(crate) async fn connect(
    State(state): State<AppState>,
    client: Option<Extension<ApiClientContext>>,
    Json(server): Json<McpServer>,
) -> Result<Json<Value>, ApiError> {
    let owner = identity(&state, client).await?;
    connect_owner(&state, &owner, server).await
}

pub(crate) async fn connect_owner(
    state: &AppState,
    owner: &PersonalMcpIdentity,
    server: McpServer,
) -> Result<Json<Value>, ApiError> {
    server.validate().map_err(ApiError::BadRequest)?;
    let item = connections(state, owner).await?.into_iter().find(|item|item.name==server.name && reqwest::Url::parse(&item.url).ok()==reqwest::Url::parse(&server.url).ok())
        .ok_or_else(||ApiError::BadRequest("no matching MCP-enabled Console OAuth app; an operator must register it in the existing Integrations setup first".into()))?;
    Ok(Json(
        json!({"ok":true,"integration":item,"authorization_url":consent_url(&item)?,"configuration_source":"console_integrations","requires_console_login":true}),
    ))
}

pub(crate) async fn login(
    State(state): State<AppState>,
    client: Option<Extension<ApiClientContext>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let owner = identity(&state, client).await?;
    login_owner(&state, &owner, name).await
}

pub(crate) async fn login_owner(
    state: &AppState,
    owner: &PersonalMcpIdentity,
    name: String,
) -> Result<Json<Value>, ApiError> {
    let item = connections(state, owner)
        .await?
        .into_iter()
        .find(|item| item.name == name)
        .ok_or_else(|| ApiError::NotFound("Console MCP integration not found".into()))?;
    Ok(Json(
        json!({"authorization_url":consent_url(&item)?,"principal":oid("prn",owner.principal_id),"requires_console_login":true}),
    ))
}

pub(crate) async fn authorize_saved_session(
    state: &AppState,
    client: &ApiClientContext,
    session: &centaur_session_core::Session,
) -> Result<(), ApiError> {
    if state
        .runtime()?
        .personal_mcp_snapshot(&session.thread_key)
        .await?
        .get(MCP_SNAPSHOT_KEY)
        .is_none()
    {
        return Ok(());
    }
    let owner = identity(state, Some(Extension(client.clone()))).await?;
    if session.iron_control_principal.as_deref() != Some(oid("prn", owner.principal_id).as_str()) {
        return Err(ApiError::Forbidden(
            "personal MCP session no longer belongs to this client's personal identity".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn snapshot(
    state: &AppState,
    client: &ApiClientContext,
    principal: &str,
    metadata: &mut Value,
) -> Result<(), ApiError> {
    let Some(owner) = client.personal_mcp_identity(&state.console_db()?).await? else {
        return Ok(());
    };
    if principal != oid("prn", owner.principal_id) {
        return Ok(());
    }
    snapshot_owner(state, &owner, metadata).await
}

pub(crate) async fn snapshot_owner(
    state: &AppState,
    owner: &PersonalMcpIdentity,
    metadata: &mut Value,
) -> Result<(), ApiError> {
    let principal = oid("prn", owner.principal_id);
    if !owner.active {
        return Err(ApiError::Forbidden(
            "personal Console account is disabled".into(),
        ));
    }
    let servers = connections(state, owner)
        .await?
        .into_iter()
        .filter(|item| item.available)
        .map(|item| McpServer {
            name: item.id,
            url: item.url,
        })
        .collect::<Vec<_>>();
    for server in &servers {
        server.validate().map_err(ApiError::BadRequest)?;
    }
    if !servers.is_empty() {
        metadata[MCP_SNAPSHOT_KEY] = json!({"principal":principal,"servers":servers});
    }
    Ok(())
}
