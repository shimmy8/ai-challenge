use anyhow::{Context, Result};
use reqwest::{header::RETRY_AFTER, Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashSet},
    error::Error,
    fmt, fs,
    future::Future,
    path::Path,
    pin::Pin,
    time::Duration,
};

pub(crate) const EMBEDDING_BATCH_SIZE: usize = 64;
const MIN_REVIEW_SIMILARITY: f32 = 0.25;
const MAX_EMBEDDING_ATTEMPTS: usize = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewChunk {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) section: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) content: String,
}

impl ReviewChunk {
    pub(crate) fn content_hash(&self) -> String {
        format!("{:x}", Sha256::digest(self.content.as_bytes()))
    }
}

pub(crate) fn discover_review_chunks(root: &Path) -> Result<Vec<ReviewChunk>> {
    let root = root
        .canonicalize()
        .context("не удалось определить корень проекта")?;
    let mut paths = BTreeSet::new();
    collect(&root, &root, &mut paths)?;
    let mut chunks = Vec::new();
    for path in paths {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("не удалось прочитать RAG source {}", path.display()))?
            .replace("\r\n", "\n");
        let source = path
            .strip_prefix(&root)
            .expect("inside root")
            .to_string_lossy()
            .replace('\\', "/");
        for (ordinal, (start_line, end_line, piece)) in split_text_with_lines(&content, 2_000)
            .into_iter()
            .enumerate()
        {
            let section = structural_section(&source, &piece)
                .map(|symbol| format!("{symbol} (lines {start_line}-{end_line})"))
                .unwrap_or_else(|| format!("lines {start_line}-{end_line}"));
            let id = format!(
                "{:x}",
                Sha256::digest(format!("{source}\0{ordinal}\0{piece}").as_bytes())
            );
            chunks.push(ReviewChunk {
                id,
                source: source.clone(),
                section,
                start_line,
                end_line,
                content: piece,
            });
        }
    }
    Ok(chunks)
}

fn collect(root: &Path, path: &Path, paths: &mut BTreeSet<std::path::PathBuf>) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("недоступен RAG path {}", path.display()))?;
    if !canonical.starts_with(root) {
        return Ok(());
    }
    if canonical.is_file() {
        if allowed_file(&canonical) {
            paths.insert(canonical);
        }
        return Ok(());
    }
    let mut entries = fs::read_dir(&canonical)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !denied_name(&name) {
            collect(root, &entry.path(), paths)?;
        }
    }
    Ok(())
}

fn denied_name(name: &str) -> bool {
    name == ".git"
        || matches!(
            name,
            "target"
                | "vendor"
                | "node_modules"
                | "dist"
                | "generated"
                | ".ai-review-artifact"
                | ".ai-review-cache"
        )
        || name.starts_with(".env")
        || matches!(name, ".fox-llm.json" | ".fox-embeddings.json")
        || name.ends_with(".db")
        || name.ends_with(".sqlite")
}

fn allowed_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if denied_name(&name) || name.contains("credential") || name.contains("secret") {
        return false;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "rs" | "toml" | "yml" | "yaml" | "json"
            )
        })
}

fn split_text_with_lines(value: &str, limit: usize) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut chunk = String::new();
    let mut start_line = 1;
    let mut line = 1;
    for character in value.chars() {
        if chunk.chars().count() == limit {
            if !chunk.trim().is_empty() {
                result.push((start_line, line, std::mem::take(&mut chunk)));
            } else {
                chunk.clear();
            }
            start_line = line;
        }
        chunk.push(character);
        if character == '\n' {
            line += 1;
        }
    }
    if !chunk.trim().is_empty() {
        result.push((start_line, line, chunk));
    }
    result
}

fn rust_section(source: &str, content: &str) -> Option<String> {
    if !source.ends_with(".rs") {
        return None;
    }
    content.lines().find_map(|line| {
        let trimmed = line.trim_start();
        [
            "pub fn ",
            "fn ",
            "pub struct ",
            "struct ",
            "pub enum ",
            "enum ",
            "impl ",
        ]
        .iter()
        .find_map(|prefix| {
            trimmed.strip_prefix(prefix).map(|tail| {
                tail.split(['(', '<', '{', ' '])
                    .next()
                    .unwrap_or("rust")
                    .to_owned()
            })
        })
    })
}

