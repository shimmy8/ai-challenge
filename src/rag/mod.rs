mod chunking;
mod config;
mod documents;
mod embeddings;
mod evaluation;
mod index;
mod metrics;
mod retrieval;

pub(crate) use chunking::*;
pub(crate) use config::*;
pub(crate) use documents::*;
pub(crate) use embeddings::*;
pub(crate) use evaluation::*;
pub(crate) use metrics::*;
pub(crate) use retrieval::*;

use anyhow::{Context, Result};
use reqwest::Client;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const EMBEDDING_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn run_indexing(options: IndexOptions) -> Result<()> {
    let mut progress_output = io::stderr().lock();
    progress(
        &mut progress_output,
        format_args!("[index] Запуск индексации"),
    )?;
    let root = std::env::current_dir()?.canonicalize()?;
    let config = EmbeddingConfig::load_from_root(&root)?;
    let provider = create_embedding_provider(
        Client::builder()
            .user_agent("fox-llm/0.1.0")
            .timeout(EMBEDDING_REQUEST_TIMEOUT)
            .build()?,
        &config,
    )?;
    run_indexing_with_provider(&root, options, provider.as_ref(), &mut progress_output).await?;
    Ok(())
}

async fn run_indexing_with_provider(
    root: &Path,
    options: IndexOptions,
    provider: &dyn EmbeddingProvider,
    progress_output: &mut dyn Write,
) -> Result<ComparisonReport> {
    run_indexing_with_provider_timeout(
        root,
        options,
        provider,
        progress_output,
        EMBEDDING_REQUEST_TIMEOUT,
    )
    .await
}

