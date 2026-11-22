//! The library engine: a calibre-compatible `metadata.db` plus the on-disk
//! book directory layout (`First Author/Title (id)/...`).
//!
//! The schema is calibre's own (version 28), including its triggers. Custom
//! SQLite functions (`title_sort`, `uuid4`, ...) are registered on every
//! connection so calibre's triggers work unmodified, and so a library created
//! here can be opened by calibre itself.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::functions::FunctionFlags;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension};

use crate::error::{Error, Result};
use crate::formats::{self, BookText};
use crate::jobs::{self, ItemResult, JobContext};
use crate::metadata::{format_datetime, now, parse_datetime, BookSummary, Metadata};
use crate::opf;
use crate::text;

mod edit;
mod fts;
mod manage;
mod organize;
mod reading;
mod search;
pub use edit::*;
pub use fts::*;
pub use manage::*;
pub use organize::*;
pub use reading::*;

pub const DB_FILE: &str = "metadata.db";
pub const METADATA_FILE: &str = "metadata.opf";
pub const COVER_FILE: &str = "cover.jpg";
const SCHEMA: &str = include_str!("schema.sql");
/// The `PRAGMA user_version` this engine reads and writes.
pub const SCHEMA_VERSION: i64 = 28;
/// Tables every usable calibre database has.
const REQUIRED_TABLES: [&str; 8] = [
    "books",
    "data",
    "authors",
    "books_authors_link",
    "tags",
    "series",
    "identifiers",
    "library_id",
];
/// Formats `read_book` can turn into text, best first.
pub const READABLE_FORMATS: [&str; 11] = [
    "EPUB", "AZW3", "KF8", "MOBI", "AZW", "PRC", "FB2", "HTML", "DOCX", "RTF", "TXT",
];

/// Test hook: where an injected failure interrupts a mutation.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// After files were written or renamed, before the database commit.
    BeforeCommit,
}

/// Result of adding a book.
#[derive(Debug, Clone, PartialEq)]
pub struct AddResult {
    pub id: i64,
    pub title: String,
    pub authors: Vec<String>,
    pub formats: Vec<String>,
    pub path: String,
}

/// How [`Library::insert_book`] creates a record.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NewBook {
    /// Reuse this id (restoring from the trash) instead of the next free one.
    pub id: Option<i64>,
    /// Keep the metadata's uuid rather than minting a new one.
    pub keep_uuid: bool,
}

/// Library-wide counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryStats {
    pub books: usize,
    pub authors: usize,
    pub series: usize,
    pub tags: usize,
    pub publishers: usize,
    pub total_bytes: u64,
    pub formats: BTreeMap<String, usize>,
}

/// Fields to change in one metadata edit. `None` means "keep"; for optional
/// fields `Some(None)` clears the value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookUpdate {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub publisher: Option<Option<String>>,
    pub series: Option<Option<String>>,
    pub series_index: Option<f64>,
    pub tags: Option<Vec<String>>,
    pub rating: Option<Option<u8>>,
    pub comments: Option<Option<String>>,
    pub identifiers: Option<BTreeMap<String, String>>,
    pub languages: Option<Vec<String>>,
    pub pubdate: Option<Option<DateTime<Utc>>>,
    pub cover: Option<Option<Vec<u8>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSort {
    Title,
    Author,
    Date,
    Pubdate,
    Series,
    Rating,
}

impl BookSort {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "title" => Some(Self::Title),
            "author" | "authors" => Some(Self::Author),
            "date" | "timestamp" | "added" => Some(Self::Date),
            "pubdate" | "published" => Some(Self::Pubdate),
            "series" => Some(Self::Series),
            "rating" => Some(Self::Rating),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Author => "author",
            Self::Date => "date",
            Self::Pubdate => "pubdate",
            Self::Series => "series",
            Self::Rating => "rating",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub query: Option<String>,
    pub sort: Option<BookSort>,
    pub descending: bool,
    pub limit: Option<usize>,
}

/// A calibre library rooted at a directory containing `metadata.db`.
pub struct Library {
    root: PathBuf,
    conn: Connection,
    read_only: Option<String>,
    fault: Option<Fault>,
}

