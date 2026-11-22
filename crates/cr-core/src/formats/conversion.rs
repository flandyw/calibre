//! Atomic file conversion between EPUB and the supported Kindle formats.

use std::fs;
use std::io::Write;
use std::path::{Component, Path};
use std::sync::LazyLock;

use kindling::{extracted::ExtractedEpub, opf::OPFData};
use percent_encoding::percent_decode_str;
use regex::Regex;
use tempfile::NamedTempFile;

use super::document;
use super::{detect_format, epub, mobi};
use crate::error::{Error, Result};
use crate::metadata::Metadata;
use crate::text::format_from_path;

/// Formats a conversion can start from, best source first.
pub const INPUT_FORMATS: [&str; 11] = [
    "EPUB", "AZW3", "KF8", "MOBI", "AZW", "PRC", "FB2", "DOCX", "HTML", "RTF", "TXT",
];
/// Formats a conversion can produce.
pub const OUTPUT_FORMATS: [&str; 5] = ["EPUB", "AZW3", "MOBI", "HTMLZ", "TXT"];

/// The screen a conversion targets; larger images are scaled down to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Profile {
    /// Keep images as they are.
    #[default]
    Generic,
    Kindle,
    Kobo,
    Tablet,
}

impl Profile {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "generic" | "default" => Some(Self::Generic),
            "kindle" => Some(Self::Kindle),
            "kobo" => Some(Self::Kobo),
            "tablet" => Some(Self::Tablet),
            _ => None,
        }
    }

    /// Screen width and height in pixels.
    pub fn screen(self) -> Option<(u32, u32)> {
        match self {
            Self::Generic => None,
            Self::Kindle => Some((1072, 1448)),
            Self::Kobo => Some((1264, 1680)),
            Self::Tablet => Some((1600, 2560)),
        }
    }
}

/// Settings for one conversion.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// Metadata to put in the output instead of what the source file holds.
    pub metadata: Option<Metadata>,
    /// Cover to put in the output instead of the one in the source file.
    pub cover: Option<Vec<u8>>,
    /// Start a new section at chapter headings (`h1`/`h2`, `Heading 1`,
    /// "Chapter ..." lines in plain text).
    pub split_chapters: bool,
    pub profile: Profile,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            metadata: None,
            cover: None,
            split_chapters: true,
            profile: Profile::Generic,
        }
    }
}

impl ConvertOptions {
    /// Whether the source's own content can be used as it is.
    fn keeps_source(&self) -> bool {
        self.metadata.is_none() && self.cover.is_none() && self.profile == Profile::Generic
    }
}

fn is_kindle(format: &str) -> bool {
    matches!(format, "AZW3" | "KF8" | "AZW" | "MOBI" | "PRC")
}

/// Convert a book with the default settings. See [`convert_with`].
pub fn convert(input: &Path, output: &Path) -> Result<()> {
    convert_with(input, output, &ConvertOptions::default())
}

/// Convert a book; the output extension selects the target format.
///
/// EPUB to Kindle and Kindle to EPUB keep the book's own files (styles and
/// fonts included). Everything else is read into a [`document::Document`] and written
/// from it. An existing output file is preserved, and a failed conversion
/// leaves no output file behind.
pub fn convert_with(input: &Path, output: &Path, options: &ConvertOptions) -> Result<()> {
    if !input.is_file() {
        return Err(Error::Other(format!("not a file: {}", input.display())));
    }
    let source = detect_format(input);
    let target = format_from_path(output).unwrap_or_default();
    let same = source == target || (is_kindle(&source) && is_kindle(&target));
    if same
        || !INPUT_FORMATS.contains(&source.as_str())
        || !OUTPUT_FORMATS.contains(&target.as_str())
    {
        return Err(Error::UnsupportedFormat(format!(
            "cannot convert {source} to {target}; inputs are {}, outputs are {}",
            INPUT_FORMATS.join(", "),
            OUTPUT_FORMATS.join(", ")
        )));
    }
    if output.symlink_metadata().is_ok() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", output.display()),
        )));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;

    // A Kindle book is read through its EPUB form.
    let mut unpacked = None;
    let (input, source) = if is_kindle(&source) {
        if target == "EPUB" && options.keeps_source() {
            mobi::write_epub(input, temporary.as_file_mut())?;
            temporary
                .persist_noclobber(output)
                .map_err(|err| err.error)?;
            return Ok(());
        }
        let mut epub = tempfile::Builder::new().suffix(".epub").tempfile()?;
        mobi::write_epub(input, epub.as_file_mut())?;
        epub.as_file_mut().flush()?;
        (unpacked.insert(epub).path(), "EPUB")
    } else {
        (input, source.as_str())
    };

    if source == "EPUB" && is_kindle(&target) && options.keeps_source() {
        write_kindle(input, temporary.path(), target == "AZW3")?;
    } else {
        let mut document = document::read(
            input,
            source,
            document::ReadOptions {
                split_chapters: options.split_chapters,
            },
        )?;
        if let Some(metadata) = &options.metadata {
            document.metadata = metadata.clone();
            if let Some(cover) = document.metadata.cover.take() {
                document.cover = Some(cover);
            }
        }
        if let Some(cover) = &options.cover {
            document.cover = Some(cover.clone());
        }
        if let Some((width, height)) = options.profile.screen() {
            document.fit_images(width, height);
        }
        match target.as_str() {
            "EPUB" => document::write_epub(&document, temporary.as_file_mut())?,
            "HTMLZ" => document::write_htmlz(&document, temporary.as_file_mut())?,
            "TXT" => document::write_txt(&document, temporary.as_file_mut())?,
            _ => {
                let mut epub = tempfile::Builder::new().suffix(".epub").tempfile()?;
                document::write_epub(&document, epub.as_file_mut())?;
                epub.as_file_mut().flush()?;
                write_kindle(epub.path(), temporary.path(), target == "AZW3")?;
            }
        }
    }
    temporary
        .persist_noclobber(output)
        .map_err(|err| err.error)?;
    Ok(())
}

