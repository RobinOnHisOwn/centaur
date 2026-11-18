use crate::{PgSessionStore, SessionStoreError};
use sqlx::types::Json;

impl PgSessionStore {
    /// Only execution snapshot data lives here. Integration configuration and
    /// ownership remain in Console's native OAuth app/credential/grant model.
    pub async fn session_mcp_metadata(
        &self,
        key: &centaur_session_core::ThreadKey,
    ) -> Result<serde_json::Value, SessionStoreError> {
        let result: Option<Json<serde_json::Value>> = sqlx::query_scalar(
            "SELECT jsonb_build_object($2::text, metadata->$2) FROM sessions WHERE thread_key = $1 AND metadata ? $2"
        ).bind(key.as_str()).bind(centaur_session_core::mcp::MCP_SNAPSHOT_KEY).fetch_optional(&self.pool).await?;
        Ok(result
            .map(|Json(value)| value)
            .unwrap_or_else(|| serde_json::json!({})))
    }
}
