//! Check a library's files against its database, repair what is safe to
//! repair, and rebuild a lost database from the `metadata.opf` files.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::manage::{format_files, DATA_DIR, TRASH_DIR};
use super::{safe_join, Library, NewBook, COVER_FILE, DB_FILE, METADATA_FILE};
use crate::error::{Error, Result};
use crate::opf;
use crate::text;

/// File types recorded as a book format when found untracked in a book folder.
const BOOK_FORMATS: [&str; 20] = [
    "EPUB", "KEPUB", "AZW3", "AZW", "MOBI", "PRC", "KFX", "PDF", "TXT", "FB2", "RTF", "DOCX",
    "HTMLZ", "HTML", "CBZ", "CBR", "DJVU", "LIT", "ODT", "ZIP",
];

/// Something in the library that does not agree with its database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The book's folder is gone.
    MissingFolder { id: i64, path: String },
    /// A recorded format has no file.
    MissingFormat { id: i64, format: String },
    /// The recorded size of a format is not the file's.
    WrongSize { id: i64, format: String, bytes: u64 },
    /// A book file in the folder that the database does not list.
    UntrackedFormat { id: i64, file: String },
    /// The database says there is a cover but `cover.jpg` is missing.
    MissingCover { id: i64 },
    /// `cover.jpg` exists but the database says there is no cover.
    UnrecordedCover { id: i64 },
    /// The folder has no `metadata.opf` backup.
    MissingOpf { id: i64 },
    /// A folder in the library that belongs to no book.
    UnknownFolder { path: String },
}

impl Problem {
    /// Whether [`Library::repair`] fixes it.
    pub fn repairable(&self) -> bool {
        !matches!(
            self,
            Self::MissingFolder { .. } | Self::UnknownFolder { .. }
        )
    }

    pub fn describe(&self) -> String {
        match self {
            Self::MissingFolder { id, path } => format!("#{id}: folder missing ({path})"),
            Self::MissingFormat { id, format } => format!("#{id}: {format} file missing"),
            Self::WrongSize { id, format, bytes } => {
                format!("#{id}: {format} size recorded wrongly (file is {bytes} bytes)")
            }
            Self::UntrackedFormat { id, file } => format!("#{id}: {file} is not in the database"),
            Self::MissingCover { id } => format!("#{id}: cover recorded but cover.jpg missing"),
            Self::UnrecordedCover { id } => format!("#{id}: cover.jpg present but not recorded"),
            Self::MissingOpf { id } => format!("#{id}: metadata.opf missing"),
            Self::UnknownFolder { path } => format!("{path}: folder belongs to no book"),
        }
    }
}

/// What rebuilding a database did.
#[derive(Debug, Clone, Default)]
pub struct RebuildReport {
    pub restored: usize,
    /// Folders that could not be restored, with the reason.
    pub failed: Vec<(String, String)>,
    /// Where the previous database was kept, if there was one.
    pub backup: Option<PathBuf>,
}

fn subdirectories(directory: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().to_string(),
                entry.path(),
            )
        })
        .filter(|(name, _)| !name.starts_with('.') && name != TRASH_DIR)
        .collect();
    found.sort();
    found
}

