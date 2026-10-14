use super::{ensure_success, read_response};
use crate::config::{
    normalize_ollama_remote_endpoint, Config, Provider, CLAUDE_MODELS_URL, OLLAMA_MODELS_URL,
    OPENAI_MODELS_URL,
};
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;

pub(crate) async fn fetch_models(
    client: &Client,
    config: &Config,
    provider: Provider,
) -> Result<Vec<String>> {
    let response = build_models_request(client, config, provider)?
        .send()
        .await
        .with_context(|| model_discovery_connection_error(provider))?;
    let (status, body) = read_response(response).await?;
    ensure_success(status, &body, &provider.to_string())?;
    parse_model_ids(&body, provider)
}

pub(crate) fn build_models_request(
    client: &Client,
    config: &Config,
    provider: Provider,
) -> Result<reqwest::RequestBuilder> {
    Ok(match provider {
        Provider::Openai => client.get(OPENAI_MODELS_URL).bearer_auth(
            config
                .key(provider)
                .ok_or_else(|| anyhow!("нет ключа OpenAI"))?,
        ),
        Provider::Claude => client
            .get(CLAUDE_MODELS_URL)
            .query(&[("limit", 1000)])
            .header(
                "x-api-key",
                config
                    .key(provider)
                    .ok_or_else(|| anyhow!("нет ключа Claude"))?,
            )
            .header("anthropic-version", "2023-06-01"),
        Provider::Ollama => client.get(OLLAMA_MODELS_URL),
        Provider::OllamaRemote => {
            let endpoint = normalize_ollama_remote_endpoint(
                config
                    .endpoint(Provider::OllamaRemote)
                    .ok_or_else(|| anyhow!("для Ollama Remote не указан endpoint"))?,
            )?;
            client.get(format!("{endpoint}/models"))
        }
    })
}

pub(crate) fn model_discovery_connection_error(provider: Provider) -> &'static str {
    match provider {
        Provider::Openai => "не удалось получить модели OpenAI",
        Provider::Claude => "не удалось получить модели Anthropic",
        Provider::Ollama => "Ollama недоступна: запустите локальный сервер Ollama",
        Provider::OllamaRemote => {
            "Ollama Remote недоступна: проверьте Raspberry Pi и настроенный endpoint"
        }
    }
}

pub(crate) fn empty_models_error(provider: Provider) -> anyhow::Error {
    match provider {
        Provider::Ollama => {
            anyhow!("в Ollama нет установленных моделей; выполните `ollama pull qwen3.5:4b`")
        }
        Provider::OllamaRemote => anyhow!(
            "в Ollama Remote нет установленных моделей; установите `qwen3:1.7b` на удалённом сервере"
        ),
        Provider::Openai | Provider::Claude => {
            anyhow!("{provider} не вернул ни одной совместимой модели")
        }
    }
}

pub(crate) fn parse_model_ids(body: &Value, provider: Provider) -> Result<Vec<String>> {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("API вернул список моделей в неизвестном формате"))?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .filter(|id| model_supports_responses_api(provider, id))
        .map(str::to_owned)
        .collect();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

fn model_supports_responses_api(provider: Provider, model: &str) -> bool {
    if matches!(
        provider,
        Provider::Claude | Provider::Ollama | Provider::OllamaRemote
    ) {
        return true;
    }

    let model = model.to_ascii_lowercase();
    let text_model = model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("ft:gpt-");
    let specialized_model = [
        "audio",
        "realtime",
        "transcribe",
        "tts",
        "image",
        "moderation",
        "search-preview",
        "deep-research",
    ]
    .iter()
    .any(|marker| model.contains(marker));
    // The models endpoint lists every model available to the account but does
    // not expose endpoint capabilities. Instruct models are documented as
    // legacy Completions-only and cannot be sent to /v1/responses.
    let legacy_completions_model = model.starts_with("gpt-3.5-turbo-instruct");

    text_model && !specialized_model && !legacy_completions_model
}
