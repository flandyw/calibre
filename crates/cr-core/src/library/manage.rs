//! Everyday library management on top of the core engine: importing with
//! duplicate handling, extra formats, bulk edits, the trash, transfers
//! between libraries and export options.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use regex::{Regex, RegexBuilder};
use rusqlite::params;

use super::{
    copy_atomic, safe_join, unique_path, write_atomic, AddResult, BookUpdate, Library, NewBook,
    COVER_FILE, METADATA_FILE,
};
use crate::error::{Error, Result};
use crate::formats;
use crate::jobs::{self, ItemResult, JobContext};
use crate::metadata::Metadata;
use crate::opf;
use crate::text;

/// Where removed books and formats wait until they are purged. The layout
/// (`.caltrash/b/<id>/`, `.caltrash/f/<id>/`) is calibre's.
pub const TRASH_DIR: &str = ".caltrash";
/// Per-book directory for extra files, as calibre names it.
pub const DATA_DIR: &str = "data";

/// File extensions picked up when scanning folders for books.
pub const IMPORTABLE_EXTENSIONS: [&str; 22] = [
    "epub", "azw3", "azw", "mobi", "prc", "kf8", "fb2", "txt", "html", "htm", "xhtml", "pdf",
    "docx", "rtf", "odt", "djvu", "cbz", "cbr", "lit", "pdb", "kfx", "md",
];

// ---------------------------------------------------------------------------
// Importing
// ---------------------------------------------------------------------------

/// What to do when an imported file looks like a book already in the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuplicatePolicy {
    /// Always create a new record.
    #[default]
    Add,
    /// Leave the library unchanged and report the matching books.
    Skip,
    /// Add the file as another format of the matching book; a format the
    /// book already has is left alone.
    Merge,
}

