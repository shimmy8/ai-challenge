use crate::rag::{documents::sha256_hex, Document, IndexStrategy};
use anyhow::{bail, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::Serialize;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Chunk {
    pub(crate) chunk_id: String,
    pub(crate) strategy: IndexStrategy,
    pub(crate) source: String,
    pub(crate) title: String,
    pub(crate) section: String,
    pub(crate) ordinal: usize,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) char_len: usize,
}

#[derive(Debug, Clone)]
struct Heading {
    level: usize,
    start: usize,
    title: String,
}

pub(crate) fn chunk_documents(
    documents: &[Document],
    strategy: IndexStrategy,
    chunk_size: usize,
    overlap: usize,
) -> Result<Vec<Chunk>> {
    anyhow::ensure!(chunk_size > 0, "размер чанка должен быть больше нуля");
    anyhow::ensure!(
        overlap < chunk_size,
        "overlap должен быть меньше размера чанка"
    );
    if strategy == IndexStrategy::All {
        bail!("для chunking требуется конкретная стратегия");
    }
    let mut result = Vec::new();
    for document in documents {
        let pieces = match strategy {
            IndexStrategy::Fixed => fixed_pieces(&document.content, chunk_size, overlap),
            IndexStrategy::Structural => structural_pieces(&document.content, chunk_size, overlap),
            IndexStrategy::All => unreachable!(),
        };
        for (ordinal, (section, content)) in pieces.into_iter().enumerate() {
            let content_hash = sha256_hex(content.as_bytes());
            let canonical = format!(
                "chunk-v1\0{}\0{}\0{}\0{}\0{}\0{}",
                document.source, strategy, chunk_size, overlap, ordinal, section
            );
            let chunk_id = sha256_hex(format!("{canonical}\0{content_hash}").as_bytes());
            result.push(Chunk {
                chunk_id,
                strategy,
                source: document.source.clone(),
                title: document.title.clone(),
                section,
                ordinal,
                char_len: content.chars().count(),
                content,
                content_hash,
            });
        }
    }
    Ok(result)
}

fn fixed_pieces(text: &str, chunk_size: usize, overlap: usize) -> Vec<(String, String)> {
    let headings = headings(text);
    split_ranges(text, chunk_size, overlap)
        .into_iter()
        .filter_map(|range| {
            let content = text[range.clone()].trim().to_owned();
            if content.is_empty() {
                return None;
            }
            let section = section_at(&headings, range.start);
            Some((section, content))
        })
        .collect()
}

fn structural_pieces(text: &str, chunk_size: usize, overlap: usize) -> Vec<(String, String)> {
    let headings = headings(text);
    if headings.is_empty() {
        return split_ranges(text, chunk_size, overlap)
            .into_iter()
            .filter_map(|range| {
                let content = text[range].trim().to_owned();
                (!content.is_empty()).then(|| ("document".to_owned(), content))
            })
            .collect();
    }

    let mut sections = Vec::new();
    if headings[0].start > 0 {
        sections.push(("document".to_owned(), 0..headings[0].start));
    }
    let mut hierarchy: Vec<Option<String>> = vec![None; 6];
    for (index, heading) in headings.iter().enumerate() {
        hierarchy.truncate(6);
        for entry in hierarchy.iter_mut().skip(heading.level) {
            *entry = None;
        }
        hierarchy[heading.level - 1] = Some(heading.title.clone());
        let section = hierarchy
            .iter()
            .filter_map(Clone::clone)
            .collect::<Vec<_>>()
            .join(" > ");
        let end = headings
            .get(index + 1)
            .map_or(text.len(), |next| next.start);
        sections.push((section, heading.start..end));
    }

    let mut result = Vec::new();
    for (section, range) in sections {
        let section_text = &text[range];
        for child in split_ranges(section_text, chunk_size, overlap) {
            let content = section_text[child].trim().to_owned();
            if !content.is_empty() {
                result.push((section.clone(), content));
            }
        }
    }
    result
}

