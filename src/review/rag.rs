use anyhow::{Context, Result};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{cmp::Ordering, collections::BTreeSet, fs, future::Future, path::Path, pin::Pin};

const EMBEDDING_BATCH_SIZE: usize = 64;
const MIN_REVIEW_SIMILARITY: f32 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewChunk {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) section: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) content: String,
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
        if denied_name(&name) {
            continue;
        }
        collect(root, &entry.path(), paths)?;
    }
    Ok(())
}

fn denied_name(name: &str) -> bool {
    name == ".git"
        || matches!(
            name,
            "target" | "vendor" | "node_modules" | "dist" | "generated" | ".ai-review-artifact"
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
    Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send + 'a>>;

pub(crate) trait ReviewEmbeddingProvider: Send + Sync {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> ReviewEmbeddingFuture<'a>;
}

pub(crate) struct OpenAiReviewEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiReviewEmbeddingProvider {
    pub(crate) fn new(client: Client, api_key: String, model: String) -> Self {
        Self {
            client,
            api_key,
            model,
        }
    }
}

impl ReviewEmbeddingProvider for OpenAiReviewEmbeddingProvider {
    fn embed<'a>(&'a self, inputs: &'a [String]) -> ReviewEmbeddingFuture<'a> {
        Box::pin(async move {
            if inputs.is_empty() {
                return Ok(Vec::new());
            }
            let response = self
                .client
                .post("https://api.openai.com/v1/embeddings")
                .bearer_auth(&self.api_key)
                .json(&serde_json::json!({"model": self.model, "input": inputs, "encoding_format": "float"}))
                .send()
                .await
                .context("не удалось получить embeddings")?
                .error_for_status()
                .context("embedding API отклонил запрос")?
                .json::<EmbeddingResponse>()
                .await
                .context("embedding API вернул некорректный JSON")?;
            let mut data = response.data;
            data.sort_by_key(|datum| datum.index);
            anyhow::ensure!(
                data.len() == inputs.len(),
                "embedding API вернул неверное число vectors"
            );
            let vectors = data
                .into_iter()
                .map(|datum| datum.embedding)
                .collect::<Vec<_>>();
            let dimensions = vectors.first().map(Vec::len).unwrap_or(0);
            anyhow::ensure!(dimensions > 0, "embedding API вернул пустые vectors");
            anyhow::ensure!(
                vectors.iter().all(|vector| vector.len() == dimensions
                    && vector.iter().all(|value| value.is_finite())),
                "embedding API вернул некорректные vectors"
            );
            Ok(vectors)
        })
    }
}

pub(crate) struct ReviewRagIndex {
    connection: Connection,
}

impl ReviewRagIndex {
    pub(crate) async fn build(
        provider: &dyn ReviewEmbeddingProvider,
        chunks: &[ReviewChunk],
    ) -> Result<Self> {
        let mut connection =
            Connection::open_in_memory().context("не удалось создать временный RAG-индекс")?;
        connection.execute_batch(
            "CREATE TABLE review_chunks (
                id TEXT PRIMARY KEY, source TEXT NOT NULL, section TEXT NOT NULL,
                start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                content TEXT NOT NULL, vector_json TEXT NOT NULL
            );",
        )?;
        let transaction = connection.transaction()?;
        for batch in chunks.chunks(EMBEDDING_BATCH_SIZE) {
            let inputs = batch
                .iter()
                .map(|chunk| chunk.content.clone())
                .collect::<Vec<_>>();
            let vectors = provider.embed(&inputs).await?;
            anyhow::ensure!(
                vectors.len() == batch.len(),
                "embedding provider вернул неверное число vectors"
            );
            for (chunk, vector) in batch.iter().zip(vectors) {
                transaction.execute(
                    "INSERT INTO review_chunks (id, source, section, start_line, end_line, content, vector_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![chunk.id, chunk.source, chunk.section, chunk.start_line as i64, chunk.end_line as i64, chunk.content, serde_json::to_string(&vector)?],
                )?;
            }
        }
        transaction.commit()?;
        Ok(Self { connection })
    }

    pub(crate) async fn retrieve(
        &self,
        provider: &dyn ReviewEmbeddingProvider,
        query: &str,
        budget: usize,
    ) -> Result<Vec<ReviewChunk>> {
        if self
            .connection
            .query_row("SELECT COUNT(*) FROM review_chunks", [], |row| {
                row.get::<_, usize>(0)
            })?
            == 0
        {
            return Ok(Vec::new());
        }
        let vectors = provider.embed(&[query.to_owned()]).await?;
        anyhow::ensure!(
            vectors.len() == 1,
            "embedding provider вернул неверный query vector"
        );
        let query_vector = &vectors[0];
        let mut statement = self.connection.prepare(
            "SELECT id, source, section, start_line, end_line, content, vector_json FROM review_chunks ORDER BY source, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, usize>(3)?,
                row.get::<_, usize>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut scored = Vec::new();
        for row in rows {
            let (id, source, section, start_line, end_line, content, vector_json) = row?;
            let vector: Vec<f32> = serde_json::from_str(&vector_json)?;
            let score = cosine(query_vector, &vector);
            if score >= MIN_REVIEW_SIMILARITY {
                scored.push((
                    score,
                    ReviewChunk {
                        id,
                        source,
                        section,
                        start_line,
                        end_line,
                        content,
                    },
                ));
            }
        }
        scored.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.1.source.cmp(&right.1.source))
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        let mut used = 0;
        Ok(scored
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
            .collect())
    }
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return -1.0;
    }
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let left_norm = left.iter().map(|v| v * v).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|v| v * v).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        -1.0
    } else {
        dot / (left_norm * right_norm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn discovery_includes_docs_and_code_but_excludes_secrets_and_generated() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::create_dir_all(directory.path().join("target")).unwrap();
        fs::create_dir_all(directory.path().join(".ai-review-artifact")).unwrap();
        fs::write(directory.path().join("README.md"), "# Docs").unwrap();
        fs::write(directory.path().join("src/lib.rs"), "pub fn hello() {}").unwrap();
        fs::write(directory.path().join(".env"), "SECRET=yes").unwrap();
        fs::write(directory.path().join("target/generated.rs"), "bad").unwrap();
        fs::write(
            directory
                .path()
                .join(".ai-review-artifact/test-report.json"),
            "untrusted",
        )
        .unwrap();
        let chunks = discover_review_chunks(directory.path()).unwrap();
        let sources = chunks
            .iter()
            .map(|chunk| chunk.source.as_str())
            .collect::<Vec<_>>();
        assert_eq!(sources, ["README.md", "src/lib.rs"]);
        assert!(chunks
            .iter()
            .any(|chunk| chunk.section.starts_with("hello (lines ")));
        assert!(chunks
            .iter()
            .any(|chunk| chunk.section.starts_with("Docs (lines ")));
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

    struct FakeEmbeddingProvider {
        calls: Arc<Mutex<Vec<usize>>>,
    }

    impl ReviewEmbeddingProvider for FakeEmbeddingProvider {
        fn embed<'a>(&'a self, inputs: &'a [String]) -> ReviewEmbeddingFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(inputs.len());
                Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect())
            })
        }
    }

    #[tokio::test]
    async fn temporary_index_batches_embeddings_and_preserves_provenance_budget() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = FakeEmbeddingProvider {
            calls: calls.clone(),
        };
        let chunks = (0..65)
            .map(|index| ReviewChunk {
                id: format!("id-{index}"),
                source: format!("src/{index}.rs"),
                section: "symbol".into(),
                start_line: 1,
                end_line: 1,
                content: "12345".into(),
            })
            .collect::<Vec<_>>();
        let index = ReviewRagIndex::build(&provider, &chunks).await.unwrap();
        let context = index.retrieve(&provider, "query", 10).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), [64, 1, 1]);
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].id, "id-0");
        assert_eq!(context[1].source, "src/1.rs");
    }
}
