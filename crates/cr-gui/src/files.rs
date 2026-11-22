//! Helpers for paths handed to the app by the OS: drag and drop, "Open With"
//! and file URLs.

use std::path::{Path, PathBuf};

use percent_encoding_lite::decode;

/// File extensions calibre-rust can add to a library.
pub const EBOOK_EXTENSIONS: &[&str] = &[
    "epub", "pdf", "mobi", "azw", "azw3", "prc", "kfx", "fb2", "txt", "html", "htm", "rtf", "docx",
    "odt", "cbz", "cbr", "djvu", "lit", "pdb", "md",
];

/// Image types accepted as a cover.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png"];

/// Paths sorted by what the app should do with them.
#[derive(Debug, Default)]
pub struct DroppedPaths {
    /// A folder that contains a calibre `metadata.db`.
    pub library: Option<PathBuf>,
    /// Ebook files, including those found inside dropped folders.
    pub books: Vec<PathBuf>,
}

pub fn classify(paths: impl IntoIterator<Item = PathBuf>) -> DroppedPaths {
    let mut result = DroppedPaths::default();
    for path in paths {
        if path.is_dir() {
            if is_library(&path) {
                if result.library.is_none() {
                    result.library = Some(path);
                }
            } else {
                collect_books(&path, 0, &mut result.books);
            }
        } else if path.file_name().is_some_and(|name| name == "metadata.db") {
            if let Some(parent) = path.parent() {
                result.library.get_or_insert_with(|| parent.to_path_buf());
            }
        } else if has_extension(&path, EBOOK_EXTENSIONS) {
            result.books.push(path);
        }
    }
    result
}

pub fn is_library(path: &Path) -> bool {
    path.join("metadata.db").is_file()
}

pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

fn collect_books(directory: &Path, depth: usize, books: &mut Vec<PathBuf>) {
    // Dropped folders are scanned a few levels deep, which covers the usual
    // "Author/Title/book.epub" layouts without walking an entire disk.
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            collect_books(&path, depth + 1, books);
        } else if has_extension(&path, EBOOK_EXTENSIONS) {
            books.push(path);
        }
    }
}

/// Converts a `file://` URL (or a bare path) from the OS into a path.
pub fn path_from_url(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        // Drop an optional host ("file://localhost/path").
        let path = match rest.find('/') {
            Some(0) => rest,
            Some(index) => &rest[index..],
            None => return None,
        };
        let decoded = decode(path)?;
        #[cfg(windows)]
        let decoded = decoded.trim_start_matches('/').replace('/', "\\");
        return Some(PathBuf::from(decoded));
    }
    if url.contains("://") {
        return None;
    }
    Some(PathBuf::from(url))
}

/// Minimal percent-decoding so this crate does not need another dependency.
mod percent_encoding_lite {
    pub fn decode(input: &str) -> Option<String> {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let hex = input.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(out).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(
            path_from_url("file:///Users/me/My%20Books/a.epub"),
            Some(PathBuf::from("/Users/me/My Books/a.epub"))
        );
        assert_eq!(
            path_from_url("file://localhost/tmp/b.pdf"),
            Some(PathBuf::from("/tmp/b.pdf"))
        );
        assert_eq!(path_from_url("https://example.com/a.epub"), None);
        assert_eq!(
            path_from_url("/tmp/c.txt"),
            Some(PathBuf::from("/tmp/c.txt"))
        );
    }

    #[test]
    fn matches_extensions_case_insensitively() {
        assert!(has_extension(Path::new("a/B.EPUB"), EBOOK_EXTENSIONS));
        assert!(!has_extension(Path::new("a/notes"), EBOOK_EXTENSIONS));
    }
}