impl Library {
    /// Create a new, empty library.
    pub fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let db_path = root.join(DB_FILE);
        if db_path.exists() {
            return Err(Error::InvalidLibrary(format!(
                "{} already contains metadata.db",
                root.display()
            )));
        }
        let conn = Connection::open(&db_path)?;
        configure_connection(&conn, true)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO library_id (uuid) VALUES (?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO preferences (key, val) VALUES ('calibre_rust_version', ?1)",
            // calibre expects every preference to be JSON.
            [concat!("\"", env!("CARGO_PKG_VERSION"), "\"")],
        )?;
        Ok(Self {
            root: root.to_path_buf(),
            conn,
            read_only: None,
            fault: None,
        })
    }

    /// Open an existing library. A database whose schema this engine does not
    /// write (older or newer than [`SCHEMA_VERSION`]) opens read-only; see
    /// [`Library::read_only_reason`].
    pub fn open(root: &Path) -> Result<Self> {
        let db_path = root.join(DB_FILE);
        if !db_path.is_file() {
            return Err(Error::InvalidLibrary(root.display().to_string()));
        }
        let probe = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = probe
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| not_calibre(&db_path))?;
        if version == 0 {
            return Err(not_calibre(&db_path));
        }
        for table in REQUIRED_TABLES {
            let found: Option<i64> = probe
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .optional()?;
            if found.is_none() {
                return Err(Error::InvalidLibrary(format!(
                    "{} is missing the `{table}` table; restore it from a backup or rebuild it in calibre",
                    db_path.display()
                )));
            }
        }
        let read_only = match version.cmp(&SCHEMA_VERSION) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(format!(
                "this library uses database schema {version}, newer than the supported {SCHEMA_VERSION}; \
                 it is open read-only. Update calibre-rust to change it."
            )),
            std::cmp::Ordering::Less => Some(format!(
                "this library uses database schema {version}, older than the supported {SCHEMA_VERSION}; \
                 it is open read-only. Open it once in calibre to upgrade it."
            )),
        };
        let conn = if read_only.is_some() {
            probe
        } else {
            drop(probe);
            Connection::open(&db_path)?
        };
        configure_connection(&conn, read_only.is_none())?;
        Ok(Self {
            root: root.to_path_buf(),
            conn,
            read_only,
            fault: None,
        })
    }

    pub fn open_or_create(root: &Path) -> Result<Self> {
        if root.join(DB_FILE).is_file() {
            Self::open(root)
        } else {
            Self::create(root)
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why this library cannot be changed, if it was opened read-only.
    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    fn writable(&self) -> Result<()> {
        match &self.read_only {
            Some(reason) => Err(Error::ReadOnly(reason.clone())),
            None => Ok(()),
        }
    }

    #[doc(hidden)]
    pub fn inject_fault(&mut self, fault: Option<Fault>) {
        self.fault = fault;
    }

    fn fail_point(&mut self, point: Fault) -> Result<()> {
        if self.fault == Some(point) {
            self.fault = None;
            return Err(Error::Other(format!("injected fault: {point:?}")));
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn library_id(&self) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT uuid FROM library_id ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )?)
    }

    /// Add a file to the library, copying it into a calibre book directory and
    /// writing `metadata.opf` / `cover.jpg`.
    pub fn add_book(&mut self, source: &Path) -> Result<AddResult> {
        self.writable()?;
        let imported = formats::import(source)?;
        let files = [(imported.format, source.to_path_buf())];
        self.insert_book(imported.metadata, &files, NewBook::default())
    }

    /// Create a book record from metadata and format files, atomically: on
    /// failure neither rows nor files remain.
    pub(crate) fn insert_book(
        &mut self,
        mut meta: Metadata,
        files: &[(String, PathBuf)],
        options: NewBook,
    ) -> Result<AddResult> {
        self.writable()?;
        let fault = self.fault.take();
        meta.normalize();
        let created = now();
        let timestamp = *meta.timestamp.get_or_insert(created);
        let pubdate = meta.pubdate.unwrap_or(timestamp);
        let author_sort = meta.computed_author_sort();
        meta.author_sort = Some(author_sort.clone());

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO books (id, title, timestamp, pubdate, series_index, author_sort, path, has_cover, last_modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 0, ?7)",
            params![
                options.id,
                meta.title,
                format_datetime(&timestamp),
                format_datetime(&pubdate),
                meta.series_index,
                author_sort,
                format_datetime(&created),
            ],
        )?;
        let id = tx.last_insert_rowid();
        if let Some(sort) = meta.title_sort.as_deref().filter(|_| options.keep_uuid) {
            // The insert trigger computed a sort; a carried-over one wins.
            tx.execute("UPDATE books SET sort=?1 WHERE id=?2", params![sort, id])?;
        }
        let relative = text::book_dir_name(id, &meta.title, meta.first_author());
        let directory = safe_join(&self.root, &relative)?;
        let mut cleanup = DirGuard::new(directory.clone());
        fs::create_dir_all(&directory)?;

        let stem = text::format_file_stem(&meta.title, meta.first_author());
        let mut stored = Vec::new();
        for (format, source) in files {
            let format = format.to_ascii_uppercase();
            if stored.contains(&format) {
                continue;
            }
            let extension = format.to_ascii_lowercase();
            let destination = directory.join(format!("{stem}.{extension}"));
            if *source != destination {
                cleanup.track(&destination);
                copy_atomic(source, &destination)?;
            }
            let size = fs::metadata(&destination)?.len();
            tx.execute(
                "INSERT INTO data (book, format, uncompressed_size, name) VALUES (?1, ?2, ?3, ?4)",
                params![id, format, size as i64, stem],
            )?;
            stored.push(format);
        }

        match meta.uuid.as_deref().filter(|_| options.keep_uuid) {
            Some(uuid) => {
                tx.execute("UPDATE books SET uuid=?1 WHERE id=?2", params![uuid, id])?;
            }
            None => {
                meta.uuid =
                    tx.query_row("SELECT uuid FROM books WHERE id=?1", [id], |row| row.get(0))?;
            }
        }

        let has_cover = if let Some(cover) = &meta.cover {
            cleanup.track(&directory.join(COVER_FILE));
            write_atomic(&directory.join(COVER_FILE), cover)?;
            true
        } else {
            false
        };

        replace_links(&tx, id, &meta)?;
        cleanup.track(&directory.join(METADATA_FILE));
        write_atomic(
            &directory.join(METADATA_FILE),
            opf::metadata_to_opf(&meta).as_bytes(),
        )?;
        tx.execute(
            "UPDATE books SET path=?1, has_cover=?2 WHERE id=?3",
            params![relative, has_cover as i64, id],
        )?;
        if fault == Some(Fault::BeforeCommit) {
            return Err(Error::Other("injected fault: BeforeCommit".into()));
        }
        tx.commit()?;
        // The book is committed; keep the directory.
        cleanup.disarm();

        Ok(AddResult {
            id,
            title: meta.title,
            authors: meta.authors,
            formats: stored,
            path: relative,
        })
    }

    /// Add many files as one job. A file that fails is reported in its own
    /// result and does not stop the rest; cancellation keeps what was added.
    pub fn add_books(
        &mut self,
        sources: &[PathBuf],
        context: &JobContext,
    ) -> Vec<ItemResult<PathBuf, AddResult>> {
        jobs::for_each(
            context,
            sources.iter().cloned(),
            |path| format!("Adding {}", file_label(path)),
            |path| self.add_book(path),
        )
    }

    /// Full metadata for one book.
    pub fn get_metadata(&self, id: i64) -> Result<Metadata> {
        let row = self
            .conn
            .query_row(
                "SELECT title, sort, author_sort, series_index, timestamp, pubdate, path, uuid,
                        has_cover, last_modified
                 FROM books WHERE id=?1",
                [id],
                |row| {
                    Ok(BookRow {
                        title: row.get(0)?,
                        title_sort: row.get(1)?,
                        author_sort: row.get(2)?,
                        series_index: row.get(3)?,
                        timestamp: row.get(4)?,
                        pubdate: row.get(5)?,
                        path: row.get(6)?,
                        uuid: row.get(7)?,
                        has_cover: row.get::<_, i64>(8)? != 0,
                        last_modified: row.get(9)?,
                    })
                },
            )
            .optional()?;
        let Some(row) = row else {
            return Err(Error::BookNotFound(id));
        };

        let mut meta = Metadata::default();
        meta.title = row.title;
        meta.title_sort = row.title_sort;
        meta.author_sort = row.author_sort;
        meta.series_index = row.series_index;
        meta.timestamp = row.timestamp.as_deref().and_then(parse_datetime);
        meta.pubdate = row.pubdate.as_deref().and_then(parse_datetime);
        meta.last_modified = row.last_modified.as_deref().and_then(parse_datetime);
        meta.uuid = row.uuid;
        meta.authors = self.names_for(id, "authors", "author", "books_authors_link")?;
        meta.tags = self.names_for(id, "tags", "tag", "books_tags_link")?;
        meta.publisher = self
            .conn
            .query_row(
                "SELECT p.name FROM publishers p
                 JOIN books_publishers_link l ON l.publisher=p.id WHERE l.book=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        meta.series = self
            .conn
            .query_row(
                "SELECT s.name FROM series s
                 JOIN books_series_link l ON l.series=s.id WHERE l.book=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        meta.rating = self
            .conn
            .query_row(
                "SELECT r.rating FROM ratings r
                 JOIN books_ratings_link l ON l.rating=r.id WHERE l.book=?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .and_then(|r| u8::try_from(r).ok());
        meta.comments = self
            .conn
            .query_row("SELECT text FROM comments WHERE book=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        meta.languages = self.languages_for(id)?;
        meta.identifiers = self.identifiers_for(id)?;
        let formats = self.formats(id)?;
        meta.format = formats.first().cloned();
        if row.has_cover {
            if let Ok(directory) = safe_join(&self.root, &row.path) {
                meta.cover = fs::read(directory.join(COVER_FILE)).ok();
            }
        }
        meta.normalize();
        Ok(meta)
    }

    /// A lightweight summary for lists and JSON output.
    pub fn get_summary(&self, id: i64) -> Result<BookSummary> {
        let mut books = self.query_summaries(
            "b.id = ?",
            &[id.to_string()],
            BookSort::Title,
            false,
            Some(1),
        )?;
        books.pop().ok_or(Error::BookNotFound(id))
    }

    /// Search/list books. See `ListOptions` and the `query` mini-language:
    /// `title:`, `author:`, `tag:`, `series:`, `publisher:`, `format:`,
    /// `language:`, `identifier:`, `rating:`, `id:` and `sort:`.
    pub fn list_books(&self, options: &ListOptions) -> Result<Vec<BookSummary>> {
        let context = self.search_context()?;
        let parsed = search::compile(options.query.as_deref().unwrap_or_default(), &context);
        let sort = options.sort.or(parsed.sort).unwrap_or(BookSort::Title);
        let descending = options.descending || parsed.descending;
        self.query_summaries(
            &parsed.where_sql,
            &parsed.params,
            sort,
            descending,
            options.limit,
        )
    }

    /// Id, uuid, title and first author of every book, for matching the
    /// files on a device back to the library.
    pub fn book_identities(&self) -> Result<Vec<(i64, String, String, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT b.id, COALESCE(b.uuid, ''), b.title, COALESCE((
                 SELECT a.name FROM books_authors_link l
                 JOIN authors a ON a.id = l.author
                 WHERE l.book = b.id ORDER BY l.id LIMIT 1), '')
             FROM books b",
        )?;
        let rows = statement.query_map([], |row| {
            let author: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                author.replace('|', ","),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn all_book_ids(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT id FROM books ORDER BY id")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Update metadata, renaming the book directory and format files if the
    /// author/title changed, then rewriting `metadata.opf`.
    pub fn update_book(&mut self, id: i64, patch: &BookUpdate) -> Result<()> {
        self.writable()?;
        let mut undo = FsUndo::default();
        let result = self.update_book_inner(id, patch, &mut undo);
        if result.is_err() {
            undo.roll_back();
        }
        result
    }

    fn update_book_inner(&mut self, id: i64, patch: &BookUpdate, undo: &mut FsUndo) -> Result<()> {
        let mut meta = self.get_metadata(id)?;
        let old_relative = self.book_path(id)?;
        let authors_changed = patch.authors.is_some();

        if let Some(title) = &patch.title {
            let title = title.trim();
            if title.is_empty() {
                return Err(Error::InvalidMetadata("title cannot be empty".into()));
            }
            meta.title = title.to_string();
        }
        if let Some(authors) = &patch.authors {
            meta.authors = authors.clone();
        }
        if let Some(publisher) = &patch.publisher {
            meta.publisher = publisher.clone();
        }
        if let Some(series) = &patch.series {
            meta.series = series.clone();
        }
        if let Some(index) = patch.series_index {
            meta.series_index = index;
        }
        if let Some(tags) = &patch.tags {
            meta.tags = tags.clone();
        }
        if let Some(rating) = &patch.rating {
            meta.rating = *rating;
        }
        if let Some(comments) = &patch.comments {
            meta.comments = comments.clone();
        }
        if let Some(identifiers) = &patch.identifiers {
            meta.identifiers = identifiers.clone();
        }
        if let Some(languages) = &patch.languages {
            meta.languages = languages.clone();
        }
        if let Some(pubdate) = &patch.pubdate {
            meta.pubdate = *pubdate;
        }
        if authors_changed {
            meta.author_sort = None;
        }
        meta.normalize();
        let author_sort = meta.computed_author_sort();
        meta.author_sort = Some(author_sort.clone());

        let new_relative = text::book_dir_name(id, &meta.title, meta.first_author());
        let old_directory = safe_join(&self.root, &old_relative)?;
        let new_directory = safe_join(&self.root, &new_relative)?;
        if old_relative != new_relative {
            if let Some(parent) = new_directory.parent() {
                fs::create_dir_all(parent)?;
            }
            if old_directory.is_dir() {
                if new_directory.exists() {
                    return Err(Error::Other(format!(
                        "cannot rename book directory: {} already exists",
                        new_directory.display()
                    )));
                }
                fs::rename(&old_directory, &new_directory)?;
                undo.renamed(&old_directory, &new_directory);
                remove_empty_parent(&old_directory);
            } else {
                fs::create_dir_all(&new_directory)?;
            }
        } else {
            fs::create_dir_all(&new_directory)?;
        }

        let new_stem = text::format_file_stem(&meta.title, meta.first_author());
        let entries = self.format_entries(id)?;
        for (format, old_name) in &entries {
            let extension = format.to_ascii_lowercase();
            let old_file =
                find_file_ignore_case(&new_directory, &format!("{old_name}.{extension}"))
                    .unwrap_or_else(|| new_directory.join(format!("{old_name}.{extension}")));
            let new_file = new_directory.join(format!("{new_stem}.{extension}"));
            if old_file != new_file && old_file.is_file() {
                if new_file.exists() && !same_file_ignore_case(&old_file, &new_file) {
                    return Err(Error::Other(format!(
                        "cannot rename format file: {} already exists",
                        new_file.display()
                    )));
                }
                fs::rename(&old_file, &new_file)?;
                undo.renamed(&old_file, &new_file);
            }
        }

        match &patch.cover {
            None => {}
            Some(None) => {
                let cover_path = new_directory.join(COVER_FILE);
                if cover_path.exists() {
                    undo.snapshot(&cover_path)?;
                    fs::remove_file(cover_path)?;
                }
                meta.cover = None;
            }
            Some(Some(bytes)) => {
                let normalized = formats::normalize_cover(bytes).ok_or_else(|| {
                    Error::InvalidMetadata("unsupported cover image format".into())
                })?;
                undo.snapshot(&new_directory.join(COVER_FILE))?;
                write_atomic(&new_directory.join(COVER_FILE), &normalized)?;
                meta.cover = Some(normalized);
            }
        }
        let has_cover = new_directory.join(COVER_FILE).is_file();
        undo.snapshot(&new_directory.join(METADATA_FILE))?;
        write_atomic(
            &new_directory.join(METADATA_FILE),
            opf::metadata_to_opf(&meta).as_bytes(),
        )?;
        self.fail_point(Fault::BeforeCommit)?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE books SET title=?1, author_sort=?2, series_index=?3, pubdate=?4,
                              path=?5, has_cover=?6, last_modified=?7
             WHERE id=?8",
            params![
                meta.title,
                author_sort,
                meta.series_index,
                meta.pubdate.as_ref().map(format_datetime),
                new_relative,
                has_cover as i64,
                format_datetime(&now()),
                id,
            ],
        )?;
        replace_links(&tx, id, &meta)?;
        for (format, _) in &entries {
            tx.execute(
                "UPDATE data SET name=?1 WHERE book=?2 AND format=?3 COLLATE NOCASE",
                params![new_stem, id, format],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_cover(&mut self, id: i64, cover: Option<&[u8]>) -> Result<()> {
        let mut patch = BookUpdate::default();
        patch.cover = Some(cover.map(|c| c.to_vec()));
        self.update_book(id, &patch)
    }

    pub fn remove_book(&mut self, id: i64, delete_files: bool) -> Result<()> {
        self.writable()?;
        let relative = self.book_path(id)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM books_pages_link WHERE book=?1", [id])?;
        tx.execute("DELETE FROM book_storage WHERE book=?1", [id])?;
        // Custom column values have no delete trigger.
        let custom: Vec<(i64, bool)> = {
            let mut stmt = tx.prepare("SELECT id, normalized FROM custom_columns")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (column, normalized) in custom {
            let table = if normalized {
                format!("books_custom_column_{column}_link")
            } else {
                format!("custom_column_{column}")
            };
            tx.execute(&format!("DELETE FROM {table} WHERE book=?1"), [id])?;
        }
        let changed = tx.execute("DELETE FROM books WHERE id=?1", [id])?;
        if changed == 0 {
            return Err(Error::BookNotFound(id));
        }
        tx.commit()?;
        if delete_files && !relative.is_empty() {
            let directory = safe_join(&self.root, &relative)?;
            if directory.is_dir() {
                fs::remove_dir_all(&directory)?;
                remove_empty_parent(&directory);
            }
        }
        Ok(())
    }

    pub fn formats(&self, id: i64) -> Result<Vec<String>> {
        self.ensure_exists(id)?;
        let mut stmt = self
            .conn
            .prepare("SELECT format FROM data WHERE book=?1 ORDER BY format")?;
        let formats = stmt
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(formats)
    }

    /// Absolute path to a format file, with a case-insensitive fallback.
    pub fn format_path(&self, id: i64, format: &str) -> Result<Option<PathBuf>> {
        let relative = self.book_path(id)?;
        let directory = safe_join(&self.root, &relative)?;
        let found: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT name, format FROM data WHERE book=?1 AND format=?2 COLLATE NOCASE",
                params![id, format],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((name, stored_format)) = found else {
            return Ok(None);
        };
        let extension = stored_format.to_ascii_lowercase();
        let canonical = directory.join(format!("{name}.{extension}"));
        if canonical.is_file() {
            return Ok(Some(canonical));
        }
        Ok(find_file_ignore_case(
            &directory,
            &format!("{name}.{extension}"),
        ))
    }

    pub fn book_path(&self, id: i64) -> Result<String> {
        let path: Option<String> = self
            .conn
            .query_row("SELECT path FROM books WHERE id=?1", [id], |row| row.get(0))
            .optional()?;
        path.ok_or(Error::BookNotFound(id))
    }

    pub fn read_book(&self, id: i64, format: Option<&str>) -> Result<BookText> {
        let format = match format {
            Some(format) => format.to_ascii_uppercase(),
            None => self.preferred_readable_format(id)?,
        };
        let path = self
            .format_path(id, &format)?
            .ok_or_else(|| Error::Other(format!("book {id} has no {format} file")))?;
        formats::extract_text(&path, Some(&format))
    }

    /// The best format of a book that can be read as text.
    pub fn preferred_readable_format(&self, id: i64) -> Result<String> {
        let formats = self.formats(id)?;
        if formats.is_empty() {
            return Err(Error::Other(format!("book {id} has no formats")));
        }
        READABLE_FORMATS
            .iter()
            .find(|wanted| formats.iter().any(|f| f.eq_ignore_ascii_case(wanted)))
            .map(|f| f.to_string())
            .ok_or_else(|| {
                Error::UnsupportedFormat(format!(
                    "book {id} only has {}; reading needs one of {}",
                    formats.join(", "),
                    READABLE_FORMATS.join(", ")
                ))
            })
    }

    pub fn cover(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let relative = self.book_path(id)?;
        let directory = safe_join(&self.root, &relative)?;
        Ok(fs::read(directory.join(COVER_FILE)).ok())
    }

    /// Extract an EPUB into a directory for editing.
    pub fn unpack_book(&self, id: i64, format: &str, destination: &Path) -> Result<usize> {
        let format = format.to_ascii_uppercase();
        if format != "EPUB" {
            return Err(Error::UnsupportedFormat(format!(
                "editing {format} files is not implemented; only EPUB"
            )));
        }
        let path = self
            .format_path(id, &format)?
            .ok_or_else(|| Error::Other(format!("book {id} has no {format} file")))?;
        formats::epub::unpack(&path, destination)
    }

    /// Replace (or add) a format file from an edited copy.
    pub fn replace_format(&mut self, id: i64, format: &str, source: &Path) -> Result<()> {
        self.writable()?;
        let format = format.to_ascii_uppercase();
        let relative = self.book_path(id)?;
        let directory = safe_join(&self.root, &relative)?;
        let meta = self.get_metadata(id)?;
        let existing = self.format_path(id, &format)?;
        let stem = existing
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| text::format_file_stem(&meta.title, meta.first_author()));
        let extension = format.to_ascii_lowercase();
        let destination = directory.join(format!("{stem}.{extension}"));
        let source = source.canonicalize()?;
        if destination.canonicalize().ok().as_ref() != Some(&source) {
            copy_atomic(&source, &destination)?;
        }
        let size = fs::metadata(&destination)?.len() as i64;
        self.conn.execute(
            "INSERT INTO data (book, format, uncompressed_size, name) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(book, format) DO UPDATE SET uncompressed_size=excluded.uncompressed_size,
                                                     name=excluded.name",
            params![id, format, size, stem],
        )?;
        self.sync_book(id)?;
        Ok(())
    }

    /// Copy books out of the library using a template. Placeholders:
    /// `{author}`, `{authors}`, `{author_sort}`, `{title}`, `{series}`,
    /// `{series_index}`, `{publisher}`, `{year}`, `{language}`, `{tags}`,
    /// `{rating}`, `{id}`.
    pub fn export_books(
        &self,
        ids: &[i64],
        destination: &Path,
        template: &str,
        format: Option<&str>,
    ) -> Result<Vec<PathBuf>> {
        let options = ExportOptions {
            template: template.to_string(),
            formats: format.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        let mut written = Vec::new();
        for &id in ids {
            written.extend(self.export_book(id, destination, &options)?);
        }
        Ok(written)
    }

    /// Rewrite `metadata.opf` for one book.
    pub fn sync_book(&mut self, id: i64) -> Result<()> {
        self.writable()?;
        let meta = self.get_metadata(id)?;
        let relative = self.book_path(id)?;
        let directory = safe_join(&self.root, &relative)?;
        fs::create_dir_all(&directory)?;
        write_atomic(
            &directory.join(METADATA_FILE),
            opf::metadata_to_opf(&meta).as_bytes(),
        )
    }

    /// Rewrite every book's `metadata.opf` (e.g. after copying a library).
    pub fn sync_all(&mut self) -> Result<usize> {
        let ids = self.all_book_ids()?;
        for &id in &ids {
            self.sync_book(id)?;
        }
        Ok(ids.len())
    }

    pub fn stats(&self) -> Result<LibraryStats> {
        // Count entities actually linked to a book; editing metadata can
        // leave orphaned rows in calibre's lookup tables.
        let count_used = |link: &str, column: &str| -> Result<usize> {
            let sql = format!("SELECT COUNT(DISTINCT {column}) FROM {link}");
            Ok(self.conn.query_row(&sql, [], |row| row.get::<_, i64>(0))? as usize)
        };
        let mut formats = BTreeMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT format, COUNT(*) FROM data GROUP BY format ORDER BY format")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                formats.insert(row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize);
            }
        }
        let total_bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(uncompressed_size), 0) FROM data",
            [],
            |row| row.get(0),
        )?;
        Ok(LibraryStats {
            books: self
                .conn
                .query_row("SELECT COUNT(*) FROM books", [], |row| row.get::<_, i64>(0))?
                as usize,
            authors: count_used("books_authors_link", "author")?,
            series: count_used("books_series_link", "series")?,
            tags: count_used("books_tags_link", "tag")?,
            publishers: count_used("books_publishers_link", "publisher")?,
            total_bytes: total_bytes.max(0) as u64,
            formats,
        })
    }

    fn ensure_exists(&self, id: i64) -> Result<()> {
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT id FROM books WHERE id=?1", [id], |row| row.get(0))
            .optional()?;
        if exists.is_some() {
            Ok(())
        } else {
            Err(Error::BookNotFound(id))
        }
    }

    fn names_for(&self, id: i64, table: &str, column: &str, link: &str) -> Result<Vec<String>> {
        let sql = format!(
            "SELECT t.name FROM {link} l JOIN {table} t ON t.id=l.{column}
             WHERE l.book=?1 ORDER BY l.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let values = stmt
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(values)
    }

    fn languages_for(&self, id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT lang.lang_code FROM books_languages_link l
             JOIN languages lang ON lang.id=l.lang_code
             WHERE l.book=?1 ORDER BY l.item_order, l.id",
        )?;
        let values = stmt
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(values)
    }

    fn identifiers_for(&self, id: i64) -> Result<BTreeMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT type, val FROM identifiers WHERE book=?1 ORDER BY type")?;
        let mut map = BTreeMap::new();
        let mut rows = stmt.query([id])?;
        while let Some(row) = rows.next()? {
            map.insert(row.get::<_, String>(0)?, row.get::<_, String>(1)?);
        }
        Ok(map)
    }

    fn format_entries(&self, id: i64) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT format, name FROM data WHERE book=?1 ORDER BY format")?;
        let entries = stmt
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(entries)
    }

    fn query_summaries(
        &self,
        where_sql: &str,
        params: &[String],
        sort: BookSort,
        descending: bool,
        limit: Option<usize>,
    ) -> Result<Vec<BookSummary>> {
        let direction = if descending { "DESC" } else { "ASC" };
        let order = match sort {
            BookSort::Title => format!("b.sort COLLATE NOCASE {direction}"),
            BookSort::Author => {
                format!("b.author_sort COLLATE NOCASE {direction}, b.sort COLLATE NOCASE")
            }
            BookSort::Date => format!("b.timestamp {direction}"),
            BookSort::Pubdate => format!("b.pubdate {direction}"),
            BookSort::Series => {
                format!("series COLLATE NOCASE {direction}, b.series_index {direction}")
            }
            BookSort::Rating => format!("rating {direction}"),
        };
        let mut sql = String::from(
            "SELECT b.id, b.title, b.author_sort, b.series_index, b.timestamp, b.pubdate,
                    b.path, b.has_cover,
                    (SELECT s.name FROM series s JOIN books_series_link l ON l.series=s.id
                     WHERE l.book=b.id LIMIT 1) AS series,
                    (SELECT r.rating FROM ratings r JOIN books_ratings_link l ON l.rating=r.id
                     WHERE l.book=b.id LIMIT 1) AS rating,
                    (SELECT p.name FROM publishers p JOIN books_publishers_link l ON l.publisher=p.id
                     WHERE l.book=b.id LIMIT 1) AS publisher
             FROM books b",
        );
        if !where_sql.trim().is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(where_sql);
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&order);
        sql.push_str(", b.id ASC");
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(params.iter()), |row| {
                Ok(SummaryRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    author_sort: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    series_index: row.get(3)?,
                    timestamp: row
                        .get::<_, Option<String>>(4)?
                        .as_deref()
                        .and_then(parse_datetime),
                    pubdate: row
                        .get::<_, Option<String>>(5)?
                        .as_deref()
                        .and_then(parse_datetime),
                    path: row.get(6)?,
                    has_cover: row.get::<_, i64>(7)? != 0,
                    series: row.get(8)?,
                    rating: row
                        .get::<_, Option<i64>>(9)?
                        .and_then(|r| u8::try_from(r).ok()),
                    publisher: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let placeholders = vec!["?"; ids.len()].join(",");
        let authors = self.batch_strings(
            &format!(
                "SELECT l.book, a.name FROM books_authors_link l JOIN authors a ON a.id=l.author
                 WHERE l.book IN ({placeholders}) ORDER BY l.id"
            ),
            &ids,
        )?;
        let tags = self.batch_strings(
            &format!(
                "SELECT l.book, t.name FROM books_tags_link l JOIN tags t ON t.id=l.tag
                 WHERE l.book IN ({placeholders}) ORDER BY l.id"
            ),
            &ids,
        )?;
        let formats = self.batch_strings(
            &format!(
                "SELECT book, format FROM data WHERE book IN ({placeholders}) ORDER BY format"
            ),
            &ids,
        )?;

        Ok(rows
            .into_iter()
            .map(|row| BookSummary {
                authors: authors.get(&row.id).cloned().unwrap_or_default(),
                tags: tags.get(&row.id).cloned().unwrap_or_default(),
                formats: formats.get(&row.id).cloned().unwrap_or_default(),
                id: row.id,
                title: row.title,
                author_sort: row.author_sort,
                publisher: row.publisher,
                series: row.series,
                series_index: row.series_index,
                rating: row.rating,
                has_cover: row.has_cover,
                timestamp: row.timestamp,
                pubdate: row.pubdate,
                path: row.path,
            })
            .collect())
    }

    fn batch_strings(&self, sql: &str, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut map: HashMap<i64, Vec<String>> = HashMap::new();
        let mut rows = stmt.query(params_from_iter(ids.iter()))?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let value: String = row.get(1)?;
            map.entry(id).or_default().push(value);
        }
        Ok(map)
    }
}

#[derive(Debug)]
struct BookRow {
    title: String,
    title_sort: Option<String>,
    author_sort: Option<String>,
    series_index: f64,
    timestamp: Option<String>,
    pubdate: Option<String>,
    path: String,
    uuid: Option<String>,
    has_cover: bool,
    last_modified: Option<String>,
}

#[derive(Debug)]
struct SummaryRow {
    id: i64,
    title: String,
    author_sort: String,
    series_index: f64,
    timestamp: Option<DateTime<Utc>>,
    pubdate: Option<DateTime<Utc>>,
    path: String,
    has_cover: bool,
    series: Option<String>,
    rating: Option<u8>,
    publisher: Option<String>,
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn not_calibre(db_path: &Path) -> Error {
    Error::InvalidLibrary(format!("{} is not a calibre database", db_path.display()))
}

fn configure_connection(conn: &Connection, writable: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(10))?;
    if writable {
        // calibre uses SQLite's rollback journal and reports `-wal`/`-shm`
        // files as library damage, so never leave a database in WAL mode.
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        if mode.eq_ignore_ascii_case("wal") {
            let _: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
        }
    }

    conn.create_scalar_function(
        "title_sort",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let title: String = ctx.get(0)?;
            Ok(text::title_sort(&title))
        },
    )?;
    conn.create_scalar_function(
        "author_to_author_sort",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let author: String = ctx.get(0)?;
            Ok(text::author_sort(&author))
        },
    )?;
    conn.create_scalar_function(
        "regexp",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            // The compiled pattern is cached for the statement.
            let pattern: Arc<Option<regex::Regex>> = ctx.get_or_create_aux(
                0,
                |value| -> std::result::Result<_, std::convert::Infallible> {
                    let source = value.as_str().unwrap_or_default();
                    let built = regex::RegexBuilder::new(source)
                        .case_insensitive(true)
                        .build();
                    Ok(built.ok())
                },
            )?;
            let text = ctx.get_raw(1).as_str().unwrap_or_default();
            Ok(pattern
                .as_ref()
                .as_ref()
                .is_some_and(|re| re.is_match(text)))
        },
    )?;
    conn.create_scalar_function("uuid4", 0, FunctionFlags::SQLITE_UTF8, |_| {
        Ok(uuid::Uuid::new_v4().to_string())
    })?;
    conn.create_scalar_function("books_list_filter", 1, FunctionFlags::SQLITE_UTF8, |_| {
        Ok(1i64)
    })?;
    Ok(())
}

