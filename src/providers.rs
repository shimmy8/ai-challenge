#![allow(unused_imports)]
use crate::{agent::*, cli::supports_temperature_with_reasoning_none, config::*, mcp::*, model::*};
use anyhow::{anyhow, bail, Context, Result};
use console::{style, Key, Term};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input, Select};
use reqwest::{Client, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
pub(crate) async fn send_request(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Result<ApiAnswer> {
    match settings.provider {
        Provider::Openai => send_openai(client, settings, history, options).await,
        Provider::Claude => send_claude(client, settings, history, options).await,
        Provider::Ollama | Provider::OllamaRemote => {
            send_ollama(client, settings, history, options).await
        }
    }
}

pub(crate) async fn send_openai(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Result<ApiAnswer> {
    send_responses_api(client, settings, history, options).await
}

pub(crate) async fn send_ollama(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Result<ApiAnswer> {
    send_responses_api(client, settings, history, options).await
}

async fn send_responses_api(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Result<ApiAnswer> {
    let payload = build_responses_payload_with_options(settings, history, options);
    let response = build_responses_request(client, settings, &payload)?
        .send()
        .await
        .with_context(|| responses_connection_error(settings.provider))?;
    let body = read_responses_response(response, settings).await?;
    parse_responses_answer(settings, &body)
}

pub(crate) fn parse_responses_answer(settings: &AgentSettings, body: &Value) -> Result<ApiAnswer> {
    let text = extract_responses_text_optional(body)?;
    let tool_calls = extract_responses_tool_calls(body)?;
    anyhow::ensure!(
        !text.trim().is_empty() || !tool_calls.is_empty(),
        "{} не вернул текст или tool call",
        settings.provider
    );
    let input_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ApiAnswer {
        text,
        task_update: None,
        task_update_warning: None,
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
        tool_calls,
        rag_citations: Vec::new(),
        generation_requests: 1,
        repair_requests: 0,
    })
}

pub(crate) fn responses_endpoint(settings: &AgentSettings) -> Result<String> {
    match settings.provider {
        Provider::Openai => Ok("https://api.openai.com/v1/responses".into()),
        Provider::Ollama => Ok(OLLAMA_RESPONSES_URL.into()),
        Provider::OllamaRemote => {
            let endpoint = normalize_ollama_remote_endpoint(
                settings
                    .endpoint
                    .as_deref()
                    .ok_or_else(|| anyhow!("для Ollama Remote не указан endpoint"))?,
            )?;
            Ok(format!("{endpoint}/responses"))
        }
        Provider::Claude => bail!("Claude не использует Responses API"),
    }
}

pub(crate) fn build_responses_request(
    client: &Client,
    settings: &AgentSettings,
    payload: &Value,
) -> Result<reqwest::RequestBuilder> {
    let mut request = client.post(responses_endpoint(settings)?).json(payload);
    if settings.provider.requires_api_key() {
        let api_key = settings
            .api_key
            .as_deref()
            .ok_or_else(|| anyhow!("нет ключа {}", settings.provider))?;
        request = request.bearer_auth(api_key);
    }
    Ok(request)
}

pub(crate) fn responses_connection_error(provider: Provider) -> &'static str {
    match provider {
        Provider::Openai => "не удалось подключиться к OpenAI",
        Provider::Ollama => "Ollama недоступна: запустите локальный сервер Ollama",
        Provider::OllamaRemote => {
            "Ollama Remote недоступна: проверьте Raspberry Pi и настроенный endpoint"
        }
        Provider::Claude => "не удалось подключиться к Anthropic",
    }
}

async fn read_responses_response(
    response: reqwest::Response,
    settings: &AgentSettings,
) -> Result<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .context("не удалось прочитать ответ API")?;
    let body: Value = serde_json::from_str(&text).map_err(|_| {
        if settings.provider.is_ollama() {
            anyhow!(
                "{} вернула несовместимый ответ; обновите Ollama до версии с Responses API (0.13.3 или новее)",
                settings.provider
            )
        } else {
            anyhow!("API вернул не JSON: {}", truncate(&text, 300))
        }
    })?;
    ensure_responses_success(status, &body, settings)?;
    Ok(body)
}

pub(crate) fn ensure_responses_success(
    status: StatusCode,
    body: &Value,
    settings: &AgentSettings,
) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    if !settings.provider.is_ollama() {
        return ensure_success(status, body, &settings.provider.to_string());
    }
    let message = api_error_message(body);
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("model")
        && (normalized.contains("not found")
            || normalized.contains("not exist")
            || normalized.contains("pull"))
    {
        if settings.provider == Provider::Ollama {
            bail!(
                "модель Ollama '{}' не установлена; выполните `ollama pull {}` или выберите установленную модель",
                settings.model,
                settings.model
            );
        }
        bail!(
            "модель Ollama Remote '{}' не установлена на удалённом сервере; установите её на Raspberry Pi или выберите доступную модель",
            settings.model
        );
    }
    if status == StatusCode::NOT_FOUND {
        bail!(
            "{} не поддерживает Responses API; обновите Ollama до версии 0.13.3 или новее",
            settings.provider
        );
    }
    bail!(
        "{} вернула {status}: {}",
        settings.provider,
        redact_secret(message, settings.api_key.as_deref())
    )
}

fn api_error_message(body: &Value) -> &str {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .unwrap_or("неизвестная ошибка API")
}

fn redact_secret<'a>(message: &'a str, secret: Option<&str>) -> Cow<'a, str> {
    match secret.filter(|secret| !secret.is_empty() && message.contains(secret)) {
        Some(secret) => Cow::Owned(message.replace(secret, "[скрыто]")),
        None => Cow::Borrowed(message),
    }
}

