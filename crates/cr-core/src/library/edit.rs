//! Saving edited books back into the library.

use std::path::Path;

use super::Library;
use crate::error::{Error, Result};
use crate::formats::editing::{self, Change, EpubDir, Issue, Severity};

/// What saving an edited book did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The library copy was replaced; the first replacement of a book keeps
    /// the old file as `ORIGINAL_EPUB`.
    Saved { changes: Vec<Change> },
    /// The edited book has the same content as the library copy.
    Unchanged,
    /// Errors that would make the book unreadable; nothing was written.
    Invalid { errors: Vec<Issue> },
}

impl Library {
    /// Replace a book's EPUB with an edited, unpacked copy. The book is
    /// checked first and refused if it has errors, unless `force` is set.
    pub fn save_edited_book(
        &mut self,
        id: i64,
        directory: &Path,
        force: bool,
    ) -> Result<SaveOutcome> {
        self.writable()?;
        let book = EpubDir::open(directory)?;
        if !force {
            let errors: Vec<Issue> = book
                .check()
                .into_iter()
                .filter(|issue| issue.severity == Severity::Error)
                .collect();
            if !errors.is_empty() {
                return Ok(SaveOutcome::Invalid { errors });
            }
        }
        let scratch = tempfile::tempdir()?;
        let packed = scratch.path().join("book.epub");
        editing::save(directory, &packed)?;
        let current = self.format_path(id, "EPUB")?;
        let changes = match &current {
            Some(current) => editing::compare(current, &packed)?,
            None => Vec::new(),
        };
        if current.is_some() && changes.is_empty() {
            return Ok(SaveOutcome::Unchanged);
        }
        if let Some(current) = &current {
            let have = self.formats(id)?;
            if !have.iter().any(|format| format == "ORIGINAL_EPUB") {
                self.replace_format(id, "ORIGINAL_EPUB", current)?;
            }
        }
        self.replace_format(id, "EPUB", &packed)?;
        Ok(SaveOutcome::Saved { changes })
    }

    /// What editing changed: the kept original against the current file.
    pub fn compare_with_original(&self, id: i64, format: &str) -> Result<Vec<Change>> {
        let format = format.to_ascii_uppercase();
        let original = self
            .format_path(id, &format!("ORIGINAL_{format}"))?
            .ok_or_else(|| Error::Other(format!("book {id} has no ORIGINAL_{format}")))?;
        let current = self
            .format_path(id, &format)?
            .ok_or_else(|| Error::Other(format!("book {id} has no {format} file")))?;
        editing::compare(&original, &current)
    }

    /// Put the kept original back in place of the edited or converted file.
    pub fn restore_original(&mut self, id: i64, format: &str) -> Result<()> {
        self.writable()?;
        let format = format.to_ascii_uppercase();
        let backup = format!("ORIGINAL_{format}");
        let original = self
            .format_path(id, &backup)?
            .ok_or_else(|| Error::Other(format!("book {id} has no {backup}")))?;
        self.replace_format(id, &format, &original)?;
        self.remove_format(id, &backup)?;
        Ok(())
    }
}
