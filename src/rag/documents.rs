use anyhow::{bail, Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Document {
    pub(crate) source: String,
    pub(crate) title: String,
    pub(crate) content: String,
    pub(crate) document_hash: String,
    pub(crate) byte_len: usize,
    pub(crate) char_len: usize,
}

pub(crate) fn discover_documents(root: &Path, sources: &[PathBuf]) -> Result<Vec<Document>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("не удалось определить корень {}", root.display()))?;
    let requested = if sources.is_empty() {
        vec![root.join("reports"), root.join("openspec/specs")]
    } else {
        sources
            .iter()
            .map(|source| {
                if source.is_absolute() {
                    source.clone()
                } else {
                    root.join(source)
                }
            })
            .collect()
    };

    let mut files = BTreeSet::new();
    for source in requested {
        collect_markdown_files(&root, &source, &mut files)?;
    }
    if files.is_empty() {
        bail!("в указанных источниках не найдено Markdown-документов");
    }

    files
        .into_iter()
        .map(|path| load_document(&root, &path))
        .collect()
}

fn collect_markdown_files(root: &Path, path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("источник недоступен: {}", path.display()))?;
    anyhow::ensure!(
        canonical.starts_with(root),
        "источник находится вне корня проекта: {}",
        path.display()
    );
    if canonical.is_file() {
        if is_markdown(&canonical) {
            files.insert(canonical);
        }
        return Ok(());
    }
    anyhow::ensure!(
        canonical.is_dir(),
        "неподдерживаемый источник: {}",
        path.display()
    );
    let mut children = fs::read_dir(&canonical)
        .with_context(|| format!("не удалось прочитать {}", canonical.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        collect_markdown_files(root, &child.path(), files)?;
    }
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn load_document(root: &Path, path: &Path) -> Result<Document> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("не удалось прочитать документ {}", path.display()))?;
    let content = normalize_markdown(&raw);
    let source = path
        .strip_prefix(root)
        .expect("canonical source is below canonical root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let title = first_h1(&content).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("document")
            .to_owned()
    });
    Ok(Document {
        source,
        title,
        document_hash: sha256_hex(content.as_bytes()),
        byte_len: content.len(),
        char_len: content.chars().count(),
        content,
    })
}

fn normalize_markdown(raw: &str) -> String {
    raw.strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n")
}

fn first_h1(markdown: &str) -> Option<String> {
    let mut in_h1 = false;
    let mut title = String::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => in_h1 = true,
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if in_h1 => {
                let title = title.trim();
                return (!title.is_empty()).then(|| title.to_owned());
            }
            Event::Text(value) | Event::Code(value) if in_h1 => title.push_str(&value),
            _ => {}
        }
    }
    None
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_sorted_normalized_and_uses_h1_or_filename() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("docs")).unwrap();
        fs::write(
            directory.path().join("docs/b.md"),
            "\u{feff}# Заголовок\r\nТекст",
        )
        .unwrap();
        fs::write(directory.path().join("docs/a.md"), "Без заголовка").unwrap();
        fs::write(directory.path().join("docs/skip.txt"), "skip").unwrap();
        let documents =
            discover_documents(directory.path(), &["docs".into(), "docs/b.md".into()]).unwrap();
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].source, "docs/a.md");
        assert_eq!(documents[0].title, "a");
        assert_eq!(documents[1].title, "Заголовок");
        assert!(!documents[1].content.contains('\r'));
        assert!(!documents[1].content.starts_with('\u{feff}'));
    }

    #[test]
    fn discovery_rejects_empty_missing_and_outside_sources() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("empty")).unwrap();
        assert!(discover_documents(directory.path(), &["empty".into()]).is_err());
        assert!(discover_documents(directory.path(), &["missing".into()]).is_err());
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.md"), "secret").unwrap();
        assert!(discover_documents(directory.path(), &[outside.path().into()]).is_err());
    }
}