fn headings(text: &str) -> Vec<Heading> {
    let mut result = Vec::new();
    let mut current: Option<(usize, usize, String)> = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(value) | Event::Code(value) if current.is_some() => {
                current.as_mut().unwrap().2.push_str(&value);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, title)) = current.take() {
                    result.push(Heading {
                        level,
                        start,
                        title: title.trim().to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    result
}

fn heading_level(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn section_at(headings: &[Heading], offset: usize) -> String {
    let mut hierarchy: Vec<Option<&str>> = vec![None; 6];
    for heading in headings
        .iter()
        .take_while(|heading| heading.start <= offset)
    {
        for entry in hierarchy.iter_mut().skip(heading.level) {
            *entry = None;
        }
        hierarchy[heading.level - 1] = Some(&heading.title);
    }
    let section = hierarchy
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" > ");
    if section.is_empty() {
        "document".to_owned()
    } else {
        section
    }
}

fn split_ranges(text: &str, chunk_size: usize, overlap: usize) -> Vec<Range<usize>> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    let total_chars = boundaries.len() - 1;
    let mut ranges = Vec::new();
    let mut start_char = 0;
    while start_char < total_chars {
        let hard_end_char = (start_char + chunk_size).min(total_chars);
        let mut end_byte = boundaries[hard_end_char];
        if hard_end_char < total_chars {
            let start_byte = boundaries[start_char];
            let candidate = &text[start_byte..end_byte];
            let minimum_chars = chunk_size / 2;
            let preferred = candidate
                .rfind("\n\n")
                .map(|index| index + 2)
                .or_else(|| candidate.rfind('\n').map(|index| index + 1));
            if let Some(relative) = preferred {
                let preferred_byte = start_byte + relative;
                let preferred_chars = text[start_byte..preferred_byte].chars().count();
                if preferred_chars >= minimum_chars {
                    end_byte = preferred_byte;
                }
            }
        }
        let start_byte = boundaries[start_char];
        ranges.push(start_byte..end_byte);
        if end_byte == text.len() {
            break;
        }
        let end_char = boundaries.binary_search(&end_byte).unwrap();
        let next = end_char.saturating_sub(overlap).max(start_char + 1);
        start_char = next;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(source: &str, content: &str) -> Document {
        Document {
            source: source.to_owned(),
            title: "Документ".to_owned(),
            content: content.to_owned(),
            document_hash: sha256_hex(content.as_bytes()),
            byte_len: content.len(),
            char_len: content.chars().count(),
        }
    }

    #[test]
    fn fixed_chunking_is_unicode_aware_and_bounded() {
        let content = "Привет мир\n\n".repeat(20);
        let chunks =
            chunk_documents(&[document("a.md", &content)], IndexStrategy::Fixed, 40, 5).unwrap();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.char_len <= 40));
        assert!(chunks.iter().all(|chunk| !chunk.content.is_empty()));
    }

    #[test]
    fn structural_chunking_tracks_hierarchy_and_ignores_fenced_hashes() {
        let content =
            "Вступление\n\n# Root\nText\n\n```rust\n# not a heading\n```\n\n## Child\nBody";
        let chunks = chunk_documents(
            &[document("a.md", content)],
            IndexStrategy::Structural,
            200,
            10,
        )
        .unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].section, "document");
        assert_eq!(chunks[1].section, "Root");
        assert_eq!(chunks[2].section, "Root > Child");
    }

    #[test]
    fn chunk_ids_are_stable_and_source_specific() {
        let first =
            chunk_documents(&[document("a.md", "same")], IndexStrategy::Fixed, 20, 2).unwrap();
        let repeated =
            chunk_documents(&[document("a.md", "same")], IndexStrategy::Fixed, 20, 2).unwrap();
        let other =
            chunk_documents(&[document("b.md", "same")], IndexStrategy::Fixed, 20, 2).unwrap();
        assert_eq!(first[0].chunk_id, repeated[0].chunk_id);
        assert_ne!(first[0].chunk_id, other[0].chunk_id);
        assert_eq!(first[0].content_hash, other[0].content_hash);
    }

    #[test]
    fn structural_large_section_uses_same_section_for_children() {
        let content = format!("# Long\n{}", "слово ".repeat(40));
        let chunks = chunk_documents(
            &[document("a.md", &content)],
            IndexStrategy::Structural,
            50,
            5,
        )
        .unwrap();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.section == "Long"));
        assert!(chunks.iter().all(|chunk| chunk.char_len <= 50));
    }

    #[test]
    fn structural_chunking_supports_setext_headings() {
        let chunks = chunk_documents(
            &[document("a.md", "Root\n====\nBody")],
            IndexStrategy::Structural,
            100,
            5,
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].section, "Root");
    }
}
