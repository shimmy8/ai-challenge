use anyhow::Result;
use std::path::Path;

use super::{bind_and_serve_group, run_orchestrator_server, McpServerKind};

pub(crate) async fn run_mcp_server(
    kind: McpServerKind,
    address: std::net::SocketAddr,
) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let config = config_for_server(kind, &current_dir.join(crate::CONFIG_FILE))?;

    match kind {
        McpServerKind::Orchestrator => {
            run_orchestrator_server(
                address,
                current_dir.join(crate::SCHEDULER_FILE),
                config.mcp.servers,
            )
            .await
        }
        _ => bind_and_serve_group(kind, address, &config, current_dir).await,
    }
}

fn config_for_server(kind: McpServerKind, path: &Path) -> Result<crate::Config> {
    match kind {
        McpServerKind::Ai => crate::Config::load_without_mcp_registry(path),
        McpServerKind::Orchestrator => crate::Config::load(path),
        _ => Ok(crate::Config::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn independent_servers_ignore_unrelated_application_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut value = serde_json::to_value(crate::Config::default()).unwrap();
        value["mcp"] = serde_json::json!({"enabled_tools": ["echo"]});
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(config_for_server(McpServerKind::Github, &path).is_ok());
        assert!(config_for_server(McpServerKind::Reporting, &path).is_ok());
        assert!(config_for_server(McpServerKind::Workspace, &path).is_ok());
        assert!(config_for_server(McpServerKind::Calendar, &path).is_ok());
        assert!(config_for_server(McpServerKind::Telegram, &path).is_ok());
        assert!(config_for_server(McpServerKind::Ai, &path).is_ok());
        assert!(config_for_server(McpServerKind::Orchestrator, &path).is_err());
    }
}
