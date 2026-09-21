#![allow(unused_imports)]

use anyhow::{anyhow, bail, Context, Result};
use console::style;
use dialoguer::{theme::ColorfulTheme, Input, MultiSelect, Select};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolRequestParams, Tool},
    tool, tool_handler, tool_router,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{
            session::local::LocalSessionManager, tower::StreamableHttpService,
            StreamableHttpServerConfig,
        },
        StreamableHttpClientTransport,
    },
    ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::net::TcpListener;

pub(crate) const MCP_SERVER_ADDR: &str = "127.0.0.1:8000";
pub(crate) const MCP_SERVER_URL: &str = "http://127.0.0.1:8000/mcp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpToolInfo {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

pub(crate) fn validate_mcp_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("адрес MCP некорректен")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "адрес MCP должен начинаться с http:// или https://"
    );
    anyhow::ensure!(url.host_str().is_some(), "в адресе MCP не указан хост");
    Ok(url.to_string())
}

pub(crate) async fn fetch_tools(url: &str) -> Result<Vec<McpToolInfo>> {
    let url = validate_mcp_url(url)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), fetch_tools_inner(&url))
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-сервера"))?
}

async fn fetch_tools_inner(url: &str) -> Result<Vec<McpToolInfo>> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()),
    );
    let client = ().serve(transport).await.context("MCP handshake не выполнен")?;
    let result = client
        .list_all_tools()
        .await
        .context("MCP-сервер не вернул список инструментов")?;
    let tools = result
        .into_iter()
        .map(|tool| McpToolInfo {
            name: tool.name.to_string(),
            description: tool.description.map(|value| value.to_string()),
        })
        .collect();
    let _ = client.cancel().await;
    Ok(tools)
}

