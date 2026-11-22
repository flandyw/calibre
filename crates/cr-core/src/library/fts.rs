//! Full-text search over book contents.
//!
//! The index is an SQLite FTS5 database beside `metadata.db`. It is derived
//! data: deleting the file loses nothing, and calibre ignores it.

use std::collections::HashMap;
use std::fs;
use std::time::UNIX_EPOCH;

use rusqlite::{params, Connection};

use super::Library;
use crate::error::Result;
use crate::formats;
use crate::jobs::JobContext;

pub const FTS_FILE: &str = "cr-full-text.db";

/// What an indexing run did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FtsReport {
    /// Books whose text was (re)indexed.
    pub indexed: usize,
    /// Books already up to date.
    pub unchanged: usize,
    /// Books dropped from the index because they left the library.
    pub removed: usize,
    /// Books whose text could not be read, with the reason.
    pub failed: Vec<(i64, String)>,
    pub cancelled: bool,
}

/// A chapter matching a full-text query.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsHit {
    pub book: i64,
    pub title: String,
    /// Chapter number, starting at 1.
    pub chapter: usize,
    pub chapter_title: String,
    /// The matching passage, with matches wrapped in `[` and `]`.
    pub snippet: String,
}

impl Library {
    fn fts_connection(&self) -> Result<Connection> {
        let conn = Connection::open(self.root.join(FTS_FILE))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS indexed (
                 book INTEGER PRIMARY KEY, format TEXT NOT NULL,
                 size INTEGER NOT NULL, mtime INTEGER NOT NULL);
             CREATE VIRTUAL TABLE IF NOT EXISTS chapters USING fts5(
                 title, text, book UNINDEXED, chapter UNINDEXED,
                 tokenize='unicode61 remove_diacritics 2');",
        )?;
        Ok(conn)
    }

    /// Whether a full-text index exists yet.
    pub fn has_fts_index(&self) -> bool {
        self.root.join(FTS_FILE).is_file()
    }

    /// Bring the full-text index up to date. Only books whose file changed
    /// since the last run are read again; each book is committed on its own,
    /// so a cancelled run keeps its progress.
    pub fn fts_index(&self, context: &JobContext) -> Result<FtsReport> {
        self.writable()?;
        let conn = self.fts_connection()?;
        let mut known: HashMap<i64, (String, i64, i64)> = {
            let mut stmt = conn.prepare("SELECT book, format, size, mtime FROM indexed")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let ids: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM books ORDER BY id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let mut report = FtsReport::default();
        for (position, &id) in ids.iter().enumerate() {
            if context.is_cancelled() {
                report.cancelled = true;
                return Ok(report);
            }
            context.report(position, ids.len(), format!("Indexing book {id}"));
            let previous = known.remove(&id);
            let Ok(format) = self.preferred_readable_format(id) else {
                // Nothing readable: make sure no stale text stays behind.
                if previous.is_some() {
                    forget(&conn, id)?;
                    report.removed += 1;
                }
                continue;
            };
            let Some(path) = self.format_path(id, &format)? else {
                continue;
            };
            let stamp = fs::metadata(&path).ok().map(|meta| {
                let modified = meta.modified().ok();
                let seconds = modified
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_secs() as i64);
                (format.clone(), meta.len() as i64, seconds)
            });
            if stamp.is_some() && stamp == previous {
                report.unchanged += 1;
                continue;
            }
            let text = match formats::extract_text(&path, Some(&format)) {
                Ok(text) => text,
                Err(error) => {
                    report.failed.push((id, error.to_string()));
                    continue;
                }
            };
            let (format, size, mtime) = stamp.unwrap_or((format, 0, 0));
            let tx = conn.unchecked_transaction()?;
            forget(&tx, id)?;
            {
                let mut insert = tx.prepare(
                    "INSERT INTO chapters (title, text, book, chapter) VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (number, chapter) in text.chapters.iter().enumerate() {
                    insert.execute(params![chapter.title, chapter.text, id, number as i64 + 1])?;
                }
            }
            tx.execute(
                "INSERT INTO indexed (book, format, size, mtime) VALUES (?1, ?2, ?3, ?4)",
                params![id, format, size, mtime],
            )?;
            tx.commit()?;
            report.indexed += 1;
        }
        // Whatever is left was indexed once but is no longer in the library.
        for id in known.into_keys() {
            forget(&conn, id)?;
            report.removed += 1;
        }
        context.report(ids.len(), ids.len(), "Index up to date");
        Ok(report)
    }

    /// Search book contents, best matches first. Plain words must all appear
    /// in a chapter; FTS5 syntax (`"a phrase"`, `OR`, `NEAR`, `prefix*`) is
    /// used when the query is valid as such.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<FtsHit>> {
        if !self.has_fts_index() || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.fts_connection()?;
        let run = |expression: &str| -> rusqlite::Result<Vec<FtsHit>> {
            let mut stmt = conn.prepare(
                "SELECT book, chapter, title, snippet(chapters, 1, '[', ']', '…', 24)
                 FROM chapters WHERE chapters MATCH ?1 ORDER BY rank",
            )?;
            let mut title = self.conn.prepare("SELECT title FROM books WHERE id=?1")?;
            let mut rows = stmt.query([expression])?;
            let mut hits = Vec::new();
            while hits.len() < limit {
                let Some(row) = rows.next()? else {
                    break;
                };
                let book: i64 = row.get(0)?;
                // Books removed since the last indexing run are skipped, and
                // do not count towards the limit.
                let title: Option<String> = rusqlite::OptionalExtension::optional(
                    title.query_row([book], |row| row.get(0)),
                )?;
                if let Some(title) = title {
                    let snippet: String = row.get(3)?;
                    hits.push(FtsHit {
                        book,
                        title,
                        chapter: row.get::<_, i64>(1)? as usize,
                        chapter_title: row.get(2)?,
                        snippet: snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                    });
                }
            }
            Ok(hits)
        };
        match run(query) {
            Ok(hits) => Ok(hits),
            // Not valid FTS5 syntax: search for the words themselves.
            Err(_) => Ok(run(&quote_words(query))?),
        }
    }
}

fn forget(conn: &Connection, book: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM chapters WHERE book=?1", [book])?;
    conn.execute("DELETE FROM indexed WHERE book=?1", [book])?;
    Ok(())
}

fn quote_words(query: &str) -> String {
    query
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}