impl DuplicatePolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "add" | "keep" => Some(Self::Add),
            "skip" | "ignore" => Some(Self::Skip),
            "merge" => Some(Self::Merge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub duplicates: DuplicatePolicy,
    /// Read metadata from the file name instead of the file. Named groups:
    /// `title`, `author`, `series`, `series_index`.
    pub filename_pattern: Option<Regex>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AddOutcome {
    Added(AddResult),
    /// The file went into an existing book. `added` is false when that book
    /// already had the format.
    Merged {
        id: i64,
        format: String,
        added: bool,
    },
    /// Nothing changed; these books match.
    Skipped {
        existing: Vec<i64>,
    },
}

impl AddOutcome {
    /// The book this file ended up in, if any.
    pub fn book_id(&self) -> Option<i64> {
        match self {
            Self::Added(book) => Some(book.id),
            Self::Merged { id, .. } => Some(*id),
            Self::Skipped { .. } => None,
        }
    }
}

/// Expand folders into the book files inside them. Files given directly are
/// kept whatever their extension.
pub fn scan_sources(paths: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in paths {
        if !path.is_dir() {
            found.push(path.clone());
            continue;
        }
        let depth = if recursive { usize::MAX } else { 1 };
        let mut inside: Vec<PathBuf> = walkdir::WalkDir::new(path)
            .max_depth(depth)
            .into_iter()
            .filter_entry(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file() && is_importable(entry.path()))
            .map(|entry| entry.into_path())
            .collect();
        inside.sort();
        found.extend(inside);
    }
    found
}

fn is_importable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMPORTABLE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Title reduced for duplicate matching: lower case, no punctuation, no
/// leading article.
fn fuzzy_title(title: &str) -> String {
    let lowered: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = lowered.split_whitespace().collect();
    if words.len() > 1 && matches!(words[0], "a" | "an" | "the") {
        words.remove(0);
    }
    words.join(" ")
}

fn author_key(authors: &[String]) -> BTreeSet<String> {
    authors
        .iter()
        .map(|author| {
            author
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .filter(|author| !author.is_empty() && author != "unknown")
        .collect()
}

fn apply_filename_pattern(pattern: &Regex, source: &Path, meta: &mut Metadata) {
    let Some(stem) = source.file_stem().map(|stem| stem.to_string_lossy()) else {
        return;
    };
    let Some(captures) = pattern.captures(&stem) else {
        return;
    };
    let group = |name: &str| {
        captures
            .name(name)
            .map(|found| found.as_str().trim().to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(title) = group("title") {
        meta.title = title;
        meta.title_sort = None;
    }
    if let Some(author) = group("author") {
        meta.authors = text::split_authors(&author);
        meta.author_sort = None;
    }
    if let Some(series) = group("series") {
        meta.series = Some(series);
    }
    if let Some(index) = group("series_index").and_then(|value| text::parse_series_index(&value)) {
        meta.series_index = index;
    }
}

impl Library {
    /// Books with the same authors and an equivalent title.
    pub fn find_duplicates(&self, title: &str, authors: &[String]) -> Result<Vec<i64>> {
        let wanted_title = fuzzy_title(title);
        let wanted_authors = author_key(authors);
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.title,
                    (SELECT GROUP_CONCAT(a.name, '\u{1f}') FROM books_authors_link l
                     JOIN authors a ON a.id=l.author WHERE l.book=b.id)
             FROM books b ORDER BY b.id",
        )?;
        let mut rows = stmt.query([])?;
        let mut matches = Vec::new();
        while let Some(row) = rows.next()? {
            let candidate: String = row.get(1)?;
            if fuzzy_title(&candidate) != wanted_title {
                continue;
            }
            let names: Option<String> = row.get(2)?;
            let names: Vec<String> = names
                .unwrap_or_default()
                .split('\u{1f}')
                .map(|name| name.replace('|', ","))
                .collect();
            if author_key(&names) == wanted_authors {
                matches.push(row.get(0)?);
            }
        }
        Ok(matches)
    }

    /// Add one file, applying the duplicate policy.
    pub fn import_book(&mut self, source: &Path, options: &ImportOptions) -> Result<AddOutcome> {
        self.writable()?;
        let imported = formats::import(source)?;
        let mut meta = imported.metadata;
        if let Some(pattern) = &options.filename_pattern {
            apply_filename_pattern(pattern, source, &mut meta);
        }
        if options.duplicates != DuplicatePolicy::Add {
            let existing = self.find_duplicates(&meta.title, &meta.authors)?;
            if let Some(&id) = existing.first() {
                if options.duplicates == DuplicatePolicy::Skip {
                    return Ok(AddOutcome::Skipped { existing });
                }
                let added = self.add_format(id, source, false)?;
                return Ok(AddOutcome::Merged {
                    id,
                    format: imported.format,
                    added,
                });
            }
        }
        let files = [(imported.format, source.to_path_buf())];
        self.insert_book(meta, &files, NewBook::default())
            .map(AddOutcome::Added)
    }

    /// Import many files as one job with a result per file.
    pub fn import_books(
        &mut self,
        sources: &[PathBuf],
        options: &ImportOptions,
        context: &JobContext,
    ) -> Vec<ItemResult<PathBuf, AddOutcome>> {
        jobs::for_each(
            context,
            sources.iter().cloned(),
            |path| format!("Adding {}", super::file_label(path)),
            |path| self.import_book(path, options),
        )
    }

    /// Create a record with metadata only, to attach formats to later.
    pub fn add_empty(&mut self, meta: Metadata) -> Result<AddResult> {
        self.insert_book(meta, &[], NewBook::default())
    }

    // -----------------------------------------------------------------------
    // Formats
    // -----------------------------------------------------------------------

    /// Attach a file to a book as another format. Returns false (and changes
    /// nothing) when the book already has that format and `replace` is off.
    pub fn add_format(&mut self, id: i64, source: &Path, replace: bool) -> Result<bool> {
        self.writable()?;
        if !source.is_file() {
            return Err(Error::Other(format!("not a file: {}", source.display())));
        }
        let format = formats::detect_format(source);
        if !replace && self.formats(id)?.iter().any(|have| *have == format) {
            return Ok(false);
        }
        self.replace_format(id, &format, source)?;
        Ok(true)
    }

    /// Remove one format from a book, keeping the file in the trash.
    pub fn remove_format(&mut self, id: i64, format: &str) -> Result<()> {
        self.writable()?;
        let format = format.to_ascii_uppercase();
        let path = self.format_path(id, &format)?;
        let changed = self.conn.execute(
            "DELETE FROM data WHERE book=?1 AND format=?2 COLLATE NOCASE",
            params![id, format],
        )?;
        if changed == 0 {
            self.ensure_exists(id)?;
            return Err(Error::Other(format!("book {id} has no {format} format")));
        }
        if let Some(path) = path {
            let trash = self.trash_dir('f', id);
            fs::create_dir_all(&trash)?;
            let target = trash.join(format!("{}.{}", id, format.to_ascii_lowercase()));
            if fs::rename(&path, &target).is_err() {
                copy_atomic(&path, &target)?;
                fs::remove_file(&path)?;
            }
        }
        self.sync_book(id)
    }

    // -----------------------------------------------------------------------
    // Trash
    // -----------------------------------------------------------------------

    fn trash_dir(&self, kind: char, id: i64) -> PathBuf {
        self.root
            .join(TRASH_DIR)
            .join(kind.to_string())
            .join(id.to_string())
    }

    /// Remove a book from the library, keeping its files and metadata in the
    /// trash so it can be restored.
    pub fn trash_book(&mut self, id: i64) -> Result<()> {
        self.writable()?;
        self.sync_book(id)?;
        let relative = self.book_path(id)?;
        let directory = safe_join(&self.root, &relative)?;
        let trash = self.trash_dir('b', id);
        if trash.exists() {
            fs::remove_dir_all(&trash)?;
        }
        if let Some(parent) = trash.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&directory, &trash)?;
        if let Err(error) = self.remove_book(id, false) {
            let _ = fs::rename(&trash, &directory);
            return Err(error);
        }
        super::remove_empty_parent(&directory);
        Ok(())
    }

    /// Books and single formats waiting in the trash, newest first.
    pub fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        let mut entries = Vec::new();
        for (kind, is_book) in [('b', true), ('f', false)] {
            let directory = self.root.join(TRASH_DIR).join(kind.to_string());
            let Ok(children) = fs::read_dir(&directory) else {
                continue;
            };
            for child in children.filter_map(|child| child.ok()) {
                let path = child.path();
                let Some(id) = child.file_name().to_string_lossy().parse::<i64>().ok() else {
                    continue;
                };
                if !path.is_dir() {
                    continue;
                }
                let meta = fs::read(path.join(METADATA_FILE))
                    .ok()
                    .and_then(|bytes| opf::parse_opf(&bytes).ok());
                let (title, authors) = match meta {
                    Some(meta) => (meta.title, meta.authors),
                    None if is_book => ("Unknown".to_string(), Vec::new()),
                    None => match self.get_metadata(id) {
                        Ok(meta) => (meta.title, meta.authors),
                        Err(_) => ("Unknown".to_string(), Vec::new()),
                    },
                };
                let formats: Vec<String> = format_files(&path)
                    .into_iter()
                    .map(|(format, _)| format)
                    .collect();
                if !is_book && formats.is_empty() {
                    continue;
                }
                entries.push(TrashEntry {
                    id,
                    is_book,
                    title,
                    authors,
                    formats,
                    deleted: child.metadata().and_then(|meta| meta.modified()).ok(),
                });
            }
        }
        entries.sort_by(|a, b| b.deleted.cmp(&a.deleted).then(a.id.cmp(&b.id)));
        Ok(entries)
    }

    /// Bring a trashed book back with its formats, cover, extra files and,
    /// when still free, its id.
    pub fn restore_book(&mut self, id: i64) -> Result<AddResult> {
        self.writable()?;
        let trash = self.trash_dir('b', id);
        if !trash.is_dir() {
            return Err(Error::Other(format!("book {id} is not in the trash")));
        }
        let mut meta = opf::parse_opf(&fs::read(trash.join(METADATA_FILE))?)?;
        meta.cover = fs::read(trash.join(COVER_FILE)).ok();
        let files = format_files(&trash);
        let options = NewBook {
            id: self.ensure_exists(id).is_err().then_some(id),
            keep_uuid: true,
        };
        let restored = self.insert_book(meta, &files, options)?;
        let extras = trash.join(DATA_DIR);
        if extras.is_dir() {
            let target = safe_join(&self.root, &restored.path)?.join(DATA_DIR);
            if fs::rename(&extras, &target).is_err() {
                copy_tree(&extras, &target)?;
            }
        }
        fs::remove_dir_all(&trash)?;
        Ok(restored)
    }

    /// Put a trashed format back on its book.
    pub fn restore_format(&mut self, id: i64, format: &str) -> Result<()> {
        self.writable()?;
        let trash = self.trash_dir('f', id);
        let file = format_files(&trash)
            .into_iter()
            .find(|(have, _)| have.eq_ignore_ascii_case(format))
            .map(|(_, path)| path)
            .ok_or_else(|| Error::Other(format!("no {format} of book {id} in the trash")))?;
        self.replace_format(id, &format.to_ascii_uppercase(), &file)?;
        fs::remove_file(&file)?;
        let _ = fs::remove_dir(&trash);
        Ok(())
    }

    /// Permanently delete one trashed book (and its trashed formats), or
    /// everything in the trash. Returns how many entries were deleted.
    pub fn purge_trash(&mut self, id: Option<i64>) -> Result<usize> {
        self.writable()?;
        let mut removed = 0;
        for kind in ['b', 'f'] {
            let directory = self.root.join(TRASH_DIR).join(kind.to_string());
            let Ok(children) = fs::read_dir(&directory) else {
                continue;
            };
            for child in children.filter_map(|child| child.ok()) {
                let name = child.file_name().to_string_lossy().to_string();
                if id.is_some_and(|id| id.to_string() != name) {
                    continue;
                }
                fs::remove_dir_all(child.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    // -----------------------------------------------------------------------
    // Extra data files
    // -----------------------------------------------------------------------

    /// Extra files stored with a book, relative to its `data/` directory.
    pub fn extra_files(&self, id: i64) -> Result<Vec<String>> {
        let directory = safe_join(&self.root, &self.book_path(id)?)?.join(DATA_DIR);
        let mut names: Vec<String> = walkdir::WalkDir::new(&directory)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| {
                let relative = entry.path().strip_prefix(&directory).ok()?;
                Some(relative.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Store a file alongside a book. Returns its name inside `data/`.
    pub fn add_extra_file(&mut self, id: i64, source: &Path) -> Result<String> {
        self.writable()?;
        let name = source
            .file_name()
            .map(|name| text::sanitize_component(&name.to_string_lossy()))
            .ok_or_else(|| Error::Other(format!("not a file: {}", source.display())))?;
        let directory = safe_join(&self.root, &self.book_path(id)?)?.join(DATA_DIR);
        fs::create_dir_all(&directory)?;
        copy_atomic(source, &directory.join(&name))?;
        Ok(name)
    }

    pub fn remove_extra_file(&mut self, id: i64, name: &str) -> Result<()> {
        self.writable()?;
        let directory = safe_join(&self.root, &self.book_path(id)?)?.join(DATA_DIR);
        fs::remove_file(safe_join(&directory, name)?)?;
        let _ = fs::remove_dir(&directory);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Transfers between libraries
    // -----------------------------------------------------------------------

    /// Copy a book with all its formats, cover, metadata and extra files into
    /// another library.
    pub fn copy_book_to(
        &self,
        id: i64,
        target: &mut Library,
        duplicates: DuplicatePolicy,
    ) -> Result<AddOutcome> {
        target.writable()?;
        if self.root == target.root {
            return Err(Error::Other(
                "source and target library are the same".into(),
            ));
        }
        let meta = self.get_metadata(id)?;
        let mut files = Vec::new();
        for format in self.formats(id)? {
            if let Some(path) = self.format_path(id, &format)? {
                files.push((format, path));
            }
        }
        if duplicates != DuplicatePolicy::Add {
            let existing = target.find_duplicates(&meta.title, &meta.authors)?;
            if let Some(&into) = existing.first() {
                if duplicates == DuplicatePolicy::Skip {
                    return Ok(AddOutcome::Skipped { existing });
                }
                let mut added = Vec::new();
                for (format, path) in &files {
                    if target.add_format(into, path, false)? {
                        added.push(format.clone());
                    }
                }
                return Ok(AddOutcome::Merged {
                    id: into,
                    added: !added.is_empty(),
                    format: added.join(", "),
                });
            }
        }
        let options = NewBook {
            id: None,
            keep_uuid: true,
        };
        let copied = target.insert_book(meta, &files, options)?;
        let extras = safe_join(&self.root, &self.book_path(id)?)?.join(DATA_DIR);
        if extras.is_dir() {
            copy_tree(
                &extras,
                &safe_join(&target.root, &copied.path)?.join(DATA_DIR),
            )?;
        }
        Ok(AddOutcome::Added(copied))
    }

    /// Copy (or, with `remove_source`, move) books to another library as one
    /// job. Moved books go to this library's trash.
    pub fn transfer_books(
        &mut self,
        ids: &[i64],
        target: &mut Library,
        duplicates: DuplicatePolicy,
        remove_source: bool,
        context: &JobContext,
    ) -> Vec<ItemResult<i64, AddOutcome>> {
        if remove_source {
            if let Err(error) = self.writable() {
                let message = error.to_string();
                return ids
                    .iter()
                    .map(|&item| ItemResult {
                        item,
                        result: Err(Error::ReadOnly(message.clone())),
                    })
                    .collect();
            }
        }
        jobs::for_each(
            context,
            ids.iter().copied(),
            |id| format!("Copying book {id}"),
            |id| {
                let outcome = self.copy_book_to(*id, target, duplicates)?;
                if remove_source && !matches!(outcome, AddOutcome::Skipped { .. }) {
                    self.trash_book(*id)?;
                }
                Ok(outcome)
            },
        )
    }

    // -----------------------------------------------------------------------
    // Bulk metadata
    // -----------------------------------------------------------------------

    /// The change a bulk edit makes to one book. `position` is the book's
    /// place in the selection, used to number a series.
    pub fn bulk_patch(&self, id: i64, position: usize, edit: &BulkEdit) -> Result<BookUpdate> {
        let meta = self.get_metadata(id)?;
        let mut patch = BookUpdate::default();
        let mut after = meta.clone();

        if let Some(authors) = &edit.authors {
            after.authors = authors.clone();
        }
        if let Some(publisher) = &edit.publisher {
            after.publisher = publisher.clone();
        }
        if let Some(series) = &edit.series {
            after.series = series.clone();
        }
        if let Some(start) = edit.series_start {
            after.series_index = start + position as f64;
        }
        if let Some(rating) = &edit.rating {
            after.rating = *rating;
        }
        if let Some(languages) = &edit.languages {
            after.languages = languages.clone();
        }
        if edit.clear_tags {
            after.tags.clear();
        }
        after.tags.retain(|tag| {
            !edit
                .remove_tags
                .iter()
                .any(|remove| remove.eq_ignore_ascii_case(tag))
        });
        for tag in &edit.add_tags {
            if !after.tags.iter().any(|have| have.eq_ignore_ascii_case(tag)) {
                after.tags.push(tag.clone());
            }
        }
        if let Some((from, to)) = edit.copy {
            let value = from.read(&after);
            to.write(&mut after, &value)?;
        }
        if let Some(replace) = &edit.replace {
            let before = replace.field.read(&after);
            let value = replace.apply(&before)?;
            replace.field.write(&mut after, &value)?;
        }

        if after.title != meta.title {
            patch.title = Some(after.title);
        }
        if after.authors != meta.authors {
            patch.authors = Some(after.authors);
        }
        if after.publisher != meta.publisher {
            patch.publisher = Some(after.publisher);
        }
        if after.series != meta.series {
            patch.series = Some(after.series);
        }
        if after.series_index != meta.series_index {
            patch.series_index = Some(after.series_index);
        }
        if after.tags != meta.tags {
            patch.tags = Some(after.tags);
        }
        if after.rating != meta.rating {
            patch.rating = Some(after.rating);
        }
        if after.comments != meta.comments {
            patch.comments = Some(after.comments);
        }
        if after.languages != meta.languages {
            patch.languages = Some(after.languages);
        }
        Ok(patch)
    }

    /// Apply one edit to many books. Each book is its own transaction; the
    /// result says whether that book changed.
    pub fn bulk_update(
        &mut self,
        ids: &[i64],
        edit: &BulkEdit,
        context: &JobContext,
    ) -> Vec<ItemResult<i64, bool>> {
        let mut position = 0;
        jobs::for_each(
            context,
            ids.iter().copied(),
            |id| format!("Editing book {id}"),
            |id| {
                let patch = self.bulk_patch(*id, position, edit)?;
                position += 1;
                if patch == BookUpdate::default() {
                    return Ok(false);
                }
                self.update_book(*id, &patch)?;
                Ok(true)
            },
        )
    }

    // -----------------------------------------------------------------------
    // Export
    // -----------------------------------------------------------------------

    /// Copy one book out of the library. Returns the files written.
    pub fn export_book(
        &self,
        id: i64,
        destination: &Path,
        options: &ExportOptions,
    ) -> Result<Vec<PathBuf>> {
        let meta = self.get_metadata(id)?;
        let rendered = super::render_template(&options.template, &meta, id);
        let mut base = destination.to_path_buf();
        for component in rendered.split('/').filter(|part| !part.is_empty()) {
            base.push(text::sanitize_component(component));
        }
        let stem = base
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let place = |extension: &str| -> Option<PathBuf> {
            let target = base.with_file_name(format!("{stem}.{extension}"));
            match options.collision {
                Collision::Rename => Some(unique_path(target)),
                Collision::Overwrite => Some(target),
                Collision::Skip => (!target.exists()).then_some(target),
            }
        };
        if let Some(parent) = base.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut written = Vec::new();
        for format in self.formats(id)? {
            let wanted = options.formats.is_empty()
                || options
                    .formats
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(&format));
            if !wanted {
                continue;
            }
            let Some(source) = self.format_path(id, &format)? else {
                continue;
            };
            if let Some(target) = place(&format.to_ascii_lowercase()) {
                copy_atomic(&source, &target)?;
                written.push(target);
            }
        }
        if options.cover {
            if let (Some(cover), Some(target)) = (&meta.cover, place("jpg")) {
                write_atomic(&target, cover)?;
                written.push(target);
            }
        }
        if options.opf {
            if let Some(target) = place("opf") {
                write_atomic(&target, opf::metadata_to_opf(&meta).as_bytes())?;
                written.push(target);
            }
        }
        Ok(written)
    }

    /// [`Library::export_book`] for many books as a job.
    pub fn export_with(
        &self,
        ids: &[i64],
        destination: &Path,
        options: &ExportOptions,
        context: &JobContext,
    ) -> Vec<ItemResult<i64, Vec<PathBuf>>> {
        jobs::for_each(
            context,
            ids.iter().copied(),
            |id| format!("Exporting book {id}"),
            |id| self.export_book(*id, destination, options),
        )
    }
}

/// Something in the trash: a whole book, or formats removed from a book.
#[derive(Debug, Clone, PartialEq)]
pub struct TrashEntry {
    pub id: i64,
    pub is_book: bool,
    pub title: String,
    pub authors: Vec<String>,
    pub formats: Vec<String>,
    pub deleted: Option<SystemTime>,
}

/// Format files directly inside a directory, as `(FORMAT, path)`.
fn format_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let Ok(children) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = children
        .filter_map(|child| child.ok())
        .map(|child| child.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let name = path.file_name().map(|name| name.to_string_lossy());
            !matches!(name.as_deref(), Some(COVER_FILE) | Some(METADATA_FILE))
        })
        .filter_map(|path| {
            let extension = path.extension()?.to_str()?.to_ascii_uppercase();
            Some((extension, path))
        })
        .collect();
    files.sort();
    files
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.map_err(|error| Error::Other(error.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|error| Error::Other(error.to_string()))?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            copy_atomic(entry.path(), &destination)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bulk edit description
// ---------------------------------------------------------------------------

/// A text field a bulk search-and-replace or copy can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Title,
    Authors,
    Publisher,
    Series,
    Tags,
    Comments,
}

impl TextField {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "title" => Some(Self::Title),
            "author" | "authors" => Some(Self::Authors),
            "publisher" => Some(Self::Publisher),
            "series" => Some(Self::Series),
            "tag" | "tags" => Some(Self::Tags),
            "comment" | "comments" => Some(Self::Comments),
            _ => None,
        }
    }

    fn read(self, meta: &Metadata) -> String {
        match self {
            Self::Title => meta.title.clone(),
            Self::Authors => meta.authors.join(" & "),
            Self::Publisher => meta.publisher.clone().unwrap_or_default(),
            Self::Series => meta.series.clone().unwrap_or_default(),
            Self::Tags => meta.tags.join(", "),
            Self::Comments => meta.comments.clone().unwrap_or_default(),
        }
    }

    fn write(self, meta: &mut Metadata, value: &str) -> Result<()> {
        let value = value.trim();
        let optional = (!value.is_empty()).then(|| value.to_string());
        match self {
            Self::Title => {
                if value.is_empty() {
                    return Err(Error::InvalidMetadata("title cannot be empty".into()));
                }
                meta.title = value.to_string();
            }
            Self::Authors => meta.authors = text::split_authors(value),
            Self::Publisher => meta.publisher = optional,
            Self::Series => meta.series = optional,
            Self::Tags => {
                meta.tags = value
                    .split(',')
                    .map(|tag| tag.trim().to_string())
                    .filter(|tag| !tag.is_empty())
                    .collect()
            }
            Self::Comments => meta.comments = optional,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchReplace {
    pub field: TextField,
    pub pattern: String,
    pub replacement: String,
    /// Treat `pattern` as a regular expression (`$1` in the replacement).
    pub regex: bool,
    pub ignore_case: bool,
}

impl SearchReplace {
    fn apply(&self, value: &str) -> Result<String> {
        let pattern = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        let matcher = RegexBuilder::new(&pattern)
            .case_insensitive(self.ignore_case)
            .build()
            .map_err(|error| Error::InvalidMetadata(format!("bad pattern: {error}")))?;
        let replaced = if self.regex {
            matcher.replace_all(value, self.replacement.as_str())
        } else {
            matcher.replace_all(value, regex::NoExpand(&self.replacement))
        };
        Ok(replaced.into_owned())
    }
}

/// One edit applied to every selected book. Unset fields are left alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BulkEdit {
    pub authors: Option<Vec<String>>,
    pub publisher: Option<Option<String>>,
    pub series: Option<Option<String>>,
    /// Number the books in selection order starting here.
    pub series_start: Option<f64>,
    pub rating: Option<Option<u8>>,
    pub languages: Option<Vec<String>>,
    pub clear_tags: bool,
    pub remove_tags: Vec<String>,
    pub add_tags: Vec<String>,
    /// Copy one field's text into another: `(from, to)`.
    pub copy: Option<(TextField, TextField)>,
    pub replace: Option<SearchReplace>,
}

// ---------------------------------------------------------------------------
// Export description
// ---------------------------------------------------------------------------

/// What to do when an exported file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Collision {
    /// Write next to it as `Name (2).ext`.
    #[default]
    Rename,
    Overwrite,
    Skip,
}

impl Collision {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "rename" => Some(Self::Rename),
            "overwrite" | "replace" => Some(Self::Overwrite),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    /// Path template; see [`Library::export_books`] for placeholders.
    pub template: String,
    /// Formats to export; empty means all.
    pub formats: Vec<String>,
    pub cover: bool,
    pub opf: bool,
    pub collision: Collision,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            template: "{author}/{title}/{title} - {author}".to_string(),
            formats: Vec::new(),
            cover: false,
            opf: false,
            collision: Collision::Rename,
        }
    }
}