fn replace_links(conn: &Connection, id: i64, meta: &Metadata) -> Result<()> {
    conn.execute("DELETE FROM books_authors_link WHERE book=?1", [id])?;
    for author in &meta.authors {
        let sort = text::author_sort(author);
        conn.execute(
            "INSERT OR IGNORE INTO authors (name, sort) VALUES (?1, ?2)",
            params![author, sort],
        )?;
        conn.execute(
            "UPDATE authors SET sort=?1 WHERE name=?2 AND (sort IS NULL OR sort='')",
            params![sort, author],
        )?;
        let author_id: i64 =
            conn.query_row("SELECT id FROM authors WHERE name=?1", [author], |row| {
                row.get(0)
            })?;
        conn.execute(
            "INSERT OR IGNORE INTO books_authors_link (book, author) VALUES (?1, ?2)",
            params![id, author_id],
        )?;
    }

    conn.execute("DELETE FROM books_tags_link WHERE book=?1", [id])?;
    for tag in &meta.tags {
        conn.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [tag])?;
        let tag_id: i64 =
            conn.query_row("SELECT id FROM tags WHERE name=?1", [tag], |row| row.get(0))?;
        conn.execute(
            "INSERT OR IGNORE INTO books_tags_link (book, tag) VALUES (?1, ?2)",
            params![id, tag_id],
        )?;
    }

    conn.execute("DELETE FROM books_publishers_link WHERE book=?1", [id])?;
    if let Some(publisher) = &meta.publisher {
        conn.execute(
            "INSERT OR IGNORE INTO publishers (name, sort) VALUES (?1, ?2)",
            params![publisher, publisher],
        )?;
        let publisher_id: i64 = conn.query_row(
            "SELECT id FROM publishers WHERE name=?1",
            [publisher],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO books_publishers_link (book, publisher) VALUES (?1, ?2)",
            params![id, publisher_id],
        )?;
    }

    conn.execute("DELETE FROM books_series_link WHERE book=?1", [id])?;
    if let Some(series) = &meta.series {
        conn.execute("INSERT OR IGNORE INTO series (name) VALUES (?1)", [series])?;
        let series_id: i64 =
            conn.query_row("SELECT id FROM series WHERE name=?1", [series], |row| {
                row.get(0)
            })?;
        conn.execute(
            "INSERT OR IGNORE INTO books_series_link (book, series) VALUES (?1, ?2)",
            params![id, series_id],
        )?;
    }

    conn.execute("DELETE FROM books_ratings_link WHERE book=?1", [id])?;
    if let Some(rating) = meta.rating.filter(|r| (1..=10).contains(r)) {
        conn.execute(
            "INSERT OR IGNORE INTO ratings (rating) VALUES (?1)",
            [rating],
        )?;
        let rating_id: i64 =
            conn.query_row("SELECT id FROM ratings WHERE rating=?1", [rating], |row| {
                row.get(0)
            })?;
        conn.execute(
            "INSERT OR IGNORE INTO books_ratings_link (book, rating) VALUES (?1, ?2)",
            params![id, rating_id],
        )?;
    }

    conn.execute("DELETE FROM comments WHERE book=?1", [id])?;
    if let Some(comments) = &meta.comments {
        conn.execute(
            "INSERT INTO comments (book, text) VALUES (?1, ?2)",
            params![id, comments],
        )?;
    }

    conn.execute("DELETE FROM identifiers WHERE book=?1", [id])?;
    for (kind, value) in &meta.identifiers {
        conn.execute(
            "INSERT OR REPLACE INTO identifiers (book, type, val) VALUES (?1, ?2, ?3)",
            params![id, kind, value],
        )?;
    }

    conn.execute("DELETE FROM books_languages_link WHERE book=?1", [id])?;
    for (order, language) in meta.languages.iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO languages (lang_code) VALUES (?1)",
            [language],
        )?;
        let language_id: i64 = conn.query_row(
            "SELECT id FROM languages WHERE lang_code=?1",
            [language],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO books_languages_link (book, lang_code, item_order)
             VALUES (?1, ?2, ?3)",
            params![id, language_id, order as i64],
        )?;
    }
    Ok(())
}

