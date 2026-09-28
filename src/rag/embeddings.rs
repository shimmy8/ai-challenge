use crate::rag::EmbeddingConfig;
use anyhow::{bail, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EmbeddingDescriptor {
    pub(crate) provider: String,
    pub(crate) endpoint_origin: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
    pub(crate) batch_size: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EmbeddingBatch {
    pub(crate) vectors: Vec<Vec<f32>>,
    pub(crate) usage_tokens: u64,
}

pub(crate) type EmbeddingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<EmbeddingBatch>> + Send + 'a>>;

pub(crate) trait EmbeddingProvider: Send + Sync {
    fn descriptor(&self) -> &EmbeddingDescriptor;
    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbeddingFuture<'a>;
}

pub(crate) fn create_embedding_provider(
    client: Client,
    config: &EmbeddingConfig,
) -> Result<Box<dyn EmbeddingProvider>> {
    config.validate()?;
    match config.provider.as_str() {
        "openai-compatible" => Ok(Box::new(OpenAiCompatibleProvider::new(client, config)?)),
        value => bail!("неизвестный embedding provider: {value}"),
    }
}

pub(crate) struct OpenAiCompatibleProvider {
    client: Client,
    endpoint: String,
    api_key: String,
    descriptor: EmbeddingDescriptor,
}

impl OpenAiCompatibleProvider {
    pub(crate) fn new(client: Client, config: &EmbeddingConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            client,
            endpoint: config.endpoint.clone(),
            api_key: config.api_key.clone(),
            descriptor: EmbeddingDescriptor {
                provider: config.provider.clone(),
                endpoint_origin: config.endpoint_origin()?,
                model: config.model.clone(),
                dimensions: config.dimensions,
                batch_size: config.batch_size,
            },
        })
    }

    fn build_request(&self, inputs: &[String]) -> Result<reqwest::Request> {
        let payload = EmbeddingRequest {
            input: inputs,
            model: &self.descriptor.model,
            encoding_format: "float",
            dimensions: self.descriptor.dimensions,
        };
        let mut request = self.client.post(&self.endpoint).json(&payload);
        if !self.api_key.trim().is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        request
            .build()
            .map_err(|_| anyhow::anyhow!("не удалось собрать embedding request"))
    }
}

impl EmbeddingProvider for OpenAiCompatibleProvider {
    fn descriptor(&self) -> &EmbeddingDescriptor {
        &self.descriptor
    }

    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbeddingFuture<'a> {
        Box::pin(async move {
            if inputs.is_empty() {
                return Ok(EmbeddingBatch {
                    vectors: Vec::new(),
                    usage_tokens: 0,
                });
            }
            let response = self
                .client
                .execute(self.build_request(inputs)?)
                .await
                .map_err(|error| safe_transport_error(&error))?;
            let status = response.status();
            if !status.is_success() {
                return Err(safe_http_error(status));
            }
            let response: EmbeddingResponse = response
                .json()
                .await
                .map_err(|_| anyhow::anyhow!("embedding endpoint вернул некорректный JSON"))?;
            validate_response(response, inputs.len(), self.descriptor.dimensions)
        })
    }
}

fn safe_http_error(status: reqwest::StatusCode) -> anyhow::Error {
    anyhow::anyhow!("embedding endpoint вернул HTTP {status}")
}

fn safe_transport_error(error: &reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("embedding endpoint не ответил до истечения timeout")
    } else if error.is_connect() {
        anyhow::anyhow!("не удалось подключиться к embedding endpoint")
    } else {
        anyhow::anyhow!("ошибка вызова embedding endpoint")
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    input: &'a [String],
    model: &'a str,
    encoding_format: &'static str,
    dimensions: usize,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
    #[serde(default)]
    usage: EmbeddingUsage,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Default, Deserialize)]
struct EmbeddingUsage {
    #[serde(default)]
    total_tokens: u64,
}

fn validate_response(
    mut response: EmbeddingResponse,
    expected_count: usize,
    expected_dimensions: usize,
) -> Result<EmbeddingBatch> {
    anyhow::ensure!(
        response.data.len() == expected_count,
        "embedding endpoint вернул неожиданное число vectors"
    );
    response.data.sort_by_key(|item| item.index);
    let mut vectors = Vec::with_capacity(expected_count);
    for (expected_index, item) in response.data.into_iter().enumerate() {
        anyhow::ensure!(
            item.index == expected_index,
            "embedding indices непоследовательны"
        );
        anyhow::ensure!(
            item.embedding.len() == expected_dimensions,
            "embedding vector имеет неожиданную размерность"
        );
        anyhow::ensure!(
            item.embedding.iter().all(|value| value.is_finite()),
            "embedding vector содержит non-finite значение"
        );
        vectors.push(item.embedding);
    }
    Ok(EmbeddingBatch {
        vectors,
        usage_tokens: response.usage.total_tokens,
    })
}

