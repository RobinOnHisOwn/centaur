//! Read-only API-client to personal Console identity binding.
use sqlx::{PgPool, Row};

use crate::{Result, decode_oid};

/// A signed token is not sufficient: its connection, account, principal and
/// consent must still be live. Never bind identities through email claims.
pub async fn mcp_connection_identity(
    pool: &PgPool,
    connection_id: &str,
    user_oid: &str,
    principal_oid: &str,
    client_oid: &str,
    resource: &str,
    scopes: &[String],
) -> Result<Option<PersonalMcpIdentity>> {
    let (Some(user), Some(principal), Some(client), Ok(connection)) = (
        decode_oid("usr", user_oid),
        decode_oid("prn", principal_oid),
        decode_oid("moc", client_oid),
        uuid::Uuid::parse_str(connection_id),
    ) else {
        return Ok(None);
    };
    let active: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
          SELECT 1 FROM mcp_oauth_refresh_tokens t
          JOIN users u ON u.id = t.user_id AND u.status = 'active'
          JOIN principals p ON p.id = t.principal_id
          JOIN mcp_oauth_clients c ON c.id = t.mcp_oauth_client_id
          WHERE t.connection_id = $1::text::uuid AND t.user_id = $2
            AND t.principal_id = $3 AND t.mcp_oauth_client_id = $4
            AND t.resource = $5 AND t.scopes @> $6::jsonb
            AND c.scopes @> $6::jsonb
            AND (NOT $7 OR (p.kind = 'console_user' AND p.console_user_id = u.id))
            AND t.revoked_at IS NULL AND t.expires_at > now()
        )
    "#,
    )
    .bind(connection.to_string())
    .bind(user)
    .bind(principal)
    .bind(client)
    .bind(resource)
    .bind(serde_json::json!(scopes))
    .bind(
        scopes
            .iter()
            .any(|scope| scope.starts_with("mcp:sessions:")),
    )
    .fetch_one(pool)
    .await?;
    Ok(active.then_some(PersonalMcpIdentity {
        user_id: user,
        principal_id: principal,
        active,
    }))
}

#[derive(Clone, Debug)]
pub struct PersonalMcpIdentity {
    pub user_id: i64,
    pub principal_id: i64,
    pub active: bool,
}

/// Creator ownership alone is insufficient: the client's selected default must
/// also be an explicitly attached personal principal of that same active user.
pub async fn personal_mcp_identity(
    pool: &PgPool,
    client_id: i64,
) -> Result<Option<PersonalMcpIdentity>> {
    let row = sqlx::query(r#"
        SELECT u.id AS user_id, p.id AS principal_id, u.status = 'active' AS active
        FROM api_clients c
        JOIN users u ON u.id = c.created_by_id
        JOIN api_client_principals l ON l.api_client_id = c.id
        JOIN principals p ON p.id = l.principal_id AND p.kind = 'console_user' AND p.console_user_id = u.id
        WHERE c.id = $1 AND c.disabled_at IS NULL
          AND (l."default" OR (SELECT count(*) FROM api_client_principals a WHERE a.api_client_id = c.id) = 1)
    "#).bind(client_id).fetch_optional(pool).await?;
    row.map(|row| {
        Ok(PersonalMcpIdentity {
            user_id: row.try_get("user_id")?,
            principal_id: row.try_get("principal_id")?,
            active: row.try_get("active")?,
        })
    })
    .transpose()
}