fn structural_section(source: &str, content: &str) -> Option<String> {
    if source.ends_with(".md") {
        return content.lines().find_map(|line| {
            line.trim_start()
                .strip_prefix('#')
                .map(|heading| heading.trim_start_matches('#').trim().to_owned())
                .filter(|heading| !heading.is_empty())
        });
    }
    rust_section(source, content)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddingErrorKind {
    Retryable,
    Permanent,
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetrievalFallbackReason {
    RateLimited,
    ProviderUnavailable,
    Timeout,
    TransportError,
}

#[derive(Debug)]
pub(crate) struct EmbeddingError {
    kind: EmbeddingErrorKind,
    fallback_reason: Option<RetrievalFallbackReason>,
    message: String,
}

impl EmbeddingError {
    pub(crate) fn retryable(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::Retryable,
            fallback_reason: Some(RetrievalFallbackReason::ProviderUnavailable),
            message: message.into(),
        }
    }
    fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::Retryable,
            fallback_reason: Some(RetrievalFallbackReason::RateLimited),
            message: message.into(),
        }
    }
    fn timeout(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::Retryable,
            fallback_reason: Some(RetrievalFallbackReason::Timeout),
            message: message.into(),
        }
    }
    fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::Retryable,
            fallback_reason: Some(RetrievalFallbackReason::TransportError),
            message: message.into(),
        }
    }
    pub(crate) fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::Permanent,
            fallback_reason: None,
            message: message.into(),
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: EmbeddingErrorKind::InvalidResponse,
            fallback_reason: None,
            message: message.into(),
        }
    }
    pub(crate) fn is_retryable(&self) -> bool {
        self.kind == EmbeddingErrorKind::Retryable
    }
    pub(crate) fn fallback_reason(&self) -> Option<RetrievalFallbackReason> {
        self.fallback_reason
    }
}

impl fmt::Display for EmbeddingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl Error for EmbeddingError {}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDatum>,
}
#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
    index: usize,
}

pub(crate) type ReviewEmbeddingFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<Vec<Vec<f32>>, EmbeddingError>> + Send + 'a>>;
pub(crate) trait ReviewEmbeddingProvider: Send + Sync {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> ReviewEmbeddingFuture<'a>;
}

pub(crate) struct OpenAiReviewEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl OpenAiReviewEmbeddingProvider {
    pub(crate) fn new(client: Client, api_key: String, model: String) -> Self {
        Self {
            client,
            api_key,
            model,
            endpoint: "https://api.openai.com/v1/embeddings".to_owned(),
        }
    }
    #[cfg(test)]
    fn with_endpoint(client: Client, endpoint: String) -> Self {
        Self {
            client,
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            endpoint,
        }
    }
    async fn request_once(
        &self,
        inputs: &[String],
    ) -> std::result::Result<Vec<Vec<f32>>, (EmbeddingError, Option<Duration>)> {
        let response = self.client.post(&self.endpoint).bearer_auth(&self.api_key)
            .json(&serde_json::json!({"model": self.model, "input": inputs, "encoding_format": "float"}))
            .send().await.map_err(|error| {
                let error = if error.is_timeout() {
                    EmbeddingError::timeout("embedding API timeout")
                } else {
                    EmbeddingError::transport("embedding API transport error")
                };
                (error, None)
            })?;
        let status = response.status();
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        if !status.is_success() {
            let error = if status == StatusCode::TOO_MANY_REQUESTS {
                EmbeddingError::rate_limited(format!(
                    "embedding API временно недоступен ({status})"
                ))
            } else if status.is_server_error() {
                EmbeddingError::retryable(format!("embedding API временно недоступен ({status})"))
            } else {
                EmbeddingError::permanent(format!("embedding API отклонил запрос ({status})"))
            };
            return Err((error, retry_after));
        }
        let parsed = response.json::<EmbeddingResponse>().await.map_err(|_| {
            (
                EmbeddingError::invalid("embedding API вернул некорректный JSON"),
                None,
            )
        })?;
        validate_vectors(parsed, inputs.len()).map_err(|error| (error, None))
    }
}