/// Removes what a failed `add_book` left behind: the files it wrote and the
/// directories it created, never anything that was already there.
#[derive(Debug)]
struct DirGuard {
    path: PathBuf,
    files: Vec<PathBuf>,
    armed: bool,
}

impl DirGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            files: Vec::new(),
            armed: true,
        }
    }

    /// Mark a file as created by this operation. Call before writing it.
    fn track(&mut self, file: &Path) {
        if !file.exists() {
            self.files.push(file.to_path_buf());
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for file in &self.files {
            let _ = fs::remove_file(file);
        }
        // `remove_dir` only succeeds on empty directories.
        if fs::remove_dir(&self.path).is_ok() {
            if let Some(parent) = self.path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

/// File changes made by a mutation, reversed if its database commit fails.
#[derive(Debug, Default)]
struct FsUndo {
    steps: Vec<UndoStep>,
}

#[derive(Debug)]
enum UndoStep {
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Restore {
        path: PathBuf,
        previous: Option<Vec<u8>>,
    },
}

impl FsUndo {
    fn renamed(&mut self, from: &Path, to: &Path) {
        self.steps.push(UndoStep::Rename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
    }

    /// Remember a file's current content (or absence) before overwriting it.
    fn snapshot(&mut self, path: &Path) -> Result<()> {
        let previous = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        self.steps.push(UndoStep::Restore {
            path: path.to_path_buf(),
            previous,
        });
        Ok(())
    }

    fn roll_back(&mut self) {
        while let Some(step) = self.steps.pop() {
            match step {
                UndoStep::Rename { from, to } => {
                    if let Some(parent) = from.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&to, &from);
                    if let Some(parent) = to.parent() {
                        let _ = fs::remove_dir(parent);
                    }
                }
                UndoStep::Restore {
                    path,
                    previous: Some(bytes),
                } => {
                    let _ = write_atomic(&path, &bytes);
                }
                UndoStep::Restore {
                    path,
                    previous: None,
                } => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
}

/// Write a file so readers see either the old or the new content, never a
/// partial one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut temporary = shared_temporary(directory)?;
    // Match the replaced file's mode before any content is written.
    if let Ok(existing) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(existing.permissions())?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// A temporary file with ordinary (umask-governed) permissions rather than
/// the owner-only default, so shared libraries stay readable by others.
fn shared_temporary(directory: &Path) -> Result<tempfile::NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    Ok(builder.tempfile_in(directory)?)
}

pub(crate) fn copy_atomic(source: &Path, destination: &Path) -> Result<()> {
    let directory = destination.parent().unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    fs::copy(source, temporary.path())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

/// Drop the author directory once its last book moved out or was deleted;
/// calibre's library check reports empty ones.
fn remove_empty_parent(book_directory: &Path) {
    if let Some(parent) = book_directory.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// True when two paths in one directory differ only by case and name the same
/// file (case-insensitive filesystems).
fn same_file_ignore_case(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => {
            x.to_string_lossy().to_lowercase() == y.to_string_lossy().to_lowercase()
        }
        _ => false,
    }
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.trim().is_empty() {
        return Err(Error::Other("empty book path".into()));
    }
    let mut out = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => {
                return Err(Error::Other(format!(
                    "unsafe path outside library: {relative}"
                )))
            }
        }
    }
    Ok(out)
}

fn find_file_ignore_case(directory: &Path, file_name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(file_name)
        {
            return Some(entry.path());
        }
    }
    None
}

fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let extension = path
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    for counter in 2..10_000 {
        let candidate = if extension.is_empty() {
            parent.join(format!("{stem} ({counter})"))
        } else {
            parent.join(format!("{stem} ({counter}).{extension}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

fn render_template(template: &str, meta: &Metadata, id: i64) -> String {
    let mut rendered = template.to_string();
    let replacements = [
        ("{author}", meta.first_author().to_string()),
        ("{authors}", meta.authors.join(" & ")),
        ("{author_sort}", meta.computed_author_sort()),
        ("{title}", meta.title.clone()),
        ("{series}", meta.series.clone().unwrap_or_default()),
        (
            "{series_index}",
            text::format_series_index(meta.series_index),
        ),
        ("{publisher}", meta.publisher.clone().unwrap_or_default()),
        (
            "{year}",
            meta.pubdate
                .map(|date| date.format("%Y").to_string())
                .unwrap_or_default(),
        ),
        (
            "{language}",
            meta.languages.first().cloned().unwrap_or_default(),
        ),
        ("{tags}", meta.tags.join(", ")),
        (
            "{rating}",
            meta.rating.map(|r| r.to_string()).unwrap_or_default(),
        ),
        ("{id}", id.to_string()),
    ];
    for (key, value) in replacements {
        rendered = rendered.replace(key, &value);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_templates() {
        let mut meta = Metadata::new("The Hobbit");
        meta.authors = vec!["J. R. R. Tolkien".into()];
        meta.series = Some("Middle Earth".into());
        assert_eq!(
            render_template("{author_sort}/{title} ({id})", &meta, 7),
            "Tolkien, J. R. R./The Hobbit (7)"
        );
    }
}