async fn run_indexing_with_provider_timeout(
    root: &Path,
    options: IndexOptions,
    provider: &dyn EmbeddingProvider,
    progress_output: &mut dyn Write,
    request_timeout: Duration,
) -> Result<ComparisonReport> {
    let total_started = Instant::now();
    progress(
        progress_output,
        format_args!("[index] Обнаружение Markdown-документов..."),
    )?;
    let documents = discover_documents(root, &options.sources)?;
    let words = documents
        .iter()
        .map(|document| document.content.split_whitespace().count())
        .sum::<usize>();
    progress(
        progress_output,
        format_args!(
            "[index] Найдено документов: {}, слов: {}",
            documents.len(),
            words
        ),
    )?;
    let selected = options.strategy.concrete();
    let index_path = resolve_path(root, &options.index_path);
    let mut store = index::IndexStore::open(&index_path)?;
    let descriptor = provider.descriptor().clone();

    let mut chunks_by_strategy = Vec::new();
    let mut all_hashes = Vec::new();
    for strategy in &selected {
        progress(
            progress_output,
            format_args!("[index] Chunking {strategy}..."),
        )?;
        let chunks = chunking::chunk_documents(
            &documents,
            *strategy,
            options.chunk_size,
            options.chunk_overlap,
        )?;
        let unique_count = chunks
            .iter()
            .map(|chunk| chunk.content_hash.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        progress(
            progress_output,
            format_args!(
                "[index] Chunking {strategy}: чанков {}, уникальных текстов {}",
                chunks.len(),
                unique_count
            ),
        )?;
        all_hashes.extend(chunks.iter().map(|chunk| chunk.content_hash.clone()));
        chunks_by_strategy.push((*strategy, chunks));
    }
    anyhow::ensure!(
        chunks_by_strategy
            .iter()
            .any(|(_, chunks)| !chunks.is_empty()),
        "корпус не содержит текста для индексации"
    );

    progress(
        progress_output,
        format_args!("[index] Проверка embedding-кеша..."),
    )?;
    let mut available = store.load_cached(all_hashes, &descriptor)?;
    let mut generated = Vec::new();
    let mut metrics = BTreeMap::new();
    let mut all_chunks = Vec::new();

    for (strategy, chunks) in chunks_by_strategy {
        let started = Instant::now();
        let mut unique = BTreeMap::new();
        for chunk in &chunks {
            unique
                .entry(chunk.content_hash.clone())
                .or_insert_with(|| chunk.content.clone());
        }
        let reused_embeddings = unique
            .keys()
            .filter(|hash| available.contains_key(*hash))
            .count();
        let missing = unique
            .into_iter()
            .filter(|(hash, _)| !available.contains_key(hash))
            .collect::<Vec<_>>();
        let new_embeddings = missing.len();
        progress(
            progress_output,
            format_args!(
                "[index] Кеш {strategy}: найдено {}, требуется получить {}",
                reused_embeddings, new_embeddings
            ),
        )?;
        let mut api_tokens = 0;
        let batch_count = new_embeddings.div_ceil(descriptor.batch_size);
        let mut completed_embeddings = 0;
        for (batch_index, batch) in missing.chunks(descriptor.batch_size).enumerate() {
            let batch_number = batch_index + 1;
            let inputs = batch
                .iter()
                .map(|(_, content)| content.clone())
                .collect::<Vec<_>>();
            progress(
                progress_output,
                format_args!(
                    "[index] Embeddings {strategy}: batch {batch_number}/{batch_count}, отправка {} текстов...",
                    inputs.len()
                ),
            )?;
            let batch_started = Instant::now();
            let response = tokio::time::timeout(request_timeout, provider.embed(&inputs))
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "embedding batch {batch_number}/{batch_count} превысил timeout {} с",
                        request_timeout.as_secs()
                    )
                })?
                .with_context(|| format!("ошибка embedding batch {batch_number}/{batch_count}"))?;
            anyhow::ensure!(
                response.vectors.len() == batch.len(),
                "embedding provider вернул неожиданное число vectors"
            );
            let batch_tokens = response.usage_tokens;
            api_tokens += batch_tokens;
            for ((hash, _), vector) in batch.iter().zip(response.vectors) {
                anyhow::ensure!(
                    vector.len() == descriptor.dimensions,
                    "embedding provider вернул неожиданную размерность"
                );
                anyhow::ensure!(
                    vector.iter().all(|value| value.is_finite()),
                    "embedding provider вернул non-finite значение"
                );
                let embedding = index::StoredEmbedding {
                    content_hash: hash.clone(),
                    model: descriptor.model.clone(),
                    dimensions: descriptor.dimensions,
                    vector,
                };
                available.insert(hash.clone(), embedding.clone());
                generated.push(embedding);
            }
            completed_embeddings += batch.len();
            progress(
                progress_output,
                format_args!(
                    "[index] Embeddings {strategy}: batch {batch_number}/{batch_count} готов, прогресс {completed_embeddings}/{new_embeddings}, tokens {batch_tokens}, {:.2} с",
                    batch_started.elapsed().as_secs_f64()
                ),
            )?;
        }
        let strategy_name = strategy.to_string();
        metrics.insert(
            strategy_name,
            strategy_metrics(
                documents.len(),
                &chunks,
                new_embeddings,
                reused_embeddings,
                api_tokens,
                started.elapsed(),
            ),
        );
        all_chunks.extend(chunks);
    }

    let comparison = comparison_report(&descriptor, metrics);
    let options_json = serde_json::to_string(&options)?;
    progress(
        progress_output,
        format_args!("[index] Публикация SQLite: {}...", index_path.display()),
    )?;
    store.publish(
        &documents,
        &all_chunks,
        &generated,
        &selected,
        &options_json,
        &comparison,
    )?;
    progress(
        progress_output,
        format_args!("[index] SQLite-индекс опубликован"),
    )?;
    if let Some(output) = &options.comparison_output {
        let comparison_path = resolve_path(root, output);
        progress(
            progress_output,
            format_args!("[index] Запись сравнения: {}...", comparison_path.display()),
        )?;
        write_comparison_atomic(&comparison_path, &comparison)?;
        progress(progress_output, format_args!("[index] Сравнение записано"))?;
    }
    print_comparison(&comparison);
    progress(
        progress_output,
        format_args!(
            "[index] Индексация завершена за {:.2} с",
            total_started.elapsed().as_secs_f64()
        ),
    )?;
    Ok(comparison)
}

