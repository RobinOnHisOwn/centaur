//! Non-secret native Console integration snapshots for sandbox startup.
use serde::{Deserialize, Serialize};

pub const MCP_SNAPSHOT_KEY: &str = "personal_mcp_servers";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub name: String,
    pub url: String,
}

/// Console's existing OAuth app slug format, not a second naming scheme.
pub fn valid_mcp_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

impl McpServer {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_mcp_name(&self.name) {
            return Err("invalid Console integration name".into());
        }
        let url =
            url::Url::parse(&self.url).map_err(|_| "MCP endpoint must be an absolute HTTPS URL")?;
        if self.url.chars().any(char::is_control)
            || url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "MCP endpoint requires HTTPS without credentials, queries, fragments or control characters"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_mcp_metadata_is_non_secret_and_uses_console_names() {
        let server = McpServer {
            name: "docs.personal~1".into(),
            url: "https://mcp.example.com/mcp".into(),
        };
        assert!(server.validate().is_ok());
        for url in [
            "http://example.com",
            "https://token@example.com",
            "https://example.com/#token",
            "https://example.com/?key=synthetic",
            "https://example.com/\nspoof",
        ] {
            assert!(
                McpServer {
                    url: url.into(),
                    ..server.clone()
                }
                .validate()
                .is_err()
            );
        }
        assert!(!valid_mcp_name("../other/user"));
        assert!(
            serde_json::from_value::<McpServer>(
                serde_json::json!({"name":"docs","url":server.url,"access_token":"synthetic"})
            )
            .is_err()
        );
    }
}
