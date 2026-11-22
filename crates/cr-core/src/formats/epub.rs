//! EPUB support: metadata/cover extraction, spine-ordered text reading, and
//! unpack/repack for the `edit` command.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

use crate::error::{Error, Result};
use crate::formats::{decode_markup, html, BookText};
use crate::metadata::Metadata;
use crate::opf;
use crate::xml;

#[derive(Debug, Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Debug)]
struct Package {
    metadata: Metadata,
    cover: Option<Vec<u8>>,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
}

/// Read metadata and the cover image from an EPUB.
pub fn metadata(path: &Path) -> Result<(Metadata, Option<Vec<u8>>)> {
    let package = read_package(path)?;
    Ok((package.metadata, package.cover))
}

/// Read an EPUB in spine order, converting each XHTML document to text.
pub fn text(path: &Path) -> Result<BookText> {
    let package = read_package(path)?;
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut chapters = Vec::new();

    for idref in &package.spine {
        let Some(item) = package.manifest.get(idref) else {
            continue;
        };
        if !is_html_media_type(&item.media_type) {
            continue;
        }
        let data = match read_entry(&mut archive, &item.href) {
            Ok(data) => data,
            Err(_) => continue,
        };
        let markup = decode_markup(&data);
        if let Some(chapter) = html::chapter(&markup, chapters.len() + 1) {
            chapters.push(chapter);
        }
    }

    Ok(BookText { chapters })
}

/// Unpack an EPUB into a directory for editing.
pub fn unpack(path: &Path, destination: &Path) -> Result<usize> {
    fs::create_dir_all(destination)?;
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut count = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else {
            // Zip-slip attempt or otherwise unsafe name: skip it.
            continue;
        };
        let output = destination.join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&output)?;
        io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    Ok(count)
}