fn write_kindle(input: &Path, output: &Path, kf8_only: bool) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = &temporary.path().canonicalize()?;
    epub::unpack(input, root)?;
    let container = fs::read(root.join("META-INF/container.xml"))
        .map_err(|_| Error::InvalidEpub("missing META-INF/container.xml".into()))?;
    let package = epub::package_path(&container)?;
    let opf_path = resolve_inside(root, root, &package)?;
    let opf = OPFData::parse(&opf_path).map_err(|error| Error::InvalidEpub(error.to_string()))?;
    validate_resources(root, &opf)?;
    let epub = ExtractedEpub::from_opf_path(&opf_path)
        .map_err(|error| Error::InvalidEpub(error.to_string()))?;
    kindling::mobi::build_mobi_from_extracted(
        &epub,
        output,
        false, // use PalmDOC compression
        false, // include dictionary inflections
        None,  // omit embedded source archive
        false, // omit CMET
        false, // retain HD images
        false, // omit creator tag
        kf8_only,
        Some("EBOK"),
        true,  // apply Kindle size limits
        false, // skip device-metadata post-build check
        false, // use normal Kindling layout
        false, // use normal accent handling
        false, // don't fold accents
        false, // retain embedded fonts
    )
    .map_err(|error| Error::Kindle(error.to_string()))
}

fn validate_resources(root: &Path, opf: &OPFData) -> Result<()> {
    for (href, _) in opf.manifest.values() {
        validate_href(root, &opf.base_dir, href)?;
    }
    for (_, href) in &opf.spine_items {
        validate_href(root, &opf.base_dir, href)?;
    }
    if let Some(href) = &opf.embedded_cover_href {
        validate_href(root, &opf.base_dir, href)?;
    }

    // Kindling also loads linked stylesheets directly from the spine markup.
    static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<link\b[^>]*>").unwrap());
    static DICTIONARY_HREF: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#).unwrap());
    static BOOK_HREF: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*"([^"]+\.css)""#).unwrap());
    for path in opf.get_content_html_paths() {
        let content = fs::read_to_string(path)?;
        for link in LINK.find_iter(&content) {
            for href in DICTIONARY_HREF
                .captures_iter(link.as_str())
                .chain(BOOK_HREF.captures_iter(link.as_str()))
            {
                validate_href(root, &opf.base_dir, &href[1])?;
            }
        }
    }
    Ok(())
}

fn validate_href(root: &Path, base: &Path, href: &str) -> Result<()> {
    resolve_inside(root, base, href)?;
    if let Ok(decoded) = percent_decode_str(href).decode_utf8() {
        resolve_inside(root, base, &decoded)?;
    }
    Ok(())
}

fn resolve_inside(root: &Path, base: &Path, name: &str) -> Result<std::path::PathBuf> {
    let mut path = base.to_path_buf();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir if path != root => {
                path.pop();
            }
            _ => {
                return Err(Error::InvalidEpub(format!(
                    "path outside the archive: {name}"
                )))
            }
        }
    }
    Ok(path)
}