impl ReviewEmbeddingProvider for OpenAiReviewEmbeddingProvider {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> ReviewEmbeddingFuture<'a> {
        Box::pin(async move {
            if inputs.is_empty() {
                return Ok(Vec::new());
            }
            for attempt in 0..MAX_EMBEDDING_ATTEMPTS {
                match self.request_once(inputs).await {
                    Ok(vectors) => return Ok(vectors),
                    Err((error, retry_after))
                        if error.is_retryable() && attempt + 1 < MAX_EMBEDDING_ATTEMPTS =>
                    {
                        let delay = retry_delay(retry_after, attempt, inputs);
                        tokio::time::sleep(delay).await;
                    }
                    Err((error, _)) => return Err(error),
                }
            }
            unreachable!("retry loop always returns")
        })
    }
}

fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let value = value?.to_str().ok()?;
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(std::time::SystemTime::now())
                .ok()
        })
}

fn retry_delay(retry_after: Option<Duration>, attempt: usize, inputs: &[String]) -> Duration {
    retry_after
        .unwrap_or_else(|| backoff_with_jitter(attempt, inputs))
        .min(MAX_RETRY_DELAY)
}

fn backoff_with_jitter(attempt: usize, inputs: &[String]) -> Duration {
    let base = 1_u64 << attempt.min(3);
    let mut hasher = Sha256::new();
    hasher.update(attempt.to_le_bytes());
    if let Some(first) = inputs.first() {
        hasher.update(first.as_bytes());
    }
    let digest = hasher.finalize();
    let jitter_ms = u16::from_le_bytes([digest[0], digest[1]]) as u64 % 251;
    Duration::from_millis(base * 1_000 + jitter_ms)
}

fn validate_vectors(
    response: EmbeddingResponse,
    expected_count: usize,
) -> std::result::Result<Vec<Vec<f32>>, EmbeddingError> {
    let mut data = response.data;
    data.sort_by_key(|datum| datum.index);
    if data.len() != expected_count {
        return Err(EmbeddingError::invalid(
            "embedding API вернул неверное число vectors",
        ));
    }
    let vectors = data
        .into_iter()
        .map(|datum| datum.embedding)
        .collect::<Vec<_>>();
    let dimensions = vectors.first().map(Vec::len).unwrap_or(0);
    if dimensions == 0
        || vectors.iter().any(|vector| {
            vector.len() != dimensions || vector.iter().any(|value| !value.is_finite())
        })
    {
        return Err(EmbeddingError::invalid(
            "embedding API вернул некорректные vectors",
        ));
    }
    Ok(vectors)
}

pub(crate) struct ReviewRagIndex<'a> {
    chunks: &'a [ReviewChunk],
    vectors: Vec<Vec<f32>>,
}

impl<'a> ReviewRagIndex<'a> {
    pub(crate) fn from_vectors(chunks: &'a [ReviewChunk], vectors: Vec<Vec<f32>>) -> Result<Self> {
        anyhow::ensure!(
            chunks.len() == vectors.len(),
            "неверное число cached vectors"
        );
        let dimensions = vectors.first().map(Vec::len).unwrap_or(0);
        anyhow::ensure!(
            chunks.is_empty()
                || (dimensions > 0
                    && vectors.iter().all(|vector| vector.len() == dimensions
                        && vector.iter().all(|value| value.is_finite()))),
            "некорректные cached vectors"
        );
        Ok(Self { chunks, vectors })
    }
    pub(crate) fn retrieve(&self, query_vector: &[f32], budget: usize) -> Vec<ReviewChunk> {
        let scored = self
            .chunks
            .iter()
            .cloned()
            .zip(&self.vectors)
            .filter_map(|(chunk, vector)| {
                let score = cosine(query_vector, vector);
                (score >= MIN_REVIEW_SIMILARITY).then_some((score, chunk))
            })
            .collect::<Vec<_>>();
        select_scored(scored, budget)
    }
}

pub(crate) fn lexical_retrieve(
    chunks: &[ReviewChunk],
    query: &str,
    budget: usize,
) -> Vec<ReviewChunk> {
    let query_tokens = tokens(query);
    let scored = chunks
        .iter()
        .filter_map(|chunk| {
            let score = overlap(&query_tokens, &tokens(&chunk.source)) * 8
                + overlap(&query_tokens, &tokens(&chunk.section)) * 4
                + overlap(&query_tokens, &tokens(&chunk.content));
            (score > 0).then_some((score as f32, chunk.clone()))
        })
        .collect::<Vec<_>>();
    select_scored(scored, budget)
}

