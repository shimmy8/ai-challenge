#![allow(unused_imports)]
use crate::{agent::*, cli::supports_temperature_with_reasoning_none, config::*, model::*};
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
) -> Result<ApiAnswer> {
    match settings.provider {
        Provider::Openai => send_openai(client, settings, history).await,
        Provider::Claude => send_claude(client, settings, history).await,
    }
}

pub(crate) async fn send_openai(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
) -> Result<ApiAnswer> {
    let payload = build_openai_payload(settings, history);
    let response = client
        .post("https://api.openai.com/v1/responses")
        .bearer_auth(&settings.api_key)
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к OpenAI")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "OpenAI")?;
    let text = extract_openai_text(&body)?;
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
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
    })
}

pub(crate) fn build_openai_payload(settings: &AgentSettings, history: &[Message]) -> Value {
    let mut payload = json!({
        "model": settings.model,
        "input": history,
        "temperature": settings.temperature
    });
    if supports_temperature_with_reasoning_none(&settings.model) {
        payload["reasoning"] = json!({ "effort": "none" });
    }
    if let Some(instructions) = &settings.instructions {
        payload["instructions"] = json!(instructions);
    }
    payload
}

pub(crate) async fn send_claude(
    client: &Client,
    settings: &AgentSettings,
    history: &[Message],
) -> Result<ApiAnswer> {
    let payload = build_claude_payload(settings, history);
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &settings.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .send()
        .await
        .context("не удалось подключиться к Anthropic")?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, "Anthropic")?;
    let text = extract_claude_text(&body)?;
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
        input_tokens,
        output_tokens,
        session_input_tokens: 0,
        session_output_tokens: 0,
    })
}

pub(crate) fn build_claude_payload(settings: &AgentSettings, history: &[Message]) -> Value {
    let mut payload = json!({
        "model": settings.model,
        "max_tokens": 4096,
        "temperature": settings.temperature,
        "messages": history
    });
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
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("неизвестная ошибка API");
    bail!("{provider} вернул {status}: {message}")
}

pub(crate) fn extract_openai_text(body: &Value) -> Result<String> {
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
    nonempty_text(parts, "OpenAI не вернул текст")
}

pub(crate) fn extract_claude_text(body: &Value) -> Result<String> {
    let parts = body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str));
    nonempty_text(parts, "Claude не вернул текст")
}

pub(crate) fn nonempty_text<'a>(
    parts: impl Iterator<Item = &'a str>,
    error: &str,
) -> Result<String> {
    let text = parts.collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() {
        bail!(error.to_owned());
    }
    Ok(text)
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