#[allow(dead_code)]
pub(crate) fn build_openai_payload(settings: &AgentSettings, history: &[Message]) -> Value {
    build_responses_payload_with_options(settings, history, &RequestOptions::default())
}

#[allow(dead_code)]
pub(crate) fn build_openai_payload_with_options(
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Value {
    build_responses_payload_with_options(settings, history, options)
}

pub(crate) fn build_responses_payload_with_options(
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Value {
    let mut input = history
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect::<Vec<_>>();
    input.extend(options.tool_calls.iter().map(|call| {
        json!({
            "type": "function_call",
            "call_id": call.id,
            "name": call.name,
            "arguments": call.arguments.to_string(),
        })
    }));
    input.extend(options.tool_results.iter().map(|result| {
        json!({
            "type": "function_call_output",
            "call_id": result.call_id,
            "output": openai_tool_output(&result.content),
        })
    }));
    let mut payload = json!({
        "model": settings.model,
        "input": input,
        "temperature": settings.temperature
    });
    if !options.tools.is_empty() {
        payload["tools"] = Value::Array(
            options
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description.clone().unwrap_or_default(),
                        "parameters": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    if settings.provider.is_ollama() || supports_temperature_with_reasoning_none(&settings.model) {
        payload["reasoning"] = json!({ "effort": "none" });
    }
    if let Some(instructions) = &settings.instructions {
        payload["instructions"] = json!(instructions);
    }
    payload
}

fn openai_tool_output(content: &Value) -> String {
    content
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| content.to_string())
}

pub(crate) async fn send_claude(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Result<ApiAnswer> {
    let payload = build_claude_payload_with_options(settings, history, options);
    let api_key = settings
        .api_key
        .as_deref()
        .ok_or_else(|| anyhow!("нет ключа Claude"))?;
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к Anthropic")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "Anthropic")?;
    let text = extract_claude_text_optional(&body)?;
    let tool_calls = extract_claude_tool_calls(&body)?;
    anyhow::ensure!(
        !text.trim().is_empty() || !tool_calls.is_empty(),
        "Claude не вернул текст или tool call"
    );
    let input_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ApiAnswer {
        text,
        task_update: None,
        task_update_warning: None,
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
        tool_calls,
        rag_citations: Vec::new(),
        generation_requests: 1,
        repair_requests: 0,
    })
}

#[allow(dead_code)]
pub(crate) fn build_claude_payload(settings: &AgentSettings, history: &[Message]) -> Value {
    build_claude_payload_with_options(settings, history, &RequestOptions::default())
}

pub(crate) fn build_claude_payload_with_options(
    settings: &AgentSettings,
    history: &[Message],
    options: &RequestOptions,
) -> Value {
    let mut messages = history
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect::<Vec<_>>();
    if !options.tool_calls.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": options.tool_calls.iter().map(|call| json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.arguments,
            })).collect::<Vec<_>>(),
        }));
    }
    if !options.tool_results.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": options.tool_results.iter().map(|result| json!({
                "type": "tool_result",
                "tool_use_id": result.call_id,
                "content": result.content,
                "is_error": result.is_error,
            })).collect::<Vec<_>>(),
        }));
    }
    let mut payload = json!({
        "model": settings.model,
        "max_tokens": 4096,
        "temperature": settings.temperature,
        "messages": messages
    });
    if !options.tools.is_empty() {
        payload["tools"] = Value::Array(
            options
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description.clone().unwrap_or_default(),
                        "input_schema": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    if let Some(instructions) = &settings.instructions {
        payload["system"] = json!(instructions);
    }
    payload
}

pub(crate) async fn read_response(response: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = response.status();
    let text = response
        .text()
        .await
        .context("не удалось прочитать ответ API")?;
    let body = serde_json::from_str(&text)
        .with_context(|| format!("API вернул не JSON: {}", truncate(&text, 300)))?;
    Ok((status, body))
}

pub(crate) fn ensure_success(status: StatusCode, body: &Value, provider: &str) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let message = api_error_message(body);
    bail!("{provider} вернул {status}: {message}")
}

