//! Ebook format detection, metadata import and text extraction.

mod conversion;
pub mod css;
pub mod document;
mod documents;
pub mod editing;
pub mod epub;
pub mod fb2;
pub mod html;
mod mobi;
pub mod pdf;
pub mod reading;
pub mod txt;

pub use conversion::{
    convert, convert_with, read_document, ConvertOptions, Profile, INPUT_FORMATS, OUTPUT_FORMATS,
};

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};
use crate::metadata::{htmlize_comments, ImportedBook, Metadata};
use crate::text;
use txt::decode_bytes;

/// One readable section of a book.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub text: String,
}

/// A book converted to plain text, split into chapters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookText {
    pub chapters: Vec<Chapter>,
}

impl BookText {
    pub fn is_empty(&self) -> bool {
        self.chapters.iter().all(|c| c.text.trim().is_empty())
    }

    pub fn full_text(&self) -> String {
        self.chapters
            .iter()
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn word_count(&self) -> usize {
        self.chapters
            .iter()
            .map(|c| c.text.split_whitespace().count())
            .sum()
    }
}

/// The canonical format name for a path, e.g. `EPUB`, `AZW3`, `HTML`.
pub fn detect_format(path: &Path) -> String {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let upper = ext.trim().to_ascii_uppercase();
        return match upper.as_str() {
            "HTM" | "XHTML" | "XHT" => "HTML".to_string(),
            other if !other.is_empty() => other.to_string(),
            _ => sniff_format(path).unwrap_or_else(|| "UNKNOWN".to_string()),
        };
    }
    sniff_format(path).unwrap_or_else(|| "UNKNOWN".to_string())
}

fn sniff_format(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut magic = Vec::new();
    file.by_ref().take(68).read_to_end(&mut magic).ok()?;
    if magic.starts_with(b"%PDF-") {
        return Some("PDF".to_string());
    }
    if magic.starts_with(b"PK\x03\x04") {
        let file = File::open(path).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        if let Ok(mut entry) = archive.by_name("mimetype") {
            let mut value = String::new();
            if entry.read_to_string(&mut value).is_ok() && value.contains("epub") {
                return Some("EPUB".to_string());
            }
        };
        if let Ok(mut entry) = archive.by_name("word/document.xml") {
            let mut value = String::new();
            if entry.read_to_string(&mut value).is_ok() {
                return Some("DOCX".to_string());
            }
        };
    }
    if let Some(kind) = mobi::sniff(&mut file, &magic) {
        return Some(kind.to_string());
    }
    None
}

/// Import a file: extract metadata and cover where the format supports it.
pub fn import(path: &Path) -> Result<ImportedBook> {
    if !path.is_file() {
        return Err(Error::Other(format!("not a file: {}", path.display())));
    }
    let format = detect_format(path);
    let (mut metadata, cover) =
        match format.as_str() {
            "EPUB" => epub::metadata(path)?,
            "FB2" => fb2::read(path)?,
            "TXT" => (txt::metadata(path), None),
            "HTML" => (html_metadata(path)?, None),
            "AZW3" | "AZW" | "MOBI" | "PRC" => {
                mobi::metadata(path).unwrap_or_else(|_| (txt::filename_metadata(path), None))
            }
            // A file that cannot be parsed is still worth storing.
            "DOCX" => documents::docx_metadata(path)
                .unwrap_or_else(|_| (txt::filename_metadata(path), None)),
            "CBZ" | "CBR" => documents::cbz_metadata(path)
                .unwrap_or_else(|_| (txt::filename_metadata(path), None)),
            "RTF" => (
                documents::rtf_metadata(path).unwrap_or_else(|_| txt::filename_metadata(path)),
                None,
            ),
            "PDF" => (
                documents::pdf_metadata(path).unwrap_or_else(|_| txt::filename_metadata(path)),
                pdf::cover(path),
            ),
            _ => (txt::filename_metadata(path), None),
        };
    metadata.format = Some(format.clone());
    if metadata.uuid.is_none() {
        metadata.uuid = Some(uuid::Uuid::new_v4().to_string());
    }
    if let Some(cover) = cover {
        metadata.cover = normalize_cover(&cover);
    }
    metadata.normalize();
    Ok(ImportedBook {
        metadata,
        format,
        source: path.to_path_buf(),
    })
}

