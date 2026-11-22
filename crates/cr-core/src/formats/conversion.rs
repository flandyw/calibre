//! Atomic file conversion between EPUB and the supported Kindle formats.

use std::fs;
use std::io::Write;
use std::path::{Component, Path};
use std::sync::LazyLock;

use kindling::{extracted::ExtractedEpub, opf::OPFData};
use percent_encoding::percent_decode_str;
use regex::Regex;
use tempfile::NamedTempFile;

use super::{detect_format, epub, extract_text, mobi};
use crate::error::{Error, Result};
use crate::text::format_from_path;

/// Convert an EPUB to AZW3 or MOBI, or a Kindle book to EPUB or TXT.
///
/// The output extension selects the target format. An existing file is
/// preserved, and failed conversions leave no output file behind.
pub fn convert(input: &Path, output: &Path) -> Result<()> {
    if !input.is_file() {
        return Err(Error::Other(format!("not a file: {}", input.display())));
    }
    let source = detect_format(input);
    let target = format_from_path(output).unwrap_or_default();
    let kindle_input = matches!(source.as_str(), "AZW3" | "AZW" | "MOBI" | "PRC");
    let kindle_output = source == "EPUB" && matches!(target.as_str(), "AZW3" | "MOBI");
    if !kindle_output && !(kindle_input && matches!(target.as_str(), "EPUB" | "TXT")) {
        return Err(Error::UnsupportedFormat(format!(
            "cannot convert {source} to {target}; use EPUB to AZW3/MOBI or Kindle to EPUB/TXT"
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
    if kindle_output {
        write_kindle(input, temporary.path(), target == "AZW3")?;
    } else if target == "EPUB" {
        mobi::write_epub(input, temporary.as_file_mut())?;
    } else {
        let text = extract_text(input, Some(&source))?;
        for chapter in text.chapters {
            writeln!(temporary, "{}\n\n{}\n", chapter.title, chapter.text)?;
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
