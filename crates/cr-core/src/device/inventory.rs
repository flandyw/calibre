//! What is on a device: its books matched to the library, removing them,
//! and the reading progress and highlights the device records.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::{read_catalog, write_catalog, Device, DeviceKind, Transport};
use crate::error::{Error, Result};
use crate::library::Library;
use crate::text;

const BOOK_EXTENSIONS: [&str; 15] = [
    "epub", "kepub", "azw3", "azw", "mobi", "prc", "kfx", "pdf", "txt", "fb2", "rtf", "docx",
    "htmlz", "cbz", "cbr",
];

pub(super) fn is_book_file(name: &str) -> bool {
    !name.starts_with('.')
        && name.rsplit_once('.').is_some_and(|(_, extension)| {
            BOOK_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBookState {
    /// The device copy is the library's file.
    InSync,
    /// The library's file of this format differs from the device copy.
    Changed,
    /// The library has the book but not in this format (a converted copy).
    OtherFormat,
    NotInLibrary,
}

impl DeviceBookState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InSync => "in sync",
            Self::Changed => "changed",
            Self::OtherFormat => "other format",
            Self::NotInLibrary => "not in library",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBook {
    /// Path on the device, relative to its root, with `/` separators.
    pub path: String,
    pub title: String,
    pub format: String,
    pub bytes: u64,
    pub book_id: Option<i64>,
    pub state: DeviceBookState,
}

/// The file name without extension and without a ` (2)` collision suffix.
fn stem_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    match stem.rsplit_once(" (") {
        Some((base, counter))
            if counter.ends_with(')') && counter[..counter.len() - 1].parse::<u32>().is_ok() =>
        {
            base.to_string()
        }
        _ => stem.to_string(),
    }
}

fn walk(directory: &Path, root: &Path, found: &mut Vec<(String, u64)>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Hidden and system folders, and Kindle sidecar folders.
        if name.starts_with('.') || name.ends_with(".sdr") {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !(directory == root && name.eq_ignore_ascii_case("system")) {
                walk(&path, root, found);
            }
        } else if kind.is_file() && is_book_file(&name) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            found.push((relative, bytes));
        }
    }
}

fn device_files(device: &Device) -> Result<Vec<(String, u64)>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Transport::Mtp(kindle) = &device.transport {
        return super::mtp::books(device, kindle);
    }
    let Transport::Mounted = &device.transport else {
        unreachable!("every other transport returned above");
    };
    if !device.root.is_dir() {
        return Err(Error::Device(format!(
            "{} is no longer connected.",
            device.name
        )));
    }
    let mut found = Vec::new();
    walk(&device.root, &device.root, &mut found);
    Ok(found)
}

/// List the books on a device and match them to the library: by the uuid
/// recorded when a book was sent, otherwise by the file name `send` gives it.
pub fn books(library: Option<&Library>, device: &Device) -> Result<Vec<DeviceBook>> {
    let mut files = device_files(device)?;
    files.sort();
    let catalog: HashMap<String, Value> = if device.is_mtp() {
        HashMap::new()
    } else {
        read_catalog(&device.catalog_path())
            .into_iter()
            .filter_map(|entry| Some((entry.get("lpath")?.as_str()?.to_string(), entry)))
            .collect()
    };
    let identities = match library {
        Some(library) => library.book_identities()?,
        None => Vec::new(),
    };
    let by_uuid: HashMap<&str, i64> = identities
        .iter()
        .filter(|(_, uuid, _, _)| !uuid.is_empty())
        .map(|(id, uuid, _, _)| (uuid.as_str(), *id))
        .collect();
    let mut by_stem: HashMap<String, Option<i64>> = HashMap::new();
    for (id, _, title, author) in &identities {
        // Two books with the same name cannot be told apart by name.
        by_stem
            .entry(text::format_file_stem(title, author).to_lowercase())
            .and_modify(|existing| *existing = None)
            .or_insert(Some(*id));
    }
    let titles: HashMap<i64, &str> = identities
        .iter()
        .map(|(id, _, title, _)| (*id, title.as_str()))
        .collect();

    let mut books = Vec::new();
    for (path, bytes) in files {
        let entry = catalog.get(&path);
        let stem = stem_of(&path);
        let book_id = entry
            .and_then(|entry| entry.get("uuid")?.as_str())
            .and_then(|uuid| by_uuid.get(uuid).copied())
            .or_else(|| by_stem.get(&stem.to_lowercase()).copied().flatten());
        let format = path
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_uppercase())
            .unwrap_or_default();
        let state = match (book_id, library) {
            (Some(id), Some(library)) => match library.format_path(id, &format)? {
                Some(file) if fs::metadata(&file).map(|m| m.len()).ok() == Some(bytes) => {
                    DeviceBookState::InSync
                }
                Some(_) => DeviceBookState::Changed,
                None => DeviceBookState::OtherFormat,
            },
            _ => DeviceBookState::NotInLibrary,
        };
        let title = book_id
            .and_then(|id| titles.get(&id).map(|title| title.to_string()))
            .or_else(|| Some(entry?.get("title")?.as_str()?.to_string()))
            .unwrap_or(stem);
        books.push(DeviceBook {
            path,
            title,
            format,
            bytes,
            book_id,
            state,
        });
    }
    Ok(books)
}

