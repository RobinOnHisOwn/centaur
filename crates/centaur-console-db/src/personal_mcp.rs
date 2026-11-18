//! Read-only API-client to personal Console identity binding.
use sqlx::{PgPool, Row};

use crate::Result;

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