impl Library {
    /// Compare every book's database record with its folder. Read-only.
    pub fn check(&self) -> Result<Vec<Problem>> {
        let mut problems = Vec::new();
        let mut known = HashSet::new();
        let books: Vec<(i64, String, bool)> = {
            let mut statement = self
                .conn
                .prepare("SELECT id, path, has_cover FROM books ORDER BY id")?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (id, path, has_cover) in books {
            known.insert(path.clone());
            let directory = match safe_join(&self.root, &path) {
                Ok(directory) if directory.is_dir() => directory,
                _ => {
                    problems.push(Problem::MissingFolder { id, path });
                    continue;
                }
            };
            let recorded: Vec<(String, String, i64)> = {
                let mut statement = self.conn.prepare(
                    "SELECT format, name, uncompressed_size FROM data WHERE book=?1 ORDER BY format",
                )?;
                let rows =
                    statement.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let mut tracked = HashSet::new();
            for (format, _, size) in &recorded {
                match self.format_path(id, format)? {
                    Some(file) => {
                        let bytes = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                        if bytes != *size as u64 {
                            problems.push(Problem::WrongSize {
                                id,
                                format: format.clone(),
                                bytes,
                            });
                        }
                        tracked.insert(file);
                    }
                    None => problems.push(Problem::MissingFormat {
                        id,
                        format: format.clone(),
                    }),
                }
            }
            for (format, file) in format_files(&directory) {
                let name = file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let base = format.strip_prefix("ORIGINAL_").unwrap_or(&format);
                if !tracked.contains(&file)
                    && !name.starts_with('.')
                    && BOOK_FORMATS.contains(&base)
                    && !recorded
                        .iter()
                        .any(|(known, _, _)| known.eq_ignore_ascii_case(&format))
                {
                    problems.push(Problem::UntrackedFormat { id, file: name });
                }
            }
            match (has_cover, directory.join(COVER_FILE).is_file()) {
                (true, false) => problems.push(Problem::MissingCover { id }),
                (false, true) => problems.push(Problem::UnrecordedCover { id }),
                _ => {}
            }
            if !directory.join(METADATA_FILE).is_file() {
                problems.push(Problem::MissingOpf { id });
            }
        }
        for (author, author_path) in subdirectories(&self.root) {
            let books = subdirectories(&author_path);
            if books.is_empty()
                && !known
                    .iter()
                    .any(|path| path.starts_with(&format!("{author}/")))
            {
                problems.push(Problem::UnknownFolder { path: author });
                continue;
            }
            for (book, _) in books {
                let path = format!("{author}/{book}");
                if !known.contains(&path) {
                    problems.push(Problem::UnknownFolder { path });
                }
            }
        }
        Ok(problems)
    }

    /// Fix the repairable problems: drop records of missing files, record
    /// untracked book files and covers, correct sizes and rewrite missing
    /// `metadata.opf` files. No book file is deleted. Returns how many were fixed.
    pub fn repair(&mut self, problems: &[Problem]) -> Result<usize> {
        self.writable()?;
        let mut fixed = 0;
        for problem in problems {
            match problem {
                Problem::MissingFormat { id, format } => {
                    // Checked again: the file may have come back since.
                    if self.format_path(*id, format)?.is_none() {
                        self.conn.execute(
                            "DELETE FROM data WHERE book=?1 AND format=?2 COLLATE NOCASE",
                            params![id, format],
                        )?;
                        fixed += 1;
                    }
                }
                Problem::WrongSize { id, format, .. } => {
                    if let Some(file) = self.format_path(*id, format)? {
                        self.conn.execute(
                            "UPDATE data SET uncompressed_size=?1 WHERE book=?2 AND format=?3 COLLATE NOCASE",
                            params![fs::metadata(file)?.len() as i64, id, format],
                        )?;
                        fixed += 1;
                    }
                }
                Problem::UntrackedFormat { id, file } => {
                    let directory = safe_join(&self.root, &self.book_path(*id)?)?;
                    let path = directory.join(file);
                    let (Some(stem), Some(extension)) = (
                        path.file_stem().and_then(|stem| stem.to_str()),
                        path.extension().and_then(|extension| extension.to_str()),
                    ) else {
                        continue;
                    };
                    let format = extension.to_ascii_uppercase();
                    if path.is_file() && !self.formats(*id)?.contains(&format) {
                        self.conn.execute(
                            "INSERT INTO data (book, format, uncompressed_size, name) VALUES (?1, ?2, ?3, ?4)",
                            params![id, format, fs::metadata(&path)?.len() as i64, stem],
                        )?;
                        fixed += 1;
                    }
                }
                Problem::MissingCover { id } | Problem::UnrecordedCover { id } => {
                    let directory = safe_join(&self.root, &self.book_path(*id)?)?;
                    self.conn.execute(
                        "UPDATE books SET has_cover=?1 WHERE id=?2",
                        params![directory.join(COVER_FILE).is_file(), id],
                    )?;
                    fixed += 1;
                }
                Problem::MissingOpf { id } => {
                    self.sync_book(*id)?;
                    fixed += 1;
                }
                Problem::MissingFolder { .. } | Problem::UnknownFolder { .. } => {}
            }
        }
        Ok(fixed)
    }

    /// Rebuild `metadata.db` from the `metadata.opf` in each book folder, for
    /// a library whose database is lost or damaged. Books keep their ids and
    /// uuids. The previous database is kept as `metadata_pre_restore.db`.
    /// Custom columns, saved searches and other settings are not in the OPF
    /// files and are not restored.
    pub fn rebuild(root: &Path) -> Result<(Self, RebuildReport)> {
        let mut folders = Vec::new();
        for (author, author_path) in subdirectories(root) {
            for (book, path) in subdirectories(&author_path) {
                // Book folders are named "Title (id)".
                let id = book
                    .rsplit_once(" (")
                    .and_then(|(_, id)| id.strip_suffix(')')?.parse::<i64>().ok());
                if let Some(id) = id.filter(|_| path.join(METADATA_FILE).is_file()) {
                    folders.push((id, format!("{author}/{book}"), path));
                }
            }
        }
        if folders.is_empty() {
            return Err(Error::InvalidLibrary(format!(
                "{} has no book folders with metadata.opf to rebuild from",
                root.display()
            )));
        }
        folders.sort();
        let mut report = RebuildReport::default();
        let database = root.join(DB_FILE);
        if database.exists() {
            let backup = root.join("metadata_pre_restore.db");
            fs::rename(&database, &backup)?;
            report.backup = Some(backup);
        }
        let mut library = Self::create(root)?;
        let mut used = HashSet::new();
        for (id, relative, folder) in folders {
            if !used.insert(id) {
                report
                    .failed
                    .push((relative, format!("another folder already has id {id}")));
                continue;
            }
            match library.restore_folder(id, &relative, &folder) {
                Ok(()) => report.restored += 1,
                Err(error) => report.failed.push((relative, error.to_string())),
            }
        }
        Ok((library, report))
    }

    fn restore_folder(&mut self, id: i64, relative: &str, folder: &Path) -> Result<()> {
        let mut meta = opf::parse_opf(&fs::read(folder.join(METADATA_FILE))?)?;
        meta.cover = fs::read(folder.join(COVER_FILE)).ok();
        // As when restoring from the trash: the OPF holds one sort key per author.
        let first = meta.authors.first().map(|author| text::author_sort(author));
        if meta.authors.len() > 1 && meta.author_sort == first {
            meta.author_sort = None;
        }
        let stem =
            text::format_file_stem(&meta.title, meta.authors.first().map_or("", String::as_str));
        let mut files: Vec<(String, PathBuf)> = format_files(folder)
            .into_iter()
            .filter(|(format, file)| {
                let base = format.strip_prefix("ORIGINAL_").unwrap_or(format);
                BOOK_FORMATS.contains(&base)
                    && !file
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            })
            .collect();
        // One file per format: the one under the library's own name when
        // there are several, so an extra copy never replaces it.
        files.sort_by_key(|(format, file)| {
            let canonical = file
                .file_stem()
                .is_some_and(|name| name.to_string_lossy() == stem);
            (format.clone(), !canonical)
        });
        files.dedup_by(|next, kept| next.0 == kept.0);
        let restored = self.insert_book(
            meta,
            &files,
            NewBook {
                id: Some(id),
                keep_uuid: true,
            },
        )?;
        let target = safe_join(&self.root, &restored.path)?;
        // The book may now live under another name; remove only the copies
        // that are safely in the new place and keep anything else.
        // Names differing only in case are one file on some filesystems.
        let same_name = |a: &Path, b: &Path| {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        };
        let mut seen = HashSet::new();
        for (format, source) in &files {
            let kept = target.join(format!(
                "{}.{}",
                text::format_file_stem(
                    &restored.title,
                    restored.authors.first().map_or("", String::as_str)
                ),
                format.to_ascii_lowercase()
            ));
            if seen.insert(format.clone()) && !same_name(source, &kept) && kept.is_file() {
                let _ = fs::remove_file(source);
            }
        }
        if !same_name(&target, folder) {
            let extras = folder.join(DATA_DIR);
            if extras.is_dir() {
                let _ = fs::rename(&extras, target.join(DATA_DIR));
            }
            let _ = fs::remove_file(folder.join(METADATA_FILE));
            let _ = fs::remove_file(folder.join(COVER_FILE));
            // Only succeeds once nothing is left in it.
            if fs::remove_dir(folder).is_ok() {
                if let Some(parent) = folder.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
        Ok(())
    }
}