#[allow(dead_code)]
pub(crate) fn extract_openai_text(body: &Value) -> Result<String> {
    let text = extract_responses_text_optional(body)?;
    if text.trim().is_empty() {
        bail!("OpenAI не вернул текст");
    }
    Ok(text)
}

fn extract_responses_text_optional(body: &Value) -> Result<String> {
    let parts = body
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str));
    Ok(parts.collect::<Vec<_>>().join("\n"))
}

#[allow(dead_code)]
pub(crate) fn extract_claude_text(body: &Value) -> Result<String> {
    let text = extract_claude_text_optional(body)?;
    if text.trim().is_empty() {
        bail!("Claude не вернул текст");
    }
    Ok(text)
}

fn extract_claude_text_optional(body: &Value) -> Result<String> {
    let parts = body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str));
    Ok(parts.collect::<Vec<_>>().join("\n"))
}

#[allow(dead_code)]
pub(crate) fn extract_openai_tool_calls(body: &Value) -> Result<Vec<ToolCall>> {
    extract_responses_tool_calls(body)
}

pub(crate) fn extract_responses_tool_calls(body: &Value) -> Result<Vec<ToolCall>> {
    body.get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Responses API tool call не содержит call_id"))?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Responses API tool call не содержит name"))?;
            let raw = item
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Responses API tool call не содержит arguments"))?;
            let arguments = serde_json::from_str(raw).with_context(|| {
                format!("Responses API tool call {name} содержит неверный JSON")
            })?;
            Ok(ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments,
            })
        })
        .collect()
}

pub(crate) fn extract_claude_tool_calls(body: &Value) -> Result<Vec<ToolCall>> {
    body.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|item| {
            Ok(ToolCall {
                id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Claude tool_use не содержит id"))?
                    .to_owned(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Claude tool_use не содержит name"))?
                    .to_owned(),
                arguments: item.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect()
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}
