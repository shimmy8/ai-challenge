use crate::rag::{Chunk, EmbeddingDescriptor};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StrategyMetrics {
    pub(crate) documents: usize,
    pub(crate) chunks: usize,
    pub(crate) min_chars: usize,
    pub(crate) max_chars: usize,
    pub(crate) mean_chars: f64,
    pub(crate) median_chars: f64,
    pub(crate) section_coverage: f64,
    pub(crate) new_embeddings: usize,
    pub(crate) reused_embeddings: usize,
    pub(crate) api_tokens: u64,
    pub(crate) duration_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ComparisonReport {
    pub(crate) provider: String,
    pub(crate) endpoint_origin: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
    pub(crate) api_tokens: u64,
    pub(crate) strategies: BTreeMap<String, StrategyMetrics>,
}

pub(crate) fn strategy_metrics(
    document_count: usize,
    chunks: &[Chunk],
    new_embeddings: usize,
    reused_embeddings: usize,
    api_tokens: u64,
    duration: Duration,
) -> StrategyMetrics {
    let mut sizes = chunks
        .iter()
        .map(|chunk| chunk.char_len)
        .collect::<Vec<_>>();
    sizes.sort_unstable();
    let min_chars = sizes.first().copied().unwrap_or(0);
    let max_chars = sizes.last().copied().unwrap_or(0);
    let mean_chars = if sizes.is_empty() {
        0.0
    } else {
        sizes.iter().sum::<usize>() as f64 / sizes.len() as f64
    };
    let median_chars = median(&sizes);
    let section_coverage = if chunks.is_empty() {
        0.0
    } else {
        chunks
            .iter()
            .filter(|chunk| chunk.section != "document")
            .count() as f64
            / chunks.len() as f64
    };
    StrategyMetrics {
        documents: document_count,
        chunks: chunks.len(),
        min_chars,
        max_chars,
        mean_chars,
        median_chars,
        section_coverage,
        new_embeddings,
        reused_embeddings,
        api_tokens,
        duration_ms: duration.as_millis(),
    }
}

fn median(sorted: &[usize]) -> f64 {
    match sorted.len() {
        0 => 0.0,
        len if len % 2 == 1 => sorted[len / 2] as f64,
        len => (sorted[len / 2 - 1] as f64 + sorted[len / 2] as f64) / 2.0,
    }
}

pub(crate) fn comparison_report(
    descriptor: &EmbeddingDescriptor,
    strategies: BTreeMap<String, StrategyMetrics>,
) -> ComparisonReport {
    ComparisonReport {
        provider: descriptor.provider.clone(),
        endpoint_origin: descriptor.endpoint_origin.clone(),
        model: descriptor.model.clone(),
        dimensions: descriptor.dimensions,
        api_tokens: strategies.values().map(|metrics| metrics.api_tokens).sum(),
        strategies,
    }
}

pub(crate) fn print_comparison(report: &ComparisonReport) {
    println!(
        "Embedding provider: {} · model: {} · dimensions: {}",
        report.provider, report.model, report.dimensions
    );
    println!(
        "{:<12} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10}",
        "Стратегия", "Файлы", "Чанки", "Среднее", "Медиана", "API tokens", "Время, мс"
    );
    for (strategy, metrics) in &report.strategies {
        println!(
            "{strategy:<12} {:>9} {:>9} {:>9.1} {:>9.1} {:>9} {:>10}",
            metrics.documents,
            metrics.chunks,
            metrics.mean_chars,
            metrics.median_chars,
            metrics.api_tokens,
            metrics.duration_ms
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::IndexStrategy;

    fn chunk(size: usize, section: &str) -> Chunk {
        Chunk {
            chunk_id: size.to_string(),
            strategy: IndexStrategy::Fixed,
            source: "a.md".to_owned(),
            title: "A".to_owned(),
            section: section.to_owned(),
            ordinal: 0,
            content: "x".repeat(size),
            content_hash: size.to_string(),
            char_len: size,
        }
    }

    #[test]
    fn metrics_calculate_even_and_odd_medians() {
        let even = strategy_metrics(
            1,
            &[chunk(1, "document"), chunk(3, "Heading")],
            2,
            0,
            10,
            Duration::from_millis(5),
        );
        assert_eq!(even.median_chars, 2.0);
        assert_eq!(even.section_coverage, 0.5);
        let odd = strategy_metrics(
            1,
            &[chunk(1, "A"), chunk(3, "B"), chunk(9, "C")],
            0,
            3,
            0,
            Duration::ZERO,
        );
        assert_eq!(odd.median_chars, 3.0);
        assert_eq!(odd.api_tokens, 0);
    }

    #[test]
    fn comparison_json_is_symmetric_and_sanitized() {
        let metrics =
            strategy_metrics(1, &[chunk(4, "Heading")], 1, 0, 7, Duration::from_millis(2));
        let report = ComparisonReport {
            provider: "fake".to_owned(),
            endpoint_origin: "http://localhost:1234".to_owned(),
            model: "model".to_owned(),
            dimensions: 3,
            api_tokens: 14,
            strategies: BTreeMap::from([
                ("fixed".to_owned(), metrics.clone()),
                ("structural".to_owned(), metrics),
            ]),
        };
        let value = serde_json::to_value(report).unwrap();
        let fixed = value["strategies"]["fixed"].as_object().unwrap();
        let structural = value["strategies"]["structural"].as_object().unwrap();
        assert_eq!(
            fixed.keys().collect::<Vec<_>>(),
            structural.keys().collect::<Vec<_>>()
        );
        let serialized = value.to_string();
        assert!(!serialized.contains("vector"));
        assert!(!serialized.contains("content"));
        assert!(!serialized.contains("api_key"));
    }
}
