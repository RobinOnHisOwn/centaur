use centaur_sandbox_core::SandboxSpec;
use centaur_session_core::{
    HarnessType,
    mcp::{MCP_SNAPSHOT_KEY, McpServer},
};
use serde::Deserialize;
use serde_json::Value;

use crate::{SessionRuntimeError, upsert_spec_env};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    principal: String,
    servers: Vec<McpServer>,
}

pub(super) fn apply(
    spec: &mut SandboxSpec,
    metadata: &Value,
    principal: Option<&str>,
    harness: &HarnessType,
) -> Result<(), SessionRuntimeError> {
    let Some(value) = metadata.get(MCP_SNAPSHOT_KEY) else {
        return Ok(());
    };
    let snapshot: Snapshot = serde_json::from_value(value.clone())
        .map_err(|_| SessionRuntimeError::BadRequest("invalid personal MCP snapshot".into()))?;
    if principal != Some(snapshot.principal.as_str()) || harness != &HarnessType::Codex {
        return Err(SessionRuntimeError::BadRequest(
            "personal MCP snapshot requires its bound personal principal and Codex harness".into(),
        ));
    }
    let base = spec
        .env
        .iter()
        .find(|entry| entry.name == "CODEX_CONFIG_OVERLAY")
        .map(|entry| entry.value.as_str())
        .unwrap_or("");
    let mut overlay: toml::Table = toml::from_str(base)
        .map_err(|_| SessionRuntimeError::BadRequest("invalid deployment Codex overlay".into()))?;
    let servers = overlay
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            SessionRuntimeError::BadRequest("deployment MCP settings must be a table".into())
        })?;
    for server in snapshot.servers {
        server.validate().map_err(SessionRuntimeError::BadRequest)?;
        let name = format!("user_{}", server.name);
        if servers.contains_key(&name) {
            return Err(SessionRuntimeError::BadRequest(
                "personal MCP name collides with deployment settings".into(),
            ));
        }
        let mut config = toml::Table::new();
        config.insert("url".into(), toml::Value::String(server.url));
        config.insert("required".into(), toml::Value::Boolean(true));
        servers.insert(name, toml::Value::Table(config));
    }
    let text = toml::to_string(&overlay)
        .map_err(|_| SessionRuntimeError::BadRequest("cannot encode MCP overlay".into()))?;
    upsert_spec_env(spec, "CODEX_CONFIG_OVERLAY", text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_sandbox_core::SandboxSpec;
    use centaur_session_core::HarnessType;
    use serde_json::json;

    #[test]
    fn personal_mcp_overlay_preserves_deployment_settings_without_credentials() {
        let metadata = json!({"personal_mcp_servers":{"principal":"prn_personal","servers":[{"name":"docs","url":"https://mcp.example.com/mcp"}]}});
        let mut spec = SandboxSpec::new("test").env("CODEX_CONFIG_OVERLAY", "model = \"existing\"\n[mcp_servers.operator]\nurl = \"https://operator.example.com/mcp\"\n");
        apply(
            &mut spec,
            &metadata,
            Some("prn_personal"),
            &HarnessType::Codex,
        )
        .unwrap();
        let overlay = spec
            .env
            .iter()
            .find(|entry| entry.name == "CODEX_CONFIG_OVERLAY")
            .unwrap();
        let value: toml::Value = toml::from_str(&overlay.value).unwrap();
        assert_eq!(value["model"].as_str(), Some("existing"));
        assert_eq!(
            value["mcp_servers"]["user_docs"]["url"].as_str(),
            Some("https://mcp.example.com/mcp")
        );
        assert!(
            value["mcp_servers"]["user_docs"]
                .get("enabled_tools")
                .is_none()
        );
        assert!(value["mcp_servers"].get("operator").is_some());
        assert!(!overlay.value.contains("token"));
        assert!(apply(&mut spec, &metadata, Some("prn_other"), &HarnessType::Codex).is_err());
        assert!(apply(&mut spec, &metadata, Some("prn_personal"), &HarnessType::Pi).is_err());
    }
}
