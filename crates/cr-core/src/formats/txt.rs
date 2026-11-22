//! Plain text and generic byte decoding.

use std::path::Path;

use crate::error::Result;
use crate::formats::{BookText, Chapter};
use crate::metadata::Metadata;
use crate::text;

/// Decode bytes using BOM markers, then UTF-8, falling back to Windows-1252
/// (the encoding most often found in older ebook files).
pub fn decode_bytes(data: &[u8]) -> String {
    if let Some(rest) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(rest);
        return decoded.into_owned();
    }
    if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        let (decoded, _, _) = encoding_rs::UTF_16BE.decode(rest);
        return decoded.into_owned();
    }
    match std::str::from_utf8(data) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(data);
            decoded.into_owned()
        }
    }
}

pub fn metadata(path: &Path) -> Metadata {
    let mut meta = Metadata::new(title_from_path(path));
    meta.format = Some("TXT".to_string());
    meta
}

pub fn text(path: &Path) -> Result<BookText> {
    let data = std::fs::read(path)?;
    let content = decode_bytes(&data);
    Ok(split_chapters(&content, &title_from_path(path)))
}

pub fn html_text(path: &Path) -> Result<BookText> {
    let data = std::fs::read(path)?;
    let markup = decode_bytes(&data);
    let body = super::html::html_to_text(&markup);
    let title = super::html::document_title(&markup)
        .or_else(|| super::html::first_heading(&markup))
        .unwrap_or_else(|| title_from_path(path));
    Ok(BookText {
        chapters: if body.trim().is_empty() {
            Vec::new()
        } else {
            vec![Chapter { title, text: body }]
        },
    })
}

/// Strip underscores and noisy suffixes from a file name to make a title.
pub fn title_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let cleaned = stem.replace('_', " ");
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Unknown".to_string()
    } else {
        collapsed
    }
}

fn split_chapters(content: &str, fallback_title: &str) -> BookText {
    let mut chapters = Vec::new();
    let mut current_title = fallback_title.to_string();
    let mut buffer = String::new();

    for line in content.replace('\r', "\n").split('\n') {
        if looks_like_heading(line) {
            if !buffer.trim().is_empty() {
                chapters.push(Chapter {
                    title: current_title,
                    text: buffer.trim().to_string(),
                });
                buffer.clear();
            }
            current_title = line.trim().to_string();
        } else {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    if !buffer.trim().is_empty() {
        chapters.push(Chapter {
            title: current_title,
            text: buffer.trim().to_string(),
        });
    }
    BookText { chapters }
}

fn looks_like_heading(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 || trimmed.ends_with('.') {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if [
        "chapter ",
        "part ",
        "book ",
        "volume ",
        "section ",
        "prologue",
        "epilogue",
        "introduction",
        "preface",
        "foreword",
        "afterword",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    lower
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == ' ')
        && lower.chars().any(|c| c.is_ascii_digit())
}

/// Human readable word count, useful for `cr show` and reading progress.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Convenience for building filename-derived metadata consistently.
pub fn filename_metadata(path: &Path) -> Metadata {
    let format = text::format_from_path(path).unwrap_or_else(|| "UNKNOWN".to_string());
    let mut meta = Metadata::new(title_from_path(path));
    meta.format = Some(format);
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_encodings() {
        assert_eq!(decode_bytes(b"hello"), "hello");
        assert_eq!(decode_bytes(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(decode_bytes(&[0xE9]), "\u{e9}");
    }

    #[test]
    fn splits_txt_chapters() {
        let text = "Chapter 1\nFirst part.\n\nChapter 2\nThe end.\n";
        let book = split_chapters(text, "Book");
        assert_eq!(book.chapters.len(), 2);
        assert_eq!(book.chapters[0].title, "Chapter 1");
        assert_eq!(book.chapters[1].text, "The end.");
    }

    #[test]
    fn keeps_unnumbered_text_as_one_chapter() {
        let book = split_chapters("Just some prose.\n\nMore prose.", "Story");
        assert_eq!(book.chapters.len(), 1);
        assert_eq!(book.chapters[0].title, "Story");
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(
            title_from_path(Path::new("the_great_book.txt")),
            "the great book"
        );
    }
}