pub(crate) fn reconcile_enabled_tools(
    available: &[McpToolInfo],
    enabled: &[String],
) -> Vec<String> {
    let available = available
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut result = enabled
        .iter()
        .filter(|name| available.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}

pub(crate) fn switch_mcp_server(config: &mut crate::config::McpConfig, url: String) {
    config.server_url = Some(url);
    config.enabled_tools.clear();
}

pub(crate) fn selected_tool_names(tools: &[McpToolInfo], selected: &[usize]) -> Vec<String> {
    let mut names = selected
        .iter()
        .filter_map(|index| tools.get(*index))
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) async fn handle_mcp_command(
    config: &mut crate::config::Config,
    path: &Path,
) -> Result<()> {
    let mut pending_url = config.mcp.server_url.clone();
    loop {
        let Some(url) = pending_url.clone() else {
            println!("{}", style("MCP-сервер ещё не настроен.").yellow());
            let Some(candidate) = prompt_mcp_url()? else {
                println!("{}", style("Настройка MCP отменена.").dim());
                return Ok(());
            };
            match connect_and_list(&candidate).await {
                Ok((normalized, tools)) => {
                    switch_mcp_server(&mut config.mcp, normalized.clone());
                    config.save(path)?;
                    println!("{}", style("MCP-сервер подключён.").green());
                    show_mcp_menu(config, path, &tools).await?;
                    return Ok(());
                }
                Err(error) => {
                    println!("{} {error:#}", style("Не удалось подключиться:").red());
                    continue;
                }
            }
        };

        match connect_and_list(&url).await {
            Ok((_normalized, tools)) => {
                show_mcp_menu(config, path, &tools).await?;
                return Ok(());
            }
            Err(error) => {
                println!(
                    "{} {}\n{}",
                    style("MCP-сервер недоступен:").red(),
                    error,
                    style("Текущая конфигурация сохранена.").dim()
                );
                let actions = ["Повторить подключение", "Указать новый адрес", "Отмена"];
                let Some(action) = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Что сделать?")
                    .items(&actions)
                    .default(0)
                    .interact_opt()?
                else {
                    return Ok(());
                };
                match action {
                    0 => {}
                    1 => {
                        pending_url = prompt_mcp_url()?;
                    }
                    _ => return Ok(()),
                }
            }
        }
    }
}

async fn connect_and_list(url: &str) -> Result<(String, Vec<McpToolInfo>)> {
    let normalized = validate_mcp_url(url)?;
    let tools = fetch_tools(&normalized).await?;
    Ok((normalized, tools))
}

fn prompt_mcp_url() -> Result<Option<String>> {
    let value = Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("Адрес MCP-сервера (например, {MCP_SERVER_URL})"))
        .allow_empty(true)
        .interact_text()?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

async fn show_mcp_menu(
    config: &mut crate::config::Config,
    path: &Path,
    tools: &[McpToolInfo],
) -> Result<()> {
    let mut tools = tools.to_vec();
    println!(
        "{} {}",
        style("MCP-сервер:").yellow(),
        style(config.mcp.server_url.as_deref().unwrap_or("не настроен")).cyan()
    );
    loop {
        let actions = [
            "Показать и включить/выключить инструменты",
            "Изменить адрес",
            "Закрыть",
        ];
        let Some(action) = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Меню MCP")
            .items(&actions)
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        match action {
            0 => select_mcp_tools(config, path, &tools)?,
            1 => {
                let Some(candidate) = prompt_mcp_url()? else {
                    continue;
                };
                match connect_and_list(&candidate).await {
                    Ok((normalized, new_tools)) => {
                        switch_mcp_server(&mut config.mcp, normalized);
                        config.save(path)?;
                        tools = new_tools;
                        println!("{}", style("MCP-сервер изменён.").green());
                    }
                    Err(error) => println!("{} {error:#}", style("Не удалось подключиться:").red()),
                }
            }
            _ => return Ok(()),
        }
    }
}

fn select_mcp_tools(
    config: &mut crate::config::Config,
    path: &Path,
    tools: &[McpToolInfo],
) -> Result<()> {
    if tools.is_empty() {
        println!("{}", style("Сервер не объявил инструментов.").dim());
        return Ok(());
    }
    let enabled = reconcile_enabled_tools(tools, &config.mcp.enabled_tools);
    let labels = tools
        .iter()
        .map(|tool| {
            tool.description.as_deref().map_or_else(
                || tool.name.clone(),
                |description| format!("{} — {description}", tool.name),
            )
        })
        .collect::<Vec<_>>();
    let defaults = tools
        .iter()
        .map(|tool| enabled.iter().any(|name| name == &tool.name))
        .collect::<Vec<_>>();
    let Some(selected) = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Выберите включённые инструменты (пробел — переключить)")
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()?
    else {
        println!("{}", style("Выбор инструментов отменён.").dim());
        return Ok(());
    };
    config.mcp.enabled_tools = selected_tool_names(tools, &selected);
    config.save(path)?;
    println!(
        "{} {}",
        style("Включённые инструменты сохранены:").yellow(),
        if config.mcp.enabled_tools.is_empty() {
            "нет".to_owned()
        } else {
            config.mcp.enabled_tools.join(", ")
        }
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EchoRequest {
    /// Text to return unchanged.
    text: String,
}

#[derive(Debug, Clone)]
struct EchoServer {
    tool_router: ToolRouter<Self>,
}

impl EchoServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl EchoServer {
    #[tool(
        name = "echo",
        description = "Возвращает переданный текст без изменений"
    )]
    async fn echo(&self, Parameters(request): Parameters<EchoRequest>) -> String {
        request.text
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EchoServer {}

pub(crate) async fn run_mcp_server() -> Result<()> {
    let listener = TcpListener::bind(MCP_SERVER_ADDR)
        .await
        .with_context(|| format!("не удалось запустить MCP-сервер на {MCP_SERVER_ADDR}"))?;
    println!("MCP-сервер запущен: {MCP_SERVER_URL}");
    serve_mcp_listener(listener).await
}

pub(crate) async fn serve_mcp_listener(listener: TcpListener) -> Result<()> {
    let service = StreamableHttpService::new(
        || Ok(EchoServer::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router)
        .await
        .context("MCP-сервер завершился с ошибкой")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_http_urls() {
        assert_eq!(
            validate_mcp_url(" http://localhost:8000/mcp ").unwrap(),
            "http://localhost:8000/mcp"
        );
        assert!(validate_mcp_url("localhost:8000/mcp").is_err());
        assert!(validate_mcp_url("file:///tmp/mcp").is_err());
    }

    #[test]
    fn removes_duplicate_and_stale_enabled_tools() {
        let available = vec![McpToolInfo {
            name: "echo".into(),
            description: None,
        }];
        assert_eq!(
            reconcile_enabled_tools(&available, &["old".into(), "echo".into(), "echo".into()]),
            vec!["echo"]
        );
    }

    #[test]
    fn switching_server_clears_previous_selection() {
        let mut config = crate::config::McpConfig {
            server_url: Some("http://old.test/mcp".into()),
            enabled_tools: vec!["old-tool".into()],
        };
        switch_mcp_server(&mut config, "http://new.test/mcp".into());
        assert_eq!(config.server_url.as_deref(), Some("http://new.test/mcp"));
        assert!(config.enabled_tools.is_empty());
    }

    #[test]
    fn selected_tool_names_are_sorted_and_deduplicated() {
        let tools = vec![
            McpToolInfo {
                name: "zeta".into(),
                description: None,
            },
            McpToolInfo {
                name: "echo".into(),
                description: None,
            },
        ];
        assert_eq!(
            selected_tool_names(&tools, &[0, 1, 0]),
            vec!["echo", "zeta"]
        );
    }

    #[tokio::test]
    async fn demo_server_supports_handshake_tool_listing_and_echo() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_mcp_listener(listener));
        let url = format!("http://{address}/mcp");

        let tools = fetch_tools(&url).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert!(tools[0]
            .description
            .as_deref()
            .is_some_and(|value| value.contains("Возвращает")));

        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url),
        );
        let client = ().serve(transport).await.unwrap();
        let result = client
            .call_tool(
                CallToolRequestParams::new("echo")
                    .with_arguments(json!({"text": "проверка"}).as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(result.content[0].as_text().unwrap().text, "проверка");
        let _ = client.cancel().await;
        task.abort();
    }
}
