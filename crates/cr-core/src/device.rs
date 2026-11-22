//! Discover USB e-readers and send books to them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::library::Library;
use crate::metadata::{format_datetime, Metadata};
use crate::text;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod mtp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Kindle,
    Kobo,
    Generic,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kindle => "kindle",
            Self::Kobo => "kobo",
            Self::Generic => "generic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Mount point or an `mtp:…` identifier for a USB Kindle.
    pub root: PathBuf,
    pub name: String,
    pub kind: DeviceKind,
    transport: Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Transport {
    Mounted,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Mtp(mtp::Kindle),
}

impl Device {
    /// Detect a device at an explicit path. Used by `cr send --device`.
    pub fn detect_at(root: &Path) -> Option<Self> {
        if !root.is_dir() {
            return None;
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());
        let has_documents = root.join("documents").is_dir();
        let kind = if root.join("system").is_dir() && has_documents {
            DeviceKind::Kindle
        } else if root.join(".kobo").is_dir() || has_kobo_marker(root) {
            DeviceKind::Kobo
        } else if root.join("metadata.calibre").is_file() || has_documents {
            DeviceKind::Generic
        } else {
            return None;
        };
        Some(Self {
            root: root.to_path_buf(),
            name,
            kind,
            transport: Transport::Mounted,
        })
    }

    pub fn is_mtp(&self) -> bool {
        !matches!(self.transport, Transport::Mounted)
    }

    /// Where books should be copied. Kindle and most readers use `documents`.
    pub fn documents_dir(&self) -> PathBuf {
        let documents = self.root.join("documents");
        if documents.is_dir() || matches!(self.kind, DeviceKind::Kindle | DeviceKind::Kobo) {
            documents
        } else {
            self.root.clone()
        }
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.root.join("metadata.calibre")
    }

    fn relative_lpath(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

/// Scan common mount points and USB for connected e-readers.
pub fn discover() -> Result<Vec<Device>> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(roots) = std::env::var_os("CR_DEVICE_ROOTS") {
        candidates.extend(std::env::split_paths(&roots));
    }

    #[cfg(target_os = "linux")]
    {
        for base in ["/media", "/run/media", "/mnt"] {
            let base = PathBuf::from(base);
            let Ok(entries) = fs::read_dir(&base) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                candidates.push(path.clone());
                // /media/<user>/<device> and /run/media/<user>/<device>
                if let Ok(children) = fs::read_dir(&path) {
                    for child in children.flatten() {
                        if child.path().is_dir() {
                            candidates.push(child.path());
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(entries) = fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    candidates.push(entry.path());
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        for letter in b'D'..=b'Z' {
            let root = PathBuf::from(format!("{}:\\", letter as char));
            if root.is_dir() {
                candidates.push(root);
            }
        }
    }

    let mut devices: Vec<Device> = candidates
        .iter()
        .filter_map(|path| Device::detect_at(path))
        .collect();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    devices.extend(mtp::discover()?);
    devices.sort_by(|a, b| a.root.cmp(&b.root));
    devices.dedup_by(|a, b| a.root == b.root);
    Ok(devices)
}

fn has_kobo_marker(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(".kobo")
    })
}

#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub format: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct SentBook {
    pub id: i64,
    pub title: String,
    pub format: String,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SendReport {
    pub sent: Vec<SentBook>,
    pub skipped: Vec<(i64, String)>,
    pub catalog: Option<PathBuf>,
    pub dry_run: bool,
}

/// Send books to a device. Mounted devices also get a `metadata.calibre` catalog.
pub fn send(
    library: &Library,
    device: &Device,
    ids: &[i64],
    options: &SendOptions,
) -> Result<SendReport> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Transport::Mtp(kindle) = &device.transport {
        return mtp::send(library, device, kindle, ids, options);
    }

    let documents = device.documents_dir();
    if !options.dry_run {
        fs::create_dir_all(&documents)?;
    }

    let mut report = SendReport {
        catalog: Some(device.catalog_path()),
        dry_run: options.dry_run,
        ..Default::default()
    };
    let mut catalog = read_catalog(&device.catalog_path());

    for &id in ids {
        let Some(book) = prepare_book(library, device, id, options, &mut report)? else {
            continue;
        };
        let mut target = documents.join(book.file_name(1, None));
        let mut counter = 2;
        while target.exists() && !options.dry_run {
            target = documents.join(book.file_name(counter, None));
            counter += 1;
        }

        let PreparedBook {
            source,
            meta,
            format,
            ..
        } = book;
        let size = fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
        if !options.dry_run {
            fs::copy(&source, &target)?;
        }
        let lpath = device.relative_lpath(&target);
        let mtime = fs::metadata(&source)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let entry = json!({
            "lpath": lpath,
            "title": meta.title,
            "authors": meta.authors,
            "author_sort": meta.computed_author_sort(),
            "series": meta.series,
            "series_index": meta.series_index,
            "tags": meta.tags,
            "rating": meta.rating,
            "languages": meta.languages,
            "publisher": meta.publisher,
            "comments": meta.comments,
            "identifiers": meta.identifiers,
            "uuid": meta.uuid,
            "application_id": meta.uuid,
            "pubdate": meta.pubdate.as_ref().map(format_datetime),
            "timestamp": meta.timestamp.as_ref().map(format_datetime),
            "last_modified": meta.last_modified.as_ref().map(format_datetime),
            "size": size,
            "format": format,
            "mtime": mtime,
        });
        if let Some(position) = catalog.iter().position(|existing| {
            existing.get("lpath").and_then(Value::as_str) == Some(lpath.as_str())
        }) {
            catalog[position] = entry;
        } else {
            catalog.push(entry);
        }

        report.sent.push(SentBook {
            id,
            title: meta.title,
            format,
            path: target,
            bytes: size,
        });
    }

    if !options.dry_run {
        let encoded = serde_json::to_string_pretty(&catalog)?;
        fs::write(&device.catalog_path(), encoded.as_bytes())?;
    }
    Ok(report)
}

struct PreparedBook {
    source: PathBuf,
    meta: Metadata,
    format: String,
    stem: String,
}

impl PreparedBook {
    fn file_name(&self, counter: usize, max_stem_bytes: Option<usize>) -> String {
        let suffix = if counter == 1 {
            String::new()
        } else {
            format!(" ({counter})")
        };
        let mut stem = self.stem.as_str();
        if let Some(max) = max_stem_bytes {
            let mut end = stem.len().min(max);
            while !stem.is_char_boundary(end) {
                end -= 1;
            }
            stem = &stem[..end];
        }
        let extension = self.format.to_ascii_lowercase();
        let mut name = text::sanitize_component(&format!("{stem}{suffix}.{extension}"));
        if name.starts_with('.') {
            name.insert(0, '_');
        }
        name
    }
}

fn prepare_book(
    library: &Library,
    device: &Device,
    id: i64,
    options: &SendOptions,
    report: &mut SendReport,
) -> Result<Option<PreparedBook>> {
    let mut skip = |reason| {
        report.skipped.push((id, reason));
        Ok(None)
    };
    let available = match library.formats(id) {
        Ok(formats) => formats,
        Err(err) => return skip(err.to_string()),
    };
    let format = match select_format(&available, options.format.as_deref(), device.is_mtp()) {
        Ok(format) => format,
        Err(reason) => return skip(reason),
    };
    let Some(source) = library.format_path(id, &format)? else {
        return skip(format!("{format} file missing from library"));
    };
    let meta = library.get_metadata(id)?;
    let stem = text::format_file_stem(&meta.title, meta.first_author());
    Ok(Some(PreparedBook {
        source,
        meta,
        format,
        stem,
    }))
}

fn select_format(
    available: &[String],
    requested: Option<&str>,
    mtp_kindle: bool,
) -> std::result::Result<String, String> {
    const KINDLE_FORMATS: [&str; 6] = ["AZW3", "AZW", "MOBI", "PRC", "PDF", "TXT"];
    if let Some(requested) = requested {
        let wanted = requested.to_ascii_uppercase();
        if mtp_kindle && !KINDLE_FORMATS.contains(&wanted.as_str()) {
            return Err(format!(
                "{wanted} cannot be read on a Kindle over USB. Use AZW3 or PDF."
            ));
        }
        return available
            .iter()
            .find(|format| format.eq_ignore_ascii_case(&wanted))
            .cloned()
            .ok_or_else(|| format!("no {wanted} file (available: {})", available.join(", ")));
    }
    if mtp_kindle {
        return KINDLE_FORMATS
            .iter()
            .find_map(|wanted| available.iter().find(|f| f.eq_ignore_ascii_case(wanted)))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "No Kindle-compatible format for USB transfer (available: {}). Add an AZW3 or PDF copy.",
                    available.join(", ")
                )
            });
    }
    preferred_format(available).ok_or_else(|| "no formats".to_string())
}

