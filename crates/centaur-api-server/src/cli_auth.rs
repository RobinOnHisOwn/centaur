//! Purpose-isolated human authentication. Never accepted by legacy API routes.
use crate::{ApiError, routes::AppState};
use axum::{
    extract::{Request, State},
    http::{Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::HashMap, time::Duration};

#[derive(Clone)]
pub(crate) struct Human {
    pub user_id: i64,
    pub user: String,
    pub principal: Option<String>,
    pub claims: Claims,
}
#[derive(Clone, Deserialize)]
pub(crate) struct Claims {
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
    pub token_use: String,
    pub client_id: String,
    pub connection_id: String,
    pub scope: String,
    pub jti: String,
}
#[derive(Deserialize)]
struct PublicKey {
    n: String,
    e: String,
}
fn denied() -> ApiError {
    ApiError::Unauthorized("invalid or expired human CLI authorization".into())
}
fn unavailable() -> ApiError {
    ApiError::ServiceUnavailable("human CLI authorization unavailable".into())
}

pub(crate) async fn authenticate(state: &AppState, raw: &str) -> Result<Human, ApiError> {
    if std::env::var("CENTAUR_CLI_AUTH_ENABLED").as_deref() != Ok("true") {
        return Err(denied());
    }
    let header = decode_header(raw).map_err(|_| denied())?;
    if header.alg != Algorithm::RS256 || header.typ.as_deref() != Some("centaur-cli+jwt") {
        return Err(denied());
    }
    let keys: HashMap<String, PublicKey> = serde_json::from_slice(
        &std::fs::read(
            std::env::var("CENTAUR_CLI_VERIFICATION_KEYS_FILE").map_err(|_| unavailable())?,
        )
        .map_err(|_| unavailable())?,
    )
    .map_err(|_| unavailable())?;
    let key = header
        .kid
        .as_ref()
        .and_then(|kid| keys.get(kid))
        .ok_or_else(denied)?;
    let resource = std::env::var("CENTAUR_CLI_SESSION_RESOURCE").map_err(|_| unavailable())?;
    let issuer = format!(
        "{}/cli",
        std::env::var("CENTAUR_CONSOLE_PUBLIC_URL")
            .map_err(|_| unavailable())?
            .trim_end_matches('/')
    );
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[&resource]);
    validation.set_issuer(&[&issuer]);
    validation.validate_nbf = true;
    validation.leeway = 5;
    let claims = decode::<Claims>(
        raw,
        &DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| unavailable())?,
        &validation,
    )
    .map_err(|_| denied())?
    .claims;
    if claims.token_use != "cli_user"
        || claims.client_id != "centaur-cli"
        || claims.jti.is_empty()
        || claims.exp - claims.iat > 300
        || claims.exp <= time::OffsetDateTime::now_utc().unix_timestamp()
    {
        return Err(denied());
    }
    let user_id = centaur_console_db::decode_oid("usr", &claims.sub).ok_or_else(denied)?;
    let connection = uuid::Uuid::parse_str(&claims.connection_id).map_err(|_| denied())?;
    let row = sqlx::query("SELECT t.scopes, ARRAY(SELECT p.id FROM principals p WHERE p.console_user_id=u.id AND p.kind='console_user' ORDER BY p.id LIMIT 2) AS principals FROM cli_oauth_refresh_tokens t JOIN users u ON u.id=t.user_id AND u.status='active' WHERE t.connection_id=$1::text::uuid AND t.user_id=$2 AND t.used_at IS NULL AND t.revoked_at IS NULL AND t.expires_at>now() AND t.connection_expires_at>now() AND t.resources @> $3::jsonb LIMIT 1")
        .bind(connection.to_string()).bind(user_id).bind(json!([resource])).fetch_optional(&state.console_db()?).await.map_err(|_| unavailable())?.ok_or_else(denied)?;
    let scopes: Value = row.try_get("scopes").map_err(|_| unavailable())?;
    if !claims.scope.split_whitespace().all(|s| {
        scopes
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(s)))
    }) {
        return Err(denied());
    }
    let principals: Vec<i64> = row.try_get("principals").map_err(|_| unavailable())?;
    let principal = (principals.len() == 1).then(|| centaur_console_db::oid("prn", principals[0]));
    Ok(Human {
        user_id,
        user: claims.sub.clone(),
        principal,
        claims,
    })
}
impl Human {
    pub fn prefix(&self) -> String {
        format!("personal:{}:", self.user)
    }
    pub fn principal(&self) -> Result<&str, ApiError> {
        self.principal.as_deref().ok_or_else(||ApiError::Forbidden("a unique personal Console principal is required; prepare your personal identity before creating a chat".into()))
    }
    pub fn personal_identity(&self) -> Result<centaur_console_db::PersonalMcpIdentity, ApiError> {
        Ok(centaur_console_db::PersonalMcpIdentity {
            user_id: self.user_id,
            principal_id: centaur_console_db::decode_oid("prn", self.principal()?)
                .ok_or_else(denied)?,
            active: true,
        })
    }
}

pub(crate) async fn authorize(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let result = async {
        let raw = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(denied)?
            .to_owned();
        let human = authenticate(&state, &raw).await?;
        let path = req.uri().path();
        let scope = if path.ends_with("/me") {
            "cli:identity"
        } else if path.contains("/mcp/") {
            "cli:integrations"
        } else if req.method() == Method::GET {
            "cli:sessions:read"
        } else {
            "cli:sessions:write"
        };
        if !human.claims.scope.split_whitespace().any(|s| s == scope) {
            return Err(ApiError::Forbidden("human CLI scope required".into()));
        }
        if let Some(rest) = path.strip_prefix("/api/cli/v1/session/") {
            let first = rest.split('/').next().unwrap_or_default();
            let key = urlencoding::decode(first).map_err(|_| denied())?;
            if !key.starts_with(&human.prefix()) {
                return Err(ApiError::NotFound("session not found".into()));
            }
            let parsed = centaur_session_core::ThreadKey::parse(key.into_owned())?;
            match state.runtime()?.session(&parsed).await {
                Ok(session)
                    if session.iron_control_principal.as_deref() != human.principal.as_deref() =>
                {
                    return Err(ApiError::NotFound("session not found".into()));
                }
                Ok(_) => {}
                Err(centaur_session_runtime::SessionRuntimeError::Store(
                    centaur_session_sqlx::SessionStoreError::NotFound { .. },
                )) if req.method() == Method::POST && !rest.contains('/') => {
                    human.principal()?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        req.extensions_mut().insert(human);
        req.extensions_mut().insert(raw);
        Ok::<_, ApiError>(())
    }
    .await;
    match result {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

pub(crate) async fn access_ended(state: AppState, raw: String, principal: Option<String>) {
    loop {
        let Ok(human) = authenticate(&state, &raw).await else {
            return;
        };
        if human.principal != principal {
            return;
        }
        let remaining = human.claims.exp - time::OffsetDateTime::now_utc().unix_timestamp();
        if remaining <= 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs((remaining as u64).min(30))).await;
    }
}