/// Repack an edited EPUB directory. `mimetype` is written first and stored
/// uncompressed, as the EPUB spec requires.
pub fn repack(directory: &Path, output: &Path) -> Result<usize> {
    if !directory.is_dir() {
        return Err(Error::Other(format!(
            "not a directory: {}",
            directory.display()
        )));
    }
    let mimetype_path = directory.join("mimetype");
    let file = File::create(output)?;
    let mut writer = ZipWriter::new(file);
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    let deflated = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    writer.start_file("mimetype", stored)?;
    if mimetype_path.is_file() {
        let mimetype = fs::read(&mimetype_path)?;
        writer.write_all(&mimetype)?;
    } else {
        writer.write_all(b"application/epub+zip")?;
    }
    let mut count = 1usize;

    let walker = walkdir::WalkDir::new(directory)
        .sort_by_file_name()
        .into_iter()
        .filter_map(std::result::Result::ok);
    for entry in walker {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path == mimetype_path || path == output {
            continue;
        }
        let relative = path
            .strip_prefix(directory)
            .map_err(|_| Error::Other("failed to build relative path".into()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() {
            continue;
        }
        writer.start_file(&relative, deflated)?;
        let mut input = File::open(path)?;
        io::copy(&mut input, &mut writer)?;
        count += 1;
    }
    writer.finish()?;
    Ok(count)
}

fn read_package(path: &Path) -> Result<Package> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;

    let container_data = read_entry(&mut archive, "META-INF/container.xml")?;
    let opf_path = package_path(&container_data)?;
    let opf_data = read_entry(&mut archive, &opf_path)
        .map_err(|_| Error::InvalidEpub(format!("missing OPF at {opf_path}")))?;

    let mut metadata = opf::parse_opf(&opf_data)?;
    let opf_xml = xml::parse(&opf_data)?;
    let opf_dir = opf_path
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default();

    let mut manifest = HashMap::new();
    if let Some(manifest_node) = opf_xml.child("manifest") {
        for item in manifest_node.children.iter().filter(|c| c.name == "item") {
            let Some(id) = item.attr("id") else { continue };
            let Some(href) = item.attr("href") else {
                continue;
            };
            manifest.insert(
                id.to_string(),
                ManifestItem {
                    href: resolve_href(&opf_dir, href),
                    media_type: item.attr("media-type").unwrap_or_default().to_string(),
                    properties: item.attr("properties").unwrap_or_default().to_string(),
                },
            );
        }
    }

    let mut spine = Vec::new();
    if let Some(spine_node) = opf_xml.child("spine") {
        for itemref in spine_node.children.iter().filter(|c| c.name == "itemref") {
            if itemref
                .attr("linear")
                .is_some_and(|v| v.eq_ignore_ascii_case("no"))
            {
                continue;
            }
            if let Some(idref) = itemref.attr("idref") {
                spine.push(idref.to_string());
            }
        }
    }

    // Locate the cover: EPUB3 `properties="cover-image"` first, then the
    // EPUB2 `<meta name="cover" content="id">` convention.
    let mut cover_href = manifest
        .values()
        .find(|item| {
            item.properties
                .split_whitespace()
                .any(|p| p == "cover-image")
        })
        .map(|item| item.href.clone());
    if cover_href.is_none() {
        if let Some(metadata_node) = opf_xml.child("metadata") {
            let cover_id = metadata_node
                .children
                .iter()
                .filter(|c| c.name == "meta")
                .find(|c| c.attr("name") == Some("cover"))
                .and_then(|c| c.attr("content"));
            if let Some(cover_id) = cover_id {
                cover_href = manifest.get(cover_id).map(|item| item.href.clone());
            }
        }
    }
    if cover_href.is_none() {
        cover_href = manifest
            .values()
            .find(|item| {
                item.media_type.starts_with("image/")
                    && (item.href.to_ascii_lowercase().contains("cover")
                        || item.href.to_ascii_lowercase().contains("titlepage"))
            })
            .map(|item| item.href.clone());
    }

    let cover = cover_href.and_then(|href| read_entry(&mut archive, &href).ok());

    if metadata.format.is_none() {
        metadata.format = Some("EPUB".to_string());
    }
    Ok(Package {
        metadata,
        cover,
        manifest,
        spine,
    })
}

pub(super) fn package_path(container: &[u8]) -> Result<String> {
    let container = xml::parse(container)?;
    let rootfile = container
        .descendant("rootfile")
        .ok_or_else(|| Error::InvalidEpub("container.xml has no rootfile".into()))?;
    rootfile
        .attr("full-path")
        .map(|path| path.trim_start_matches('/').to_string())
        .ok_or_else(|| Error::InvalidEpub("rootfile has no full-path".into()))
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| Error::InvalidEpub(format!("missing entry: {name}")))?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data)?;
    Ok(data)
}

fn is_html_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/xhtml+xml" | "text/html" | "application/xml" | "text/xml" | ""
    )
}

/// Resolve an OPF-relative href to an archive path.
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let without_fragment = href.split('#').next().unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(without_fragment)
        .decode_utf8_lossy()
        .replace('\\', "/");
    let mut parts: Vec<String> = if decoded.starts_with('/') {
        Vec::new()
    } else {
        base_dir
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}

/// Convert a path to a `file://` URL relative to an OPF directory (used when
/// rewriting container-relative links, kept for future editing support).
pub fn href_from_path(base_dir: &str, path: &str) -> String {
    let base = PathBuf::from(base_dir);
    let target = PathBuf::from(path);
    target
        .strip_prefix(&base)
        .unwrap_or(&target)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_hrefs() {
        assert_eq!(
            resolve_href("OEBPS", "text/ch1.xhtml"),
            "OEBPS/text/ch1.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/text", "../images/cover.jpg"),
            "OEBPS/images/cover.jpg"
        );
        assert_eq!(resolve_href("", "/a/b.xhtml"), "a/b.xhtml");
        assert_eq!(
            resolve_href("OEBPS", "chapter%201.xhtml#frag"),
            "OEBPS/chapter 1.xhtml"
        );
    }

    #[test]
    fn detects_html_media_types() {
        assert!(is_html_media_type("application/xhtml+xml"));
        assert!(is_html_media_type("text/html"));
        assert!(!is_html_media_type("image/jpeg"));
    }
}
