use super::{serve_project_workspace, McpRouter, SharedToolExecutor, ToolExecutor};
use crate::config::McpServerConfig;
use anyhow::{Context, Result};
use std::{net::Ipv4Addr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const PROJECT_SERVER_ID: &str = "project";
const PROJECT_MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(200);

pub(crate) struct ProjectMcpRuntime {
    router: Arc<McpRouter>,
    shutdown: Option<oneshot::Sender<()>>,
    server_task: JoinHandle<Result<()>>,
    #[cfg(test)]
    url: String,
}

impl ProjectMcpRuntime {
    pub(crate) async fn start(workspace: PathBuf) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .with_context(|| format!("не удалось определить workspace {}", workspace.display()))?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("не удалось открыть loopback-порт для project-only MCP")?;
        let address = listener.local_addr()?;
        let url = format!("http://{address}/mcp");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(serve_project_workspace(
            listener,
            workspace.clone(),
            shutdown_rx,
        ));
        let router = Arc::new(
            McpRouter::connect(&[McpServerConfig {
                id: PROJECT_SERVER_ID.into(),
                url: url.clone(),
                disabled_tools: Vec::new(),
            }])
            .await,
        );
        anyhow::ensure!(
            router.warnings().is_empty() && !router.is_empty(),
            "project-only MCP не прошёл handshake: {}",
            router.warnings().join("; ")
        );
        let names = router
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = [
            "codegraph_explore",
            "git_diff",
            "list_files",
            "read_file",
            "search_files",
            "write_file",
        ]
        .into_iter()
        .map(|name| format!("{PROJECT_SERVER_ID}__{name}"))
        .collect();
        anyhow::ensure!(
            names == expected,
            "project-only MCP объявил неожиданные инструменты"
        );
        Ok(Self {
            router,
            shutdown: Some(shutdown_tx),
            server_task,
            #[cfg(test)]
            url,
        })
    }

    pub(crate) fn executor(&self) -> SharedToolExecutor {
        self.router.clone()
    }

    #[cfg(test)]
    fn url(&self) -> &str {
        &self.url
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let router = Arc::try_unwrap(self.router).ok();
        finish_project_shutdown(router, self.server_task).await
    }
}

async fn finish_project_shutdown(
    mut router: Option<McpRouter>,
    mut server_task: JoinHandle<Result<()>>,
) -> Result<()> {
    let graceful_shutdown = async {
        if let Some(router) = router.take() {
            router.close().await;
        }
        (&mut server_task)
            .await
            .context("задача project-only MCP аварийно завершилась")??;
        Ok(())
    };
    if let Ok(result) = tokio::time::timeout(PROJECT_MCP_SHUTDOWN_TIMEOUT, graceful_shutdown).await
    {
        return result;
    }
    server_task.abort();
    let _ = server_task.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpSession, ToolCall};

    #[tokio::test]
    async fn runtime_uses_ephemeral_loopback_calls_files_and_shuts_down() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".codegraph")).unwrap();
        std::fs::write(directory.path().join("README.md"), "demo\n").unwrap();
        let config_path = directory.path().join(".fox-llm.json");
        let runtime = ProjectMcpRuntime::start(directory.path().to_path_buf())
            .await
            .unwrap();
        let url = runtime.url().to_owned();
        let definitions = runtime.executor().definitions();
        assert_eq!(definitions.len(), 6);
        assert_eq!(
            definitions
                .iter()
                .filter(|definition| !definition.read_only)
                .count(),
            1
        );
        let result = runtime
            .executor()
            .execute(&ToolCall {
                id: "read-1".into(),
                name: "project__read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            })
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(!config_path.exists());
        runtime.shutdown().await.unwrap();
        assert!(McpSession::connect(&url).await.is_err());
    }

    #[tokio::test]
    async fn stalled_project_server_shutdown_is_aborted_after_budget() {
        let server_task = tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        let started = std::time::Instant::now();
        finish_project_shutdown(None, server_task).await.unwrap();
        assert!(started.elapsed() >= PROJECT_MCP_SHUTDOWN_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