/// Delete books from a device by the paths [`books`] reports. Returns the
/// paths that were deleted; a path that is not a book on the device is an error.
pub fn remove(device: &Device, paths: &[String]) -> Result<Vec<String>> {
    let present: Vec<String> = device_files(device)?
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    if let Some(missing) = paths.iter().find(|path| !present.contains(path)) {
        return Err(Error::Device(format!(
            "{missing} is not a book on {}",
            device.name
        )));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Transport::Mtp(kindle) = &device.transport {
        return super::mtp::remove(device, kindle, paths);
    }
    let mut catalog = read_catalog(&device.catalog_path());
    let listed = catalog.len();
    for path in paths {
        let file = device.root.join(path);
        fs::remove_file(&file)?;
        // The Kindle keeps a book's position and notes in a folder beside it.
        let sidecar = file.with_extension("sdr");
        if sidecar.is_dir() {
            let _ = fs::remove_dir_all(sidecar);
        }
        // Folders a file name template made, once they are empty.
        let mut parent = file.parent();
        while let Some(folder) = parent {
            if folder == device.root
                || folder == device.documents_dir()
                || fs::remove_dir(folder).is_err()
            {
                break;
            }
            parent = folder.parent();
        }
        catalog.retain(|entry| entry.get("lpath").and_then(Value::as_str) != Some(path.as_str()));
    }
    if catalog.len() != listed {
        write_catalog(device, &catalog)?;
    }
    Ok(paths.to_vec())
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceAnnotation {
    pub text: String,
    pub note: Option<String>,
    pub location: Option<String>,
    pub created: Option<String>,
}

/// What a device recorded about reading one book.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceReading {
    pub title: String,
    pub author: Option<String>,
    /// 0 to 100, where the device records it.
    pub percent: Option<f64>,
    pub finished: bool,
    pub last_read: Option<String>,
    pub annotations: Vec<DeviceAnnotation>,
}

/// Reading progress and highlights kept by the device itself: Kobo's
/// database, or a Kindle's `My Clippings.txt` (highlights and notes only).
/// The device is only read.
pub fn reading(device: &Device) -> Result<Vec<DeviceReading>> {
    if device.is_mtp() {
        return Err(Error::Device(
            "A Kindle connected over MTP does not expose its highlights or reading position."
                .to_string(),
        ));
    }
    match device.kind {
        DeviceKind::Kobo => kobo_reading(&device.root.join(".kobo/KoboReader.sqlite")),
        DeviceKind::Kindle => {
            let clippings = device.documents_dir().join("My Clippings.txt");
            match fs::read(&clippings) {
                Ok(data) => Ok(kindle_clippings(&String::from_utf8_lossy(&data))),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(err) => Err(err.into()),
            }
        }
        DeviceKind::Generic => Err(Error::Device(format!(
            "{} does not record reading progress in a format that can be read.",
            device.name
        ))),
    }
}

fn kobo_reading(database: &Path) -> Result<Vec<DeviceReading>> {
    if !database.is_file() {
        return Err(Error::Device(
            "No Kobo database on the device (.kobo/KoboReader.sqlite).".to_string(),
        ));
    }
    // Work on a copy: opening the database in place can write to the device.
    let scratch = tempfile::tempdir()?;
    let copy = scratch.path().join("KoboReader.sqlite");
    fs::copy(database, &copy)?;
    let connection = rusqlite::Connection::open(&copy)?;
    let mut order = Vec::new();
    let mut books: HashMap<String, DeviceReading> = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT ContentID, Title, Attribution, ___PercentRead, ReadStatus, DateLastRead
             FROM content WHERE ContentType = '6' ORDER BY Title",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, title, author, percent, status, last_read) = row?;
            let finished = status == Some(2);
            order.push(id.clone());
            books.insert(
                id,
                DeviceReading {
                    title: title.unwrap_or_default(),
                    author: author.filter(|author| !author.is_empty()),
                    percent: if finished { Some(100.0) } else { percent },
                    finished,
                    last_read,
                    annotations: Vec::new(),
                },
            );
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT VolumeID, Text, Annotation, DateCreated FROM Bookmark
             WHERE COALESCE(Text, '') <> '' OR COALESCE(Annotation, '') <> ''
             ORDER BY DateCreated",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (volume, text, note, created) = row?;
            if let Some(book) = books.get_mut(&volume) {
                book.annotations.push(DeviceAnnotation {
                    text: text.unwrap_or_default().trim().to_string(),
                    note: note.filter(|note| !note.trim().is_empty()),
                    location: None,
                    created,
                });
            }
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|id| books.remove(&id))
        .filter(|book| {
            book.finished
                || book.percent.is_some_and(|percent| percent > 0.0)
                || !book.annotations.is_empty()
        })
        .collect())
}

