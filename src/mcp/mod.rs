#![allow(unused_imports)]

use anyhow::{anyhow, bail, Context, Result};
use console::style;
use dialoguer::{theme::ColorfulTheme, Input, MultiSelect, Select};
use rmcp::{
    model::{CallToolRequestParams, Tool},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use std::path::Path;

mod calendar;
mod server;
mod tools;
pub(crate) use calendar::load_mcp_env_file;
pub(crate) use server::{run_mcp_server, serve_mcp_listener};
pub(crate) use tools::*;

pub(crate) const MCP_SERVER_ADDR: &str = "127.0.0.1:8000";
pub(crate) const MCP_SERVER_URL: &str = "http://127.0.0.1:8000/mcp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpToolInfo {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) read_only: bool,
    pub(crate) destructive: bool,
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
    let session = McpSession::connect(url).await?;
    let tools = session.tools.clone();
    session.close().await;
    Ok(tools)
}

pub(crate) struct McpSession {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    pub(crate) tools: Vec<McpToolInfo>,
}

pub(crate) struct McpRuntime {
    session: McpSession,
    definitions: Vec<ToolDefinition>,
}

impl McpRuntime {
    pub(crate) async fn connect(url: &str, enabled: &[String]) -> Result<Self> {
        let session = McpSession::connect(url).await?;
        let definitions = enabled_tool_definitions(&session.tools, enabled);
        Ok(Self {
            session,
            definitions,
        })
    }
}

impl ToolExecutor for McpRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    fn execute<'a>(&'a self, call: &'a ToolCall) -> ToolFuture<'a> {
        Box::pin(async move {
            if !self
                .definitions
                .iter()
                .any(|definition| definition.name == call.name)
            {
                bail!("MCP-инструмент «{}» не разрешён", call.name);
            }
            self.session.call(call).await
        })
    }
}

impl McpSession {
    pub(crate) async fn connect(url: &str) -> Result<Self> {
        let url = validate_mcp_url(url)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(url),
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
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
                    read_only: tool
                        .annotations
                        .as_ref()
                        .and_then(|annotations| annotations.read_only_hint)
                        .unwrap_or(false),
                    destructive: tool
                        .annotations
                        .as_ref()
                        .and_then(|annotations| annotations.destructive_hint)
                        .unwrap_or(false),
                })
                .collect();
            Ok(Self { client, tools })
        })
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-сервера"))?
    }

    pub(crate) async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let arguments = call
            .arguments
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow!("аргументы MCP-инструмента должны быть JSON-объектом"))?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.client
                .call_tool(CallToolRequestParams::new(call.name.clone()).with_arguments(arguments)),
        )
        .await
        .map_err(|_| anyhow!("истекло время ожидания MCP-инструмента {}", call.name))?
        .with_context(|| format!("MCP-инструмент {} завершился ошибкой", call.name))?;
        let content = result
            .structured_content
            .unwrap_or_else(|| serde_json::to_value(&result.content).unwrap_or_default());
        Ok(ToolResult {
            call_id: call.id.clone(),
            content,
            is_error: result.is_error.unwrap_or(false),
        })
    }

    pub(crate) async fn close(self) {
        let _ = self.client.cancel().await;
    }
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

pub(crate) fn enabled_tool_definitions(
    available: &[McpToolInfo],
    enabled: &[String],
) -> Vec<ToolDefinition> {
    let enabled = enabled.iter().collect::<std::collections::BTreeSet<_>>();
    available
        .iter()
        .filter(|tool| enabled.contains(&tool.name))
        .map(|tool| ToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            read_only: tool.read_only,
            destructive: tool.destructive,
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::server::*;
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;

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
            input_schema: json!({}),
            read_only: true,
            destructive: false,
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
                input_schema: json!({}),
                read_only: true,
                destructive: false,
            },
            McpToolInfo {
                name: "echo".into(),
                description: None,
                input_schema: json!({}),
                read_only: true,
                destructive: false,
            },
        ];
        assert_eq!(
            selected_tool_names(&tools, &[0, 1, 0]),
            vec!["echo", "zeta"]
        );
    }

    #[test]
    fn enabled_tool_definitions_preserve_schema_and_annotations() {
        let tools = vec![
            McpToolInfo {
                name: "write".into(),
                description: Some("Writes data".into()),
                input_schema: json!({"type": "object", "required": ["title"]}),
                read_only: false,
                destructive: false,
            },
            McpToolInfo {
                name: "hidden".into(),
                description: None,
                input_schema: json!({"type": "object"}),
                read_only: true,
                destructive: false,
            },
        ];
        let selected = enabled_tool_definitions(&tools, &["write".into(), "stale".into()]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "write");
        assert_eq!(selected[0].input_schema["required"][0], "title");
        assert!(!selected[0].read_only);
    }

    #[tokio::test]
    async fn demo_server_supports_handshake_tool_listing_and_echo() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_mcp_listener(listener));
        let url = format!("http://{address}/mcp");

        let tools = fetch_tools(&url).await.unwrap();
        assert_eq!(tools.len(), 2);
        let echo = tools.iter().find(|tool| tool.name == "echo").unwrap();
        assert!(echo
            .description
            .as_deref()
            .is_some_and(|value| value.contains("Возвращает")));
        let calendar = tools
            .iter()
            .find(|tool| tool.name == "create_calendar_event")
            .unwrap();
        assert!(!calendar.read_only);
        assert_eq!(
            calendar.input_schema["required"],
            json!(["title", "start_at", "end_at"])
        );

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
