use std::collections::BTreeSet;

const STOP_WORDS: &[&str] = &[
    "and",
    "are",
    "for",
    "from",
    "how",
    "the",
    "what",
    "when",
    "where",
    "with",
    "без",
    "для",
    "как",
    "или",
    "это",
    "что",
    "где",
    "когда",
    "при",
    "его",
    "она",
    "они",
];

pub(crate) fn significant_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| {
            !(character.is_alphanumeric() || character == '_' || character == '-')
        })
        .filter_map(|raw| {
            let token = raw.to_lowercase();
            if token.is_empty() || STOP_WORDS.contains(&token.as_str()) {
                return None;
            }
            let exact = token.chars().any(|character| character.is_numeric())
                || token.contains('_')
                || token.contains('-');
            (exact || token.chars().count() >= 3).then_some(token)
        })
        .collect()
}

pub(crate) fn coverage(query_tokens: &BTreeSet<String>, target: &str) -> f32 {
    if query_tokens.is_empty() {
        return 0.0;
    }
    let target = significant_tokens(target);
    query_tokens
        .iter()
        .filter(|token| target.contains(*token))
        .count() as f32
        / query_tokens.len() as f32
}

pub(crate) fn rerank_score(semantic: f32, content: f32, metadata: f32) -> f32 {
    0.70 * semantic + 0.20 * content + 0.10 * metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokenization_is_unicode_aware_and_preserves_exact_identifiers() {
        assert_eq!(
            significant_tokens("И ЁЖИК ёжик API_v2 x 42 rust-2021 and"),
            BTreeSet::from([
                "42".to_owned(),
                "api_v2".to_owned(),
                "rust-2021".to_owned(),
                "ёжик".to_owned()
            ])
        );
    }
    #[test]
    fn lexical_coverage_can_reorder_close_semantic_results() {
        let query = significant_tokens("API_v2 timeout");
        assert!(
            rerank_score(0.80, coverage(&query, "API_v2 timeout"), 0.0)
                > rerank_score(0.82, coverage(&query, "общая настройка"), 0.0)
        );
    }
}
