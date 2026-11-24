//! Helpers for paths handed to the app by the OS: drag and drop, "Open With"
//! and file URLs.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// File extensions calibre-rust can add to a library.
pub const EBOOK_EXTENSIONS: &[&str] = &[
    "epub", "pdf", "mobi", "azw", "azw3", "prc", "kfx", "fb2", "txt", "html", "htm", "rtf", "docx",
    "odt", "cbz", "cbr", "cb7", "djvu", "lit", "pdb", "md", "kepub", "kf8", "htmlz",
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
            if let Some(parent) = path
                .parent()
                .map(|parent| {
                    if parent.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        parent
                    }
                })
                .filter(|parent| is_library(parent))
            {
                result.library.get_or_insert_with(|| parent.to_path_buf());
            }
        } else if path.is_file() && has_extension(&path, EBOOK_EXTENSIONS) {
            result.books.push(path);
        }
    }
    let mut seen = HashSet::new();
    result
        .books
        .retain(|path| seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())));
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
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'));
        if hidden {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if !is_library(&path) {
                collect_books(&path, depth + 1, books);
            }
        } else if path.is_file() && has_extension(&path, EBOOK_EXTENSIONS) {
            books.push(path);
        }
    }
}

/// Converts a `file://` URL (or a bare path) from the OS into a path.
pub fn path_from_url(url: &str) -> Option<PathBuf> {
    if url
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
    {
        let parsed = url::Url::parse(url).ok()?;
        if parsed.host().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
            return None;
        }
        let path = parsed.to_file_path().ok()?;
        return (!path.as_os_str().as_encoded_bytes().contains(&0)).then_some(path);
    }
    if url.is_empty() || url.contains("://") || url.contains('\0') {
        return None;
    }
    Some(PathBuf::from(url))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    Paths(Vec<PathBuf>),
    Preferences,
    ChooseLibrary,
    AddBooks,
    OpenBook,
    Read(PathBuf),
    Help,
    Version,
}

pub fn launch(args: impl IntoIterator<Item = OsString>) -> Result<Launch, String> {
    let mut args = args.into_iter();
    let mut bare = false;
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        let flag = (!bare).then(|| arg.to_str()).flatten();
        let action = match flag {
            Some("--") => {
                bare = true;
                continue;
            }
            Some("--help" | "-h") => Launch::Help,
            Some("--version" | "-V") => Launch::Version,
            Some("--preferences") => Launch::Preferences,
            Some("--choose-library") => Launch::ChooseLibrary,
            Some("--add-books") => Launch::AddBooks,
            Some("--open-book") => Launch::OpenBook,
            Some("--read") => Launch::Read(launch_path(
                args.next().ok_or("--read requires one ebook path.")?,
            )?),
            Some(flag) if flag.starts_with("-psn_") && cfg!(target_os = "macos") => continue,
            Some(flag) if flag.starts_with('-') => return Err(format!("Unknown option: {flag}")),
            _ => {
                paths.push(launch_path(arg)?);
                continue;
            }
        };
        if !paths.is_empty() || args.next().is_some() {
            return Err("Launch options must be used on their own.".into());
        }
        return Ok(action);
    }
    Ok(Launch::Paths(paths))
}

fn launch_path(arg: OsString) -> Result<PathBuf, String> {
    match arg.to_str() {
        Some(arg) => path_from_url(arg).ok_or_else(|| format!("Expected a local path: {arg}")),
        None => Ok(PathBuf::from(arg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    #[cfg(unix)]
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
        assert_eq!(path_from_url("file://remote/etc/a.epub"), None);
        assert_eq!(path_from_url("file:///tmp/a%00.epub"), None);
        assert_eq!(
            path_from_url("FILE:///tmp/a%23b.epub"),
            Some(PathBuf::from("/tmp/a#b.epub"))
        );
        assert_eq!(
            path_from_url("/tmp/c.txt"),
            Some(PathBuf::from("/tmp/c.txt"))
        );
    }

    #[core::prelude::v1::test]
    fn matches_extensions_case_insensitively() {
        assert!(has_extension(Path::new("a/B.EPUB"), EBOOK_EXTENSIONS));
        assert!(!has_extension(Path::new("a/notes"), EBOOK_EXTENSIONS));
    }

    #[core::prelude::v1::test]
    fn launch_accepts_filenames_after_separator_and_exclusive_actions() {
        assert_eq!(
            launch(["--", "--preferences", "some book.epub"].map(OsString::from)).unwrap(),
            Launch::Paths(vec!["--preferences".into(), "some book.epub".into()])
        );
        assert_eq!(
            launch(["--read", "some book.epub"].map(OsString::from)).unwrap(),
            Launch::Read("some book.epub".into())
        );
        assert!(launch(["--read"].map(OsString::from)).is_err());
        assert!(launch(["--preferences", "book.epub"].map(OsString::from)).is_err());
    }

    #[core::prelude::v1::test]
    fn folder_import_deduplicates_inputs_and_skips_nested_libraries() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.kepub");
        let nested = dir.path().join("nested library");
        std::fs::create_dir(&nested).unwrap();
        for path in [
            &book,
            &nested.join("metadata.db"),
            &nested.join("book.epub"),
        ] {
            std::fs::write(path, b"").unwrap();
        }
        let dropped = classify([dir.path().to_path_buf(), book.clone(), nested.clone()]);
        assert_eq!(dropped.books, vec![book]);
        assert_eq!(dropped.library, Some(nested));
    }
}