fn progress(output: &mut dyn Write, arguments: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(output, "{arguments}").context("не удалось записать progress индексации")?;
    output
        .flush()
        .context("не удалось вывести progress индексации")
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn write_comparison_atomic(path: &Path, comparison: &ComparisonReport) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("comparison.json");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, comparison)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fragments_in_order(text: &str, fragments: &[&str]) {
        let mut offset = 0;
        for fragment in fragments {
            let relative = text[offset..]
                .find(fragment)
                .unwrap_or_else(|| panic!("в progress отсутствует ожидаемый этап: {fragment}"));
            offset += relative + fragment.len();
        }
    }

    fn write_corpus(root: &Path, text: &str) {
        fs::create_dir_all(root.join("reports")).unwrap();
        fs::write(root.join("reports/doc.md"), text).unwrap();
    }

    #[tokio::test]
    async fn pipeline_publishes_both_strategies_and_reuses_cache() {
        let directory = tempfile::tempdir().unwrap();
        write_corpus(
            directory.path(),
            "# Root\nПервый раздел с текстом.\n\n## Child\nВторой раздел с текстом.",
        );
        let provider = FakeEmbeddingProvider::new(3, 2);
        let options = IndexOptions {
            sources: vec!["reports".into()],
            index_path: "index.db".into(),
            comparison_output: Some("comparison.json".into()),
            chunk_size: 40,
            chunk_overlap: 5,
            ..IndexOptions::default()
        };
        let mut progress_output = Vec::new();
        let first = run_indexing_with_provider(
            directory.path(),
            options.clone(),
            &provider,
            &mut progress_output,
        )
        .await
        .unwrap();
        assert!(first.api_tokens > 0);
        let call_count = provider.calls.lock().unwrap().len();
        let second_log_start = progress_output.len();
        let second =
            run_indexing_with_provider(directory.path(), options, &provider, &mut progress_output)
                .await
                .unwrap();
        assert_eq!(second.api_tokens, 0);
        assert_eq!(provider.calls.lock().unwrap().len(), call_count);
        let full_log = String::from_utf8(progress_output.clone()).unwrap();
        assert!(full_log.contains("Обнаружение Markdown-документов"));
        assert!(full_log.contains("Chunking fixed"));
        assert!(full_log.contains("Chunking structural"));
        assert!(full_log.contains("batch 1/"));
        assert!(full_log.contains("Публикация SQLite"));
        assert!(full_log.contains("Запись сравнения"));
        assert!(full_log.contains("Индексация завершена"));
        assert_fragments_in_order(
            &full_log,
            &[
                "Обнаружение Markdown-документов",
                "Найдено документов",
                "Chunking fixed...",
                "Chunking fixed:",
                "Chunking structural...",
                "Chunking structural:",
                "Проверка embedding-кеша",
                "Кеш fixed:",
                "Embeddings fixed: batch 1/",
                "Публикация SQLite",
                "SQLite-индекс опубликован",
                "Запись сравнения",
                "Сравнение записано",
                "Индексация завершена",
            ],
        );
        let second_log = String::from_utf8(progress_output[second_log_start..].to_vec()).unwrap();
        assert!(second_log.contains("требуется получить 0"));
        assert!(!second_log.contains("batch "));
        let store = index::IndexStore::open(&directory.path().join("index.db")).unwrap();
        assert!(store.chunk_count(IndexStrategy::Fixed).unwrap() > 0);
        assert!(store.chunk_count(IndexStrategy::Structural).unwrap() > 0);
        assert_eq!(store.run_count().unwrap(), 2);
        let exported: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(directory.path().join("comparison.json")).unwrap(),
        )
        .unwrap();
        assert!(exported.get("strategies").is_some());
        let serialized = exported.to_string();
        assert!(!serialized.contains("Первый раздел"));
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("vector"));
    }

    #[tokio::test]
    async fn provider_failure_preserves_previous_index() {
        let directory = tempfile::tempdir().unwrap();
        write_corpus(directory.path(), "# Root\nПервый текст");
        let options = IndexOptions {
            sources: vec!["reports".into()],
            strategy: IndexStrategy::Fixed,
            index_path: "index.db".into(),
            chunk_size: 100,
            chunk_overlap: 5,
            comparison_output: None,
        };
        let mut progress_output = Vec::new();
        run_indexing_with_provider(
            directory.path(),
            options.clone(),
            &FakeEmbeddingProvider::new(3, 8),
            &mut progress_output,
        )
        .await
        .unwrap();
        fs::write(
            directory.path().join("reports/doc.md"),
            "# Root\nИзменённый текст",
        )
        .unwrap();
        assert!(run_indexing_with_provider(
            directory.path(),
            options,
            &FakeEmbeddingProvider::failing(3),
            &mut progress_output,
        )
        .await
        .is_err());
        let store = index::IndexStore::open(&directory.path().join("index.db")).unwrap();
        assert_eq!(store.run_count().unwrap(), 1);
        assert_eq!(store.chunk_count(IndexStrategy::Fixed).unwrap(), 1);
        let log = String::from_utf8(progress_output).unwrap();
        assert!(log.contains("batch 1/1"));
        assert!(!log.contains("Изменённый текст"));
        assert!(!log.contains("Authorization"));
        assert!(!log.contains("api_key"));
    }

    #[tokio::test]
    async fn publishing_one_strategy_preserves_the_other() {
        let directory = tempfile::tempdir().unwrap();
        write_corpus(directory.path(), "# Root\nText\n\n## Child\nMore text");
        let provider = FakeEmbeddingProvider::new(2, 8);
        let all = IndexOptions {
            sources: vec!["reports".into()],
            index_path: "index.db".into(),
            chunk_size: 30,
            chunk_overlap: 3,
            ..IndexOptions::default()
        };
        let mut progress_output = Vec::new();
        run_indexing_with_provider(
            directory.path(),
            all.clone(),
            &provider,
            &mut progress_output,
        )
        .await
        .unwrap();
        let fixed = IndexOptions {
            strategy: IndexStrategy::Fixed,
            ..all
        };
        run_indexing_with_provider(directory.path(), fixed, &provider, &mut progress_output)
            .await
            .unwrap();
        let store = index::IndexStore::open(&directory.path().join("index.db")).unwrap();
        assert!(store.chunk_count(IndexStrategy::Structural).unwrap() > 0);
    }

    #[tokio::test]
    async fn changing_one_document_only_embeds_its_cache_miss() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("reports")).unwrap();
        fs::write(directory.path().join("reports/a.md"), "# A\nalpha").unwrap();
        fs::write(directory.path().join("reports/b.md"), "# B\nbeta").unwrap();
        let provider = FakeEmbeddingProvider::new(2, 8);
        let options = IndexOptions {
            sources: vec!["reports".into()],
            strategy: IndexStrategy::Fixed,
            index_path: "index.db".into(),
            chunk_size: 100,
            chunk_overlap: 5,
            comparison_output: None,
        };
        let mut progress_output = Vec::new();
        run_indexing_with_provider(
            directory.path(),
            options.clone(),
            &provider,
            &mut progress_output,
        )
        .await
        .unwrap();
        fs::write(directory.path().join("reports/a.md"), "# A\nalpha changed").unwrap();
        let report =
            run_indexing_with_provider(directory.path(), options, &provider, &mut progress_output)
                .await
                .unwrap();
        let metrics = &report.strategies["fixed"];
        assert_eq!(metrics.new_embeddings, 1);
        assert_eq!(metrics.reused_embeddings, 1);
        assert_eq!(provider.calls.lock().unwrap().last().unwrap().len(), 1);
    }

    struct PendingEmbeddingProvider {
        descriptor: EmbeddingDescriptor,
    }

    impl PendingEmbeddingProvider {
        fn new() -> Self {
            Self {
                descriptor: EmbeddingDescriptor {
                    provider: "pending".to_owned(),
                    endpoint_origin: "local://pending".to_owned(),
                    model: "pending-model".to_owned(),
                    dimensions: 2,
                    batch_size: 8,
                },
            }
        }
    }

    impl EmbeddingProvider for PendingEmbeddingProvider {
        fn descriptor(&self) -> &EmbeddingDescriptor {
            &self.descriptor
        }

        fn embed<'a>(&'a self, _inputs: &'a [String]) -> EmbeddingFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn embedding_timeout_is_safe_and_does_not_publish() {
        let directory = tempfile::tempdir().unwrap();
        write_corpus(directory.path(), "# Root\nsuper secret chunk text");
        let options = IndexOptions {
            sources: vec!["reports".into()],
            strategy: IndexStrategy::Fixed,
            index_path: "index.db".into(),
            chunk_size: 100,
            chunk_overlap: 5,
            comparison_output: None,
        };
        let mut progress_output = Vec::new();
        let error = run_indexing_with_provider_timeout(
            directory.path(),
            options,
            &PendingEmbeddingProvider::new(),
            &mut progress_output,
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("timeout"));
        let log = String::from_utf8(progress_output).unwrap();
        assert!(log.contains("batch 1/1"));
        assert!(!log.contains("super secret"));
        assert!(!log.contains("content_hash"));
        let store = index::IndexStore::open(&directory.path().join("index.db")).unwrap();
        assert_eq!(store.run_count().unwrap(), 0);
    }
}