/// Parse a Kindle's `My Clippings.txt`.
fn kindle_clippings(text: &str) -> Vec<DeviceReading> {
    let mut books: Vec<DeviceReading> = Vec::new();
    for entry in text.trim_start_matches('\u{feff}').split("==========") {
        let mut lines = entry.lines().map(str::trim).filter(|line| !line.is_empty());
        let (Some(heading), Some(details)) = (lines.next(), lines.next()) else {
            continue;
        };
        let heading = heading.trim_start_matches('\u{feff}');
        let body = lines.collect::<Vec<_>>().join("\n");
        let is_note = details.contains("Your Note");
        if body.is_empty() || !(is_note || details.contains("Your Highlight")) {
            continue;
        }
        // "Title (Author)"; the author is the last parenthesis.
        let (title, author) = match heading.rsplit_once(" (") {
            Some((title, author)) if author.ends_with(')') => (
                title.to_string(),
                Some(author[..author.len() - 1].to_string()),
            ),
            _ => (heading.to_string(), None),
        };
        let mut fields = details.trim_start_matches("- ").split(" | ");
        let location = fields.next().map(|place| {
            place
                .trim_start_matches("Your Highlight ")
                .trim_start_matches("Your Note ")
                .trim_start_matches("on ")
                .trim_start_matches("at ")
                .to_string()
        });
        let created = details
            .rsplit_once("Added on ")
            .map(|(_, date)| date.to_string());
        let annotation = DeviceAnnotation {
            text: if is_note { String::new() } else { body.clone() },
            note: is_note.then_some(body),
            location,
            created,
        };
        match books
            .iter_mut()
            .find(|book| book.title == title && book.author == author)
        {
            Some(book) => book.annotations.push(annotation),
            None => books.push(DeviceReading {
                title,
                author,
                percent: None,
                finished: false,
                last_read: None,
                annotations: vec![annotation],
            }),
        }
    }
    books
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_drop_extension_and_collision_suffix() {
        assert_eq!(stem_of("documents/A Book - Ann (2).azw3"), "A Book - Ann");
        assert_eq!(stem_of("A Book (Annotated).epub"), "A Book (Annotated)");
        assert!(is_book_file("x.EPUB"));
        assert!(!is_book_file(".x.epub.part"));
        assert!(!is_book_file("notes.json"));
    }

    #[test]
    fn parses_kindle_clippings() {
        let text = "\u{feff}Dune (Frank Herbert)\r\n- Your Highlight on page 12 | Location 180-181 | Added on Monday, 2 March 2026 10:00:00\r\n\r\nFear is the mind-killer.\r\n==========\r\nDune (Frank Herbert)\r\n- Your Note on page 12 | Location 181 | Added on Monday, 2 March 2026 10:01:00\r\n\r\nthe litany\r\n==========\r\nDune (Frank Herbert)\r\n- Your Bookmark on page 20 | Added on Monday, 2 March 2026\r\n\r\n\r\n==========\r\nUntitled\r\n- Your Highlight at Location 5 | Added on Tuesday\r\n\r\nx\r\n==========\r\n";
        let books = kindle_clippings(text);
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].title, "Dune");
        assert_eq!(books[0].author.as_deref(), Some("Frank Herbert"));
        assert_eq!(books[0].annotations.len(), 2);
        assert_eq!(books[0].annotations[0].text, "Fear is the mind-killer.");
        assert_eq!(books[0].annotations[0].location.as_deref(), Some("page 12"));
        assert_eq!(books[0].annotations[1].note.as_deref(), Some("the litany"));
        assert_eq!(books[1].author, None);
        assert_eq!(
            books[1].annotations[0].location.as_deref(),
            Some("Location 5")
        );
    }
}
