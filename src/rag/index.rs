use crate::rag::{Chunk, ComparisonReport, Document, EmbeddingDescriptor, IndexStrategy};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashMap, fs, path::Path};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoredEmbedding {
    pub(crate) content_hash: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
    pub(crate) vector: Vec<f32>,
}

pub(crate) struct IndexStore {
    connection: Connection,
}

impl IndexStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("не удалось открыть индекс {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS index_runs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 strategy_set TEXT NOT NULL,
                 options_json TEXT NOT NULL,
                 comparison_json TEXT NOT NULL,
                 api_tokens INTEGER NOT NULL CHECK (api_tokens >= 0)
             );
             CREATE TABLE IF NOT EXISTS documents (
                 source TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 document_hash TEXT NOT NULL,
                 byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
                 char_len INTEGER NOT NULL CHECK (char_len >= 0)
             );
             CREATE TABLE IF NOT EXISTS embeddings (
                 content_hash TEXT NOT NULL,
                 model TEXT NOT NULL,
                 dimensions INTEGER NOT NULL CHECK (dimensions > 0),
                 vector_blob BLOB NOT NULL,
                 PRIMARY KEY (content_hash, model, dimensions)
             );
             CREATE TABLE IF NOT EXISTS chunks (
                 chunk_id TEXT PRIMARY KEY,
                 strategy TEXT NOT NULL CHECK (strategy IN ('fixed', 'structural')),
                 source TEXT NOT NULL REFERENCES documents(source),
                 ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                 title TEXT NOT NULL,
                 section TEXT NOT NULL,
                 content TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 model TEXT NOT NULL,
                 dimensions INTEGER NOT NULL,
                 run_id INTEGER NOT NULL REFERENCES index_runs(id),
                 FOREIGN KEY (content_hash, model, dimensions)
                     REFERENCES embeddings(content_hash, model, dimensions)
             );",
        )?;
        Ok(Self { connection })
    }

    pub(crate) fn load_cached(
        &self,
        hashes: impl IntoIterator<Item = String>,
        descriptor: &EmbeddingDescriptor,
    ) -> Result<HashMap<String, StoredEmbedding>> {
        let mut result = HashMap::new();
        let mut statement = self.connection.prepare(
            "SELECT vector_blob FROM embeddings
             WHERE content_hash = ?1 AND model = ?2 AND dimensions = ?3",
        )?;
        for hash in hashes {
            let blob = statement
                .query_row(
                    params![hash, descriptor.model, descriptor.dimensions as i64],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?;
            if let Some(blob) = blob {
                result.insert(
                    hash.clone(),
                    StoredEmbedding {
                        content_hash: hash,
                        model: descriptor.model.clone(),
                        dimensions: descriptor.dimensions,
                        vector: decode_vector(&blob, descriptor.dimensions)?,
                    },
                );
            }
        }
        Ok(result)
    }

    pub(crate) fn publish(
        &mut self,
        documents: &[Document],
        chunks: &[Chunk],
        embeddings: &[StoredEmbedding],
        selected: &[IndexStrategy],
        options_json: &str,
        comparison: &ComparisonReport,
    ) -> Result<()> {
        for embedding in embeddings {
            anyhow::ensure!(
                embedding.vector.len() == embedding.dimensions,
                "размер embedding vector не совпадает с dimensions"
            );
            anyhow::ensure!(
                embedding.vector.iter().all(|value| value.is_finite()),
                "embedding vector содержит non-finite значение"
            );
        }
        let comparison_json = serde_json::to_string(comparison)?;
        let strategy_set = selected
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO index_runs (strategy_set, options_json, comparison_json, api_tokens)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                strategy_set,
                options_json,
                comparison_json,
                comparison.api_tokens as i64
            ],
        )?;
        let run_id = transaction.last_insert_rowid();
        for document in documents {
            transaction.execute(
                "INSERT INTO documents (source, title, document_hash, byte_len, char_len)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source) DO UPDATE SET
                    title = excluded.title,
                    document_hash = excluded.document_hash,
                    byte_len = excluded.byte_len,
                    char_len = excluded.char_len",
                params![
                    document.source,
                    document.title,
                    document.document_hash,
                    document.byte_len as i64,
                    document.char_len as i64
                ],
            )?;
        }
        for embedding in embeddings {
            transaction.execute(
                "INSERT INTO embeddings (content_hash, model, dimensions, vector_blob)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(content_hash, model, dimensions) DO UPDATE SET
                    vector_blob = excluded.vector_blob",
                params![
                    embedding.content_hash,
                    embedding.model,
                    embedding.dimensions as i64,
                    encode_vector(&embedding.vector)
                ],
            )?;
        }
        for strategy in selected {
            transaction.execute(
                "DELETE FROM chunks WHERE strategy = ?1",
                [strategy.to_string()],
            )?;
        }
        for chunk in chunks {
            transaction.execute(
                "INSERT INTO chunks
                 (chunk_id, strategy, source, ordinal, title, section, content,
                  content_hash, model, dimensions, run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    chunk.chunk_id,
                    chunk.strategy.to_string(),
                    chunk.source,
                    chunk.ordinal as i64,
                    chunk.title,
                    chunk.section,
                    chunk.content,
                    chunk.content_hash,
                    comparison.model,
                    comparison.dimensions as i64,
                    run_id
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn chunk_count(&self, strategy: IndexStrategy) -> Result<usize> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM chunks WHERE strategy = ?1",
            [strategy.to_string()],
            |row| row.get::<_, i64>(0),
        )? as usize)
    }

    #[cfg(test)]
    pub(crate) fn run_count(&self) -> Result<usize> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM index_runs", [], |row| {
                row.get::<_, i64>(0)
            })? as usize)
    }
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode_vector(blob: &[u8], dimensions: usize) -> Result<Vec<f32>> {
    anyhow::ensure!(
        blob.len() == dimensions * std::mem::size_of::<f32>(),
        "повреждён embedding BLOB: длина не совпадает с dimensions"
    );
    let (values, remainder) = blob.as_chunks::<4>();
    anyhow::ensure!(remainder.is_empty(), "повреждён embedding BLOB");
    Ok(values
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::StrategyMetrics;
    use std::collections::BTreeMap;

    fn comparison() -> ComparisonReport {
        ComparisonReport {
            provider: "fake".to_owned(),
            endpoint_origin: "local://fake".to_owned(),
            model: "fake-model".to_owned(),
            dimensions: 2,
            api_tokens: 0,
            strategies: BTreeMap::from([(
                "fixed".to_owned(),
                StrategyMetrics {
                    documents: 1,
                    chunks: 1,
                    min_chars: 4,
                    max_chars: 4,
                    mean_chars: 4.0,
                    median_chars: 4.0,
                    section_coverage: 0.0,
                    new_embeddings: 1,
                    reused_embeddings: 0,
                    api_tokens: 0,
                    duration_ms: 0,
                },
            )]),
        }
    }

    fn document() -> Document {
        Document {
            source: "doc.md".to_owned(),
            title: "Doc".to_owned(),
            content: "text".to_owned(),
            document_hash: "document-hash".to_owned(),
            byte_len: 4,
            char_len: 4,
        }
    }

    fn chunk() -> Chunk {
        Chunk {
            chunk_id: "chunk-id".to_owned(),
            strategy: IndexStrategy::Fixed,
            source: "doc.md".to_owned(),
            title: "Doc".to_owned(),
            section: "document".to_owned(),
            ordinal: 0,
            content: "text".to_owned(),
            content_hash: "content-hash".to_owned(),
            char_len: 4,
        }
    }

    fn embedding() -> StoredEmbedding {
        StoredEmbedding {
            content_hash: "content-hash".to_owned(),
            model: "fake-model".to_owned(),
            dimensions: 2,
            vector: vec![1.0, 2.0],
        }
    }

    #[test]
    fn vector_blob_round_trip_validates_dimensions() {
        let vector = vec![1.25, -2.5, 3.75];
        let blob = encode_vector(&vector);
        assert_eq!(decode_vector(&blob, 3).unwrap(), vector);
        assert!(decode_vector(&blob, 2).is_err());
    }

    #[test]
    fn index_store_creates_private_wal_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.db");
        let store = IndexStore::open(&path).unwrap();
        assert_eq!(store.run_count().unwrap(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(store
            .connection
            .execute(
                "INSERT INTO embeddings (content_hash, model, dimensions, vector_blob) VALUES ('bad', 'm', 0, X'')",
                [],
            )
            .is_err());
    }

    #[test]
    fn failed_transaction_keeps_previous_chunks_and_run_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = IndexStore::open(&directory.path().join("index.db")).unwrap();
        store
            .publish(
                &[document()],
                &[chunk()],
                &[embedding()],
                &[IndexStrategy::Fixed],
                "{}",
                &comparison(),
            )
            .unwrap();
        let duplicate = chunk();
        assert!(store
            .publish(
                &[document()],
                &[duplicate.clone(), duplicate],
                &[],
                &[IndexStrategy::Fixed],
                "{}",
                &comparison(),
            )
            .is_err());
        assert_eq!(store.run_count().unwrap(), 1);
        assert_eq!(store.chunk_count(IndexStrategy::Fixed).unwrap(), 1);
    }
}