fn tokens(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| token.chars().count() >= 2)
        .map(|token| token.to_lowercase())
        .collect()
}
fn overlap(left: &HashSet<String>, right: &HashSet<String>) -> usize {
    left.intersection(right).count()
}

fn select_scored(mut scored: Vec<(f32, ReviewChunk)>, budget: usize) -> Vec<ReviewChunk> {
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.1.source.cmp(&right.1.source))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    let mut used = 0;
    scored
        .into_iter()
        .filter_map(|(_, chunk)| {
            let chars = chunk.content.chars().count();
            if used + chars > budget {
                None
            } else {
                used += chars;
                Some(chunk)
            }
        })
        .collect()
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return -1.0;
    }
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        -1.0
    } else {
        dot / (left_norm * right_norm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode as AxumStatus, routing::post, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        Arc,
    };

    #[test]
    fn discovery_includes_docs_and_code_but_excludes_secrets_and_generated() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::create_dir_all(directory.path().join("target")).unwrap();
        fs::create_dir_all(directory.path().join(".ai-review-cache")).unwrap();
        fs::write(directory.path().join("README.md"), "# Docs").unwrap();
        fs::write(directory.path().join("src/lib.rs"), "pub fn hello() {}").unwrap();
        fs::write(directory.path().join(".env"), "SECRET=yes").unwrap();
        fs::write(directory.path().join("target/generated.rs"), "bad").unwrap();
        fs::write(directory.path().join(".ai-review-cache/cache.md"), "bad").unwrap();
        let chunks = discover_review_chunks(directory.path()).unwrap();
        let sources = chunks
            .iter()
            .map(|chunk| chunk.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(sources, ["README.md", "src/lib.rs"]);
        assert_eq!(chunks, discover_review_chunks(directory.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_symlink_outside_root() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join("outside.md")).unwrap();
        assert!(discover_review_chunks(directory.path()).unwrap().is_empty());
    }

    #[test]
    fn vector_and_lexical_retrieval_are_deterministic_and_budgeted() {
        let chunks = vec![
            ReviewChunk {
                id: "b".into(),
                source: "src/cache.rs".into(),
                section: "cache".into(),
                start_line: 1,
                end_line: 1,
                content: "cache vector".into(),
            },
            ReviewChunk {
                id: "a".into(),
                source: "docs/review.md".into(),
                section: "review".into(),
                start_line: 1,
                end_line: 1,
                content: "review fallback".into(),
            },
        ];
        let index =
            ReviewRagIndex::from_vectors(&chunks, vec![vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        assert_eq!(index.retrieve(&[1.0, 0.0], 20)[0].id, "b");
        assert_eq!(lexical_retrieve(&chunks, "src cache", 20)[0].id, "b");
    }

    #[test]
    fn lexical_retrieval_prioritizes_provenance_omits_zero_score_and_tie_breaks() {
        let chunk = |id: &str, source: &str, section: &str, content: &str| ReviewChunk {
            id: id.into(),
            source: source.into(),
            section: section.into(),
            start_line: 1,
            end_line: 1,
            content: content.into(),
        };
        let chunks = vec![
            chunk("path", "src/cache.rs", "other", "unrelated"),
            chunk(
                "content",
                "src/other.rs",
                "other",
                "cache cache cache cache",
            ),
            chunk("z", "docs/z.md", "cache", "section"),
            chunk("a", "docs/a.md", "cache", "section"),
            chunk("zero", "docs/none.md", "none", "nothing relevant"),
        ];
        let found = lexical_retrieve(&chunks, "cache", 10_000);
        assert_eq!(found[0].id, "path");
        assert_eq!(found[1].id, "a");
        assert_eq!(found[2].id, "z");
        assert_eq!(found[3].id, "content");
        assert!(!found.iter().any(|chunk| chunk.id == "zero"));
        assert!(lexical_retrieve(&chunks, "cache", 5).is_empty());
    }

    #[tokio::test]
    async fn embedding_provider_retries_429_three_times_without_exposing_body() {
        async fn handler(
            State(calls): State<Arc<AtomicUsize>>,
        ) -> (AxumStatus, [(String, String); 1], &'static str) {
            calls.fetch_add(1, AtomicOrdering::SeqCst);
            (
                AxumStatus::TOO_MANY_REQUESTS,
                [("retry-after".to_owned(), "0".to_owned())],
                "sensitive response body",
            )
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/embeddings", post(handler))
            .with_state(calls.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
            Client::new(),
            format!("http://{address}/embeddings"),
        );
        let error = provider.embed(&["query".to_owned()]).await.unwrap_err();
        assert!(error.is_retryable());
        assert_eq!(
            error.fallback_reason(),
            Some(RetrievalFallbackReason::RateLimited)
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 3);
        assert!(!error.to_string().contains("sensitive"));
        assert!(!error.to_string().contains("test-key"));
    }

    #[tokio::test]
    async fn embedding_provider_classifies_permanent_and_invalid_responses() {
        for status in [
            AxumStatus::BAD_REQUEST,
            AxumStatus::UNAUTHORIZED,
            AxumStatus::FORBIDDEN,
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let handler_calls = calls.clone();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                "/embeddings",
                post(move || {
                    let calls = handler_calls.clone();
                    async move {
                        calls.fetch_add(1, AtomicOrdering::SeqCst);
                        (status, "secret response body")
                    }
                }),
            );
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
                Client::new(),
                format!("http://{address}/embeddings"),
            );
            let error = provider.embed(&["query".to_owned()]).await.unwrap_err();
            assert!(!error.is_retryable());
            assert!(!error.to_string().contains("secret"));
            assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/embeddings",
            post(|| async { (AxumStatus::OK, "not-json") }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
            Client::new(),
            format!("http://{address}/embeddings"),
        );
        let error = provider.embed(&["query".to_owned()]).await.unwrap_err();
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("некорректный JSON"));
    }

    #[tokio::test]
    async fn embedding_provider_retries_5xx_then_succeeds() {
        async fn handler(
            State(calls): State<Arc<AtomicUsize>>,
        ) -> (AxumStatus, [(String, String); 1], String) {
            let call = calls.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            if call < 3 {
                return (
                    AxumStatus::SERVICE_UNAVAILABLE,
                    [("retry-after".to_owned(), "0".to_owned())],
                    "temporary".to_owned(),
                );
            }
            (
                AxumStatus::OK,
                [("content-type".to_owned(), "application/json".to_owned())],
                r#"{"data":[{"embedding":[1.0,0.0],"index":0}]}"#.to_owned(),
            )
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/embeddings", post(handler))
            .with_state(calls.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
            Client::new(),
            format!("http://{address}/embeddings"),
        );
        assert_eq!(
            provider.embed(&["query".to_owned()]).await.unwrap(),
            [vec![1.0, 0.0]]
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 3);
    }

    #[tokio::test]
    async fn timeout_and_transport_errors_are_retryable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/embeddings",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                (AxumStatus::OK, "{}")
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
            Client::builder()
                .timeout(Duration::from_millis(1))
                .build()
                .unwrap(),
            format!("http://{address}/embeddings"),
        );
        let (error, _) = provider
            .request_once(&["query".to_owned()])
            .await
            .unwrap_err();
        assert!(error.is_retryable());
        assert_eq!(
            error.fallback_reason(),
            Some(RetrievalFallbackReason::Timeout)
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let provider = OpenAiReviewEmbeddingProvider::with_endpoint(
            Client::new(),
            format!("http://{address}/embeddings"),
        );
        let (error, _) = provider
            .request_once(&["query".to_owned()])
            .await
            .unwrap_err();
        assert!(error.is_retryable());
        assert_eq!(
            error.fallback_reason(),
            Some(RetrievalFallbackReason::TransportError)
        );
        assert!(backoff_with_jitter(20, &["query".to_owned()]) > Duration::ZERO);
        assert_eq!(
            retry_delay(Some(Duration::from_secs(100)), 0, &[]),
            MAX_RETRY_DELAY
        );
        let future =
            httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(30));
        let header = reqwest::header::HeaderValue::from_str(&future).unwrap();
        assert!(parse_retry_after(Some(&header)).is_some());
    }
}