#[cfg(test)]
pub(crate) struct FakeEmbeddingProvider {
    descriptor: EmbeddingDescriptor,
    pub(crate) calls: std::sync::Mutex<Vec<Vec<String>>>,
    fail: bool,
}

#[cfg(test)]
impl FakeEmbeddingProvider {
    pub(crate) fn new(dimensions: usize, batch_size: usize) -> Self {
        Self {
            descriptor: EmbeddingDescriptor {
                provider: "fake".to_owned(),
                endpoint_origin: "local://fake".to_owned(),
                model: "fake-model".to_owned(),
                dimensions,
                batch_size,
            },
            calls: std::sync::Mutex::new(Vec::new()),
            fail: false,
        }
    }

    pub(crate) fn failing(dimensions: usize) -> Self {
        Self {
            fail: true,
            ..Self::new(dimensions, 8)
        }
    }
}

#[cfg(test)]
impl EmbeddingProvider for FakeEmbeddingProvider {
    fn descriptor(&self) -> &EmbeddingDescriptor {
        &self.descriptor
    }

    fn embed<'a>(&'a self, inputs: &'a [String]) -> EmbeddingFuture<'a> {
        self.calls.lock().unwrap().push(inputs.to_vec());
        Box::pin(async move {
            if self.fail {
                bail!("fake provider failure");
            }
            let vectors = inputs
                .iter()
                .map(|input| {
                    let seed = input.chars().count() as f32;
                    (0..self.descriptor.dimensions)
                        .map(|index| seed + index as f32)
                        .collect()
                })
                .collect();
            Ok(EmbeddingBatch {
                vectors,
                usage_tokens: inputs.len() as u64 * 10,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn response_validation_orders_vectors_and_rejects_invalid_data() {
        let valid = EmbeddingResponse {
            data: vec![
                EmbeddingData {
                    index: 1,
                    embedding: vec![2.0, 3.0],
                },
                EmbeddingData {
                    index: 0,
                    embedding: vec![0.0, 1.0],
                },
            ],
            usage: EmbeddingUsage { total_tokens: 7 },
        };
        let batch = validate_response(valid, 2, 2).unwrap();
        assert_eq!(batch.vectors[0], vec![0.0, 1.0]);
        assert_eq!(batch.usage_tokens, 7);
        let invalid = EmbeddingResponse {
            data: vec![EmbeddingData {
                index: 0,
                embedding: vec![f32::NAN],
            }],
            usage: EmbeddingUsage::default(),
        };
        assert!(validate_response(invalid, 1, 1).is_err());
    }

    #[test]
    fn adapter_uses_configured_endpoint_payload_and_bearer_key() {
        let config = EmbeddingConfig {
            provider: "openai-compatible".to_owned(),
            endpoint: "http://127.0.0.1:11434/embed".to_owned(),
            model: "local-model".to_owned(),
            dimensions: 2,
            batch_size: 3,
            api_key: "test-key".to_owned(),
        };
        let provider = OpenAiCompatibleProvider::new(Client::new(), &config).unwrap();
        let request = provider.build_request(&["hello".to_owned()]).unwrap();
        assert_eq!(request.url().as_str(), "http://127.0.0.1:11434/embed");
        assert_eq!(request.headers()["authorization"], "Bearer test-key");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["model"], "local-model");
        assert_eq!(body["dimensions"], 2);

        let without_key = EmbeddingConfig {
            api_key: String::new(),
            ..config
        };
        let provider = OpenAiCompatibleProvider::new(Client::new(), &without_key).unwrap();
        let request = provider.build_request(&["hello".to_owned()]).unwrap();
        assert!(request.headers().get("authorization").is_none());

        let provider = create_embedding_provider(Client::new(), &without_key).unwrap();
        assert_eq!(provider.descriptor().model, "local-model");
        assert_eq!(provider.descriptor().batch_size, 3);
    }

    #[test]
    fn http_error_does_not_expose_key_or_input() {
        let error = safe_http_error(reqwest::StatusCode::UNAUTHORIZED);
        let message = format!("{error:#}");
        assert!(!message.contains("top-secret"));
        assert!(!message.contains("private chunk"));
        assert!(!message.contains("secret input"));
    }
}