fn read_catalog(path: &Path) -> Vec<Value> {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn preferred_format(available: &[String]) -> Option<String> {
    const PREFERRED: [&str; 7] = ["EPUB", "AZW3", "MOBI", "PDF", "HTML", "TXT", "FB2"];
    for wanted in PREFERRED {
        if let Some(found) = available.iter().find(|f| f.eq_ignore_ascii_case(wanted)) {
            return Some(found.clone());
        }
    }
    available.first().cloned()
}

/// Ensure a path is inside the device root (defence in depth for send paths).
pub fn ensure_inside_device(device: &Device, path: &Path) -> Result<()> {
    if path.starts_with(&device.root) {
        Ok(())
    } else {
        Err(Error::Device(format!(
            "{} is outside device {}",
            path.display(),
            device.root.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kindle_layout() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("system")).unwrap();
        fs::create_dir_all(dir.path().join("documents")).unwrap();
        let device = Device::detect_at(dir.path()).unwrap();
        assert_eq!(device.kind, DeviceKind::Kindle);
        assert_eq!(device.documents_dir(), dir.path().join("documents"));
    }

    #[test]
    fn rejects_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Device::detect_at(dir.path()).is_none());
    }

    #[test]
    fn prefers_epub() {
        let formats = vec!["MOBI".to_string(), "EPUB".to_string()];
        assert_eq!(preferred_format(&formats).as_deref(), Some("EPUB"));
    }

    #[test]
    fn mtp_kindle_selects_a_format_it_can_read_over_usb() {
        let formats = ["EPUB".to_string(), "AZW3".to_string(), "PDF".to_string()];
        assert_eq!(select_format(&formats, None, true).unwrap(), "AZW3");
        assert_eq!(select_format(&formats, Some("pdf"), true).unwrap(), "PDF");
        assert!(select_format(&formats, Some("EPUB"), true).is_err());
        assert!(select_format(&formats[..1], None, true).is_err());
    }
}
