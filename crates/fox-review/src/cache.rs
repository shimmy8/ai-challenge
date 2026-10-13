use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

pub(crate) const CACHE_SCHEMA_VERSION: u32 = 1;
pub(crate) const CHUNKING_VERSION: &str = "unicode-2000-v1";
pub(crate) const EMBEDDING_ORIGIN: &str = "https://api.openai.com";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CacheNamespace {
    schema_version: u32,
    provider_origin: String,
    embedding_model: String,
    dimensions: usize,
    chunking_version: String,
}

impl CacheNamespace {
    pub(crate) fn openai(embedding_model: &str, dimensions: usize) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            provider_origin: EMBEDDING_ORIGIN.to_owned(),
            embedding_model: embedding_model.to_owned(),
            dimensions,
            chunking_version: CHUNKING_VERSION.to_owned(),
        }
    }

    pub(crate) fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[derive(Debug, Default)]
pub(crate) struct CachedVectors {
    pub(crate) vectors: BTreeMap<String, Vec<f32>>,
}

impl CachedVectors {
    pub(crate) fn load(path: &Path, expected: &CacheNamespace) -> Self {
        Self::try_load(path, expected).unwrap_or_default()
    }

    fn try_load(path: &Path, expected: &CacheNamespace) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("не удалось открыть embedding cache")?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        anyhow::ensure!(integrity == "ok", "embedding cache повреждён");
        let raw_namespace: String = connection.query_row(
            "SELECT value FROM metadata WHERE key = 'namespace'",
            [],
            |row| row.get(0),
        )?;
        let actual: CacheNamespace = serde_json::from_str(&raw_namespace)?;
        anyhow::ensure!(&actual == expected, "embedding cache несовместим");

        let mut statement = connection.prepare(
            "SELECT content_hash, dimensions, vector_json FROM embeddings ORDER BY content_hash",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, usize>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut vectors = BTreeMap::new();
        for row in rows {
            let (content_hash, dimensions, raw_vector) = row?;
            let vector: Vec<f32> = serde_json::from_str(&raw_vector)?;
            anyhow::ensure!(
                dimensions == expected.dimensions()
                    && vector.len() == expected.dimensions()
                    && vector.iter().all(|value| value.is_finite()),
                "embedding cache содержит некорректный vector"
            );
            vectors.insert(content_hash, vector);
        }
        Ok(Self { vectors })
    }

    pub(crate) fn save(
        path: &Path,
        namespace: &CacheNamespace,
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> Result<()> {
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("review-rag.db"),
            uuid::Uuid::new_v4()
        ));
        let result = (|| -> Result<()> {
            let mut connection = Connection::open(&temporary)?;
            connection.execute_batch(
                "PRAGMA journal_mode = DELETE;
                 CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE embeddings (
                    content_hash TEXT PRIMARY KEY,
                    dimensions INTEGER NOT NULL,
                    vector_json TEXT NOT NULL
                 );",
            )?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO metadata (key, value) VALUES ('namespace', ?1)",
                [serde_json::to_string(namespace)?],
            )?;
            for (content_hash, vector) in vectors {
                anyhow::ensure!(
                    vector.len() == namespace.dimensions()
                        && vector.iter().all(|value| value.is_finite()),
                    "нельзя сохранить некорректный embedding vector"
                );
                transaction.execute(
                    "INSERT INTO embeddings (content_hash, dimensions, vector_json) VALUES (?1, ?2, ?3)",
                    params![content_hash, namespace.dimensions(), serde_json::to_string(vector)?],
                )?;
            }
            transaction.commit()?;
            connection.execute_batch("PRAGMA optimize;")?;
            drop(connection);
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_round_trip_prunes_and_rejects_namespace_or_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review-rag.db");
        let namespace = CacheNamespace::openai("embed-a", 2);
        let vectors = BTreeMap::from([
            ("keep".to_owned(), vec![1.0, 0.0]),
            ("stale".to_owned(), vec![0.0, 1.0]),
        ]);
        CachedVectors::save(&path, &namespace, &vectors).unwrap();
        assert_eq!(CachedVectors::load(&path, &namespace).vectors.len(), 2);
        let bytes = fs::read(&path).unwrap();
        let database = String::from_utf8_lossy(&bytes);
        assert!(!database.contains("source code that must stay out"));
        assert!(!database.contains("api-key"));

        let pruned = BTreeMap::from([("keep".to_owned(), vec![1.0, 0.0])]);
        CachedVectors::save(&path, &namespace, &pruned).unwrap();
        assert_eq!(CachedVectors::load(&path, &namespace).vectors, pruned);
        assert!(
            CachedVectors::load(&path, &CacheNamespace::openai("embed-b", 2))
                .vectors
                .is_empty()
        );

        fs::write(&path, "not sqlite").unwrap();
        assert!(CachedVectors::load(&path, &namespace).vectors.is_empty());
    }

    #[test]
    fn invalid_cached_vector_invalidates_whole_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review-rag.db");
        let namespace = CacheNamespace::openai("embed", 2);
        CachedVectors::save(
            &path,
            &namespace,
            &BTreeMap::from([("hash".to_owned(), vec![1.0, 0.0])]),
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE embeddings SET vector_json = '[1.0]' WHERE content_hash = 'hash'",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(CachedVectors::load(&path, &namespace).vectors.is_empty());

        CachedVectors::save(
            &path,
            &namespace,
            &BTreeMap::from([("hash".to_owned(), vec![1.0, 0.0])]),
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE embeddings SET vector_json = '[1e999,0.0]' WHERE content_hash = 'hash'",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(CachedVectors::load(&path, &namespace).vectors.is_empty());
    }
}
