use super::{serve_help_workspace, McpRouter, SharedToolExecutor, ToolExecutor};
use crate::config::McpServerConfig;
use anyhow::{Context, Result};
use std::{net::Ipv4Addr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const HELP_SERVER_ID: &str = "project-help";
const HELP_MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(200);

pub(crate) struct HelpMcpRuntime {
    router: Arc<McpRouter>,
    shutdown: Option<oneshot::Sender<()>>,
    server_task: JoinHandle<Result<()>>,
    #[cfg(test)]
    url: String,
}

impl HelpMcpRuntime {
    pub(crate) async fn start(workspace: PathBuf) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .with_context(|| format!("не удалось определить workspace {}", workspace.display()))?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("не удалось открыть loopback-порт для help-only MCP")?;
        let address = listener.local_addr()?;
        let url = format!("http://{address}/mcp");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(serve_help_workspace(listener, workspace, shutdown_rx));
        let config = McpServerConfig {
            id: HELP_SERVER_ID.into(),
            url: url.clone(),
            disabled_tools: Vec::new(),
        };
        let router = Arc::new(McpRouter::connect(&[config]).await);
        anyhow::ensure!(
            router.warnings().is_empty() && !router.is_empty(),
            "help-only MCP не прошёл handshake: {}",
            router.warnings().join("; ")
        );
        let names = router
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = [
            format!("{HELP_SERVER_ID}__codegraph_explore"),
            format!("{HELP_SERVER_ID}__git_current_branch"),
        ]
        .into_iter()
        .collect();
        anyhow::ensure!(
            names == expected,
            "help-only MCP объявил неожиданные инструменты"
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
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let router = Arc::try_unwrap(self.router).ok();
        finish_help_shutdown(router, self.server_task).await
    }
}

async fn finish_help_shutdown(
    mut router: Option<McpRouter>,
    mut server_task: JoinHandle<Result<()>>,
) -> Result<()> {
    let graceful_shutdown = async {
        if let Some(router) = router.take() {
            router.close().await;
        }
        (&mut server_task)
            .await
            .context("задача help-only MCP аварийно завершилась")??;
        Ok(())
    };

    if let Ok(result) = tokio::time::timeout(HELP_MCP_SHUTDOWN_TIMEOUT, graceful_shutdown).await {
        return result;
    }

    server_task.abort();
    let _ = server_task.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_uses_ephemeral_loopback_and_shuts_down() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".codegraph")).unwrap();
        let runtime = HelpMcpRuntime::start(directory.path().to_path_buf())
            .await
            .unwrap();
        let url = runtime.url().to_owned();
        let definitions = runtime.executor().definitions();
        assert_eq!(definitions.len(), 2);
        assert!(definitions
            .iter()
            .all(|definition| definition.read_only && !definition.destructive));
        assert!(definitions
            .iter()
            .all(|definition| !definition.name.ends_with("__save_report")));
        runtime.shutdown().await.unwrap();
        assert!(super::super::McpSession::connect(&url).await.is_err());
    }

    #[tokio::test]
    async fn stalled_server_shutdown_is_aborted_after_200_milliseconds() {
        let server_task = tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        let started = std::time::Instant::now();

        finish_help_shutdown(None, server_task).await.unwrap();

        assert!(started.elapsed() >= HELP_MCP_SHUTDOWN_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
