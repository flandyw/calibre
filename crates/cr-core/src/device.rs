//! Discover USB e-readers and send books to them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::jobs::JobContext;
use crate::library::Library;
use crate::metadata::{format_datetime, Metadata};
use crate::text;

mod inventory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod mtp;

pub use inventory::*;

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
    /// Skip a book with no format the device reads instead of converting it.
    pub no_convert: bool,
    /// File name on the device, without extension: `{title}`, `{author}`,
    /// `{series}`, `{series_index}` and `{id}`, with `/` between folders.
    /// The default is `{title} - {author}`.
    pub template: Option<String>,
    /// Progress and cancellation between books.
    pub job: JobContext,
}

#[derive(Debug, Clone)]
pub struct SentBook {
    pub id: i64,
    pub title: String,
    pub format: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// The library format this copy was converted from, if it was.
    pub converted_from: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SendReport {
    pub sent: Vec<SentBook>,
    pub skipped: Vec<(i64, String)>,
    pub catalog: Option<PathBuf>,
    pub dry_run: bool,
    /// The send was cancelled before every book was copied.
    pub cancelled: bool,
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

    // Convert and size everything first, so a full device is noticed before
    // anything is written to it.
    let mut books = Vec::new();
    for (index, &id) in ids.iter().enumerate() {
        if options.job.is_cancelled() {
            report.cancelled = true;
            break;
        }
        options.job.report(index, ids.len() * 2, "Preparing");
        if let Some(book) = prepare_book(library, device, id, options, &mut report)? {
            books.push((id, book));
        }
    }
    let needed: u64 = books
        .iter()
        .map(|(_, book)| fs::metadata(&book.source).map(|m| m.len()).unwrap_or(0))
        .sum();
    if let Some(free) = free_space(&device.root).filter(|_| !options.dry_run) {
        if needed > free {
            return Err(Error::Device(format!(
                "{} needs {needed} bytes but has {free} free. Nothing was sent.",
                device.name
            )));
        }
    }

    let total = books.len();
    let mut failure = None;
    for (index, (id, book)) in books.into_iter().enumerate() {
        if options.job.is_cancelled() {
            report.cancelled = true;
            break;
        }
        options
            .job
            .report(total + index, total * 2, book.meta.title.clone());
        let folder = book
            .folders
            .iter()
            .fold(documents.clone(), |path, folder| path.join(folder));
        let mut target = folder.join(book.file_name(1, None));
        let mut counter = 2;
        while target.exists() && !options.dry_run {
            target = folder.join(book.file_name(counter, None));
            counter += 1;
        }

        let PreparedBook {
            source,
            meta,
            format,
            converted_from,
            ..
        } = book;
        let size = fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
        if !options.dry_run {
            // The books already copied stay listed if the device goes away.
            if let Err(err) = copy_verified(&source, &target, size) {
                failure = Some(Error::Device(format!(
                    "Could not copy {} to {}: {err}. Check that the device is still connected.",
                    meta.title, device.name
                )));
                break;
            }
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
            converted_from,
        });
    }

    if !options.dry_run && !report.sent.is_empty() {
        write_catalog(device, &catalog)?;
    }
    match failure {
        Some(err) => Err(err),
        None => Ok(report),
    }
}

fn write_catalog(device: &Device, catalog: &[Value]) -> Result<()> {
    let encoded = serde_json::to_string_pretty(catalog)?;
    let partial = device.root.join(".metadata.calibre.part");
    fs::write(&partial, encoded.as_bytes())?;
    Ok(fs::rename(partial, device.catalog_path())?)
}

/// Copy under a temporary name and rename, so an unplugged device is never
/// left with a truncated book under its final name.
fn copy_verified(source: &Path, target: &Path, size: u64) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let partial = target.with_file_name(format!(".{name}.part"));
    let copied = fs::copy(source, &partial).and_then(|copied| {
        if copied == size && fs::metadata(&partial)?.len() == size {
            fs::rename(&partial, target)
        } else {
            Err(std::io::Error::other("the copy is incomplete"))
        }
    });
    if copied.is_err() {
        let _ = fs::remove_file(&partial);
    }
    copied
}