/// Extract readable text from a book in its own format.
pub fn extract_text(path: &Path, format: Option<&str>) -> Result<BookText> {
    let format = format
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| detect_format(path));
    match format.as_str() {
        "EPUB" => epub::text(path),
        "FB2" => fb2::text(path),
        "TXT" => txt::text(path),
        "HTML" => txt::html_text(path),
        "AZW3" | "KF8" | "AZW" | "MOBI" | "PRC" => mobi::text(path),
        "DOCX" => documents::docx_text(path),
        "RTF" => documents::rtf_text(path),
        "PDF" => pdf::text(path),
        other => Err(Error::UnsupportedFormat(format!(
            "{other} text extraction is not implemented yet"
        ))),
    }
}

/// Decode markup bytes into a string.
pub fn decode_markup(data: &[u8]) -> String {
    decode_bytes(data)
}

/// Normalise a cover image to JPEG (what calibre stores as `cover.jpg`).
pub fn normalize_cover(data: &[u8]) -> Option<Vec<u8>> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(data.to_vec());
    }
    // SVG covers cannot be rasterised without a renderer; leave them out.
    let head = &data[..data.len().min(256)];
    if head.starts_with(b"<svg") || head.starts_with(b"<?xml") {
        return None;
    }
    let image = image::load_from_memory(data).ok()?;
    let rgb = image.to_rgb8();
    let mut output = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 88);
    encoder.encode_image(&rgb).ok()?;
    Some(output)
}

fn html_metadata(path: &Path) -> Result<Metadata> {
    let data = std::fs::read(path)?;
    let markup = decode_bytes(&data);
    let mut meta = Metadata::new(
        html::document_title(&markup)
            .or_else(|| html::first_heading(&markup))
            .unwrap_or_else(|| txt::title_from_path(path)),
    );
    if let Some(author) = html::meta_content(&markup, &["author", "dc.creator", "dcterms.creator"])
    {
        meta.authors = text::split_authors(&author);
    }
    if let Some(language) = html::meta_content(&markup, &["language", "dc.language"]) {
        meta.languages.push(language);
    }
    if let Some(keywords) = html::meta_content(&markup, &["keywords"]) {
        meta.tags.extend(
            keywords
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        );
    }
    if let Some(description) = html::meta_content(&markup, &["description", "dc.description"]) {
        meta.comments = Some(htmlize_comments(&description));
    }
    meta.format = Some("HTML".to_string());
    meta.normalize();
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(detect_format(Path::new("book.epub")), "EPUB");
        assert_eq!(detect_format(Path::new("book.xhtml")), "HTML");
        assert_eq!(detect_format(Path::new("book.azw3")), "AZW3");
        assert_eq!(detect_format(Path::new("book.azw")), "AZW");
        assert_eq!(detect_format(Path::new("book.prc")), "PRC");
        assert_eq!(detect_format(Path::new("book.unknownext")), "UNKNOWNEXT");
    }

    #[test]
    fn passes_through_jpeg_covers() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
        assert_eq!(normalize_cover(&jpeg), Some(jpeg));
        assert_eq!(normalize_cover(b"<svg></svg>"), None);
    }

    #[test]
    fn computes_word_counts() {
        let book = BookText {
            chapters: vec![
                Chapter {
                    title: "A".into(),
                    text: "one two".into(),
                },
                Chapter {
                    title: "B".into(),
                    text: "three".into(),
                },
            ],
        };
        assert_eq!(book.word_count(), 3);
        assert_eq!(book.full_text(), "one two\n\nthree");
    }
}