/// Bytes free on the filesystem holding `path`, where it can be asked.
fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is a valid C string and `stats` is writable.
        if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: statvfs succeeded, so it filled `stats`.
        let stats = unsafe { stats.assume_init() };
        Some(stats.f_bavail as u64 * stats.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

struct PreparedBook {
    source: PathBuf,
    meta: Metadata,
    format: String,
    stem: String,
    /// Folders under the documents folder, from the file name template.
    folders: Vec<String>,
    converted_from: Option<String>,
    /// Holds the converted file until it is copied.
    _scratch: Option<tempfile::TempDir>,
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
    let kindle = device.kind == DeviceKind::Kindle;
    let requested = options.format.as_deref();
    let meta = library.get_metadata(id)?;
    let (folders, stem) = file_stem(options.template.as_deref(), &meta, id);
    let format = match select_format(&available, requested, kindle) {
        Ok(format) => format,
        Err(reason) => {
            // Nothing the device reads: convert, unless that was declined
            // or the format asked for is one the device cannot read at all.
            let target = requested.map_or_else(
                || if kindle { "AZW3" } else { "EPUB" }.to_string(),
                str::to_ascii_uppercase,
            );
            let source = (!options.no_convert
                && (!kindle || KINDLE_FORMATS.contains(&target.as_str()))
                && crate::formats::OUTPUT_FORMATS.contains(&target.as_str()))
            .then(|| {
                CONVERT_FROM
                    .iter()
                    .filter(|format| **format != target)
                    .find_map(|wanted| available.iter().find(|f| f.eq_ignore_ascii_case(wanted)))
            })
            .flatten();
            let Some(from) = source else {
                return skip(reason);
            };
            let Some(original) = library.format_path(id, from)? else {
                return skip(format!("{from} file missing from library"));
            };
            let scratch = tempfile::tempdir()?;
            let converted = scratch
                .path()
                .join(format!("book.{}", target.to_ascii_lowercase()));
            // Only a conversion that reads back as a book is sent.
            let checked = crate::formats::convert(&original, &converted)
                .and_then(|()| crate::formats::read_document(&converted, false));
            return match checked {
                Ok(document) if !document.sections.is_empty() => Ok(Some(PreparedBook {
                    source: converted,
                    meta,
                    format: target,
                    stem,
                    folders,
                    converted_from: Some(from.clone()),
                    _scratch: Some(scratch),
                })),
                Ok(_) => skip(format!(
                    "converting {from} to {target} produced an empty book"
                )),
                Err(err) => skip(format!("could not convert {from} to {target}: {err}")),
            };
        }
    };
    let Some(source) = library.format_path(id, &format)? else {
        return skip(format!("{format} file missing from library"));
    };
    Ok(Some(PreparedBook {
        source,
        meta,
        format,
        stem,
        folders,
        converted_from: None,
        _scratch: None,
    }))
}

const KINDLE_FORMATS: [&str; 6] = ["AZW3", "AZW", "MOBI", "PRC", "PDF", "TXT"];
/// Library formats a device copy can be converted from, best first.
const CONVERT_FROM: [&str; 10] = [
    "EPUB", "AZW3", "MOBI", "AZW", "DOCX", "FB2", "HTMLZ", "HTML", "RTF", "TXT",
];

/// Folders and file stem for a book on the device.
fn file_stem(template: Option<&str>, meta: &Metadata, id: i64) -> (Vec<String>, String) {
    let Some(template) = template else {
        return (
            Vec::new(),
            text::format_file_stem(&meta.title, meta.first_author()),
        );
    };
    let index = if meta.series.is_none() {
        String::new()
    } else if meta.series_index.fract() == 0.0 {
        format!("{:.0}", meta.series_index)
    } else {
        meta.series_index.to_string()
    };
    let mut parts: Vec<String> = template
        .split('/')
        .map(|part| {
            let filled = part
                .replace("{title}", &meta.title)
                .replace("{author}", meta.first_author())
                .replace("{series}", meta.series.as_deref().unwrap_or_default())
                .replace("{series_index}", &index)
                .replace("{id}", &id.to_string());
            text::sanitize_component(filled.trim())
        })
        .filter(|part| !part.is_empty() && part != "." && part != "..")
        .collect();
    let stem = parts
        .pop()
        .unwrap_or_else(|| text::format_file_stem(&meta.title, meta.first_author()));
    (parts, stem)
}

fn select_format(
    available: &[String],
    requested: Option<&str>,
    mtp_kindle: bool,
) -> std::result::Result<String, String> {
    if let Some(requested) = requested {
        let wanted = requested.to_ascii_uppercase();
        if mtp_kindle && !KINDLE_FORMATS.contains(&wanted.as_str()) {
            return Err(format!(
                "{wanted} cannot be read on a Kindle. Use AZW3 or PDF."
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
                    "No Kindle-compatible format (available: {}). Add an AZW3 or PDF copy.",
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
