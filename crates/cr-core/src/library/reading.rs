//! Reading state: last position, bookmarks and highlights.
//!
//! It lives in its own SQLite file beside `metadata.db`, so reading never
//! changes a book or calibre's data. calibre's own viewer annotations are
//! read from `metadata.db` and can be copied in, but are never written.

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::Library;
use crate::error::{Error, Result};
use crate::formats::reading::{Book, Locator};

pub const READING_FILE: &str = "cr-reading.db";

/// Where reading stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadingPosition {
    pub locator: Locator,
    /// How far into the book this is, from 0 to 1.
    pub fraction: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Bookmark,
    Highlight,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bookmark => "bookmark",
            Self::Highlight => "highlight",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: i64,
    pub book: i64,
    pub kind: AnnotationKind,
    pub start: Locator,
    /// Byte offset of the end in the same block; equal to the start's
    /// offset for a bookmark.
    pub end: usize,
    /// The highlighted passage, or a bookmark's title.
    pub text: String,
    pub note: String,
    /// The heading the place falls under, for listings.
    pub chapter: String,
    pub created: DateTime<Utc>,
}

/// A bookmark or highlight made in calibre's viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibreAnnotation {
    pub id: String,
    pub kind: AnnotationKind,
    pub format: String,
    /// The highlighted passage, or a bookmark's title.
    pub text: String,
    pub note: String,
    /// Index of the EPUB file the annotation is in.
    pub spine_index: Option<usize>,
    pub created: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationFormat {
    Markdown,
    Json,
}

fn annotation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Annotation> {
    let kind: String = row.get(2)?;
    Ok(Annotation {
        id: row.get(0)?,
        book: row.get(1)?,
        kind: if kind == "bookmark" {
            AnnotationKind::Bookmark
        } else {
            AnnotationKind::Highlight
        },
        start: Locator {
            section: row.get::<_, i64>(3)? as usize,
            block: row.get::<_, i64>(4)? as usize,
            offset: row.get::<_, i64>(5)? as usize,
        },
        end: row.get::<_, i64>(6)? as usize,
        text: row.get(7)?,
        note: row.get(8)?,
        chapter: row.get(9)?,
        created: Utc
            .timestamp_opt(row.get(10)?, 0)
            .single()
            .unwrap_or_default(),
    })
}

const ANNOTATION_COLUMNS: &str =
    "id, book, kind, section, block, start, end, text, note, chapter, created";

impl Library {
    fn reading_connection(&self) -> Result<Connection> {
        let conn = Connection::open(self.root.join(READING_FILE))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS positions (
                 book INTEGER PRIMARY KEY, section INTEGER NOT NULL,
                 block INTEGER NOT NULL, offset INTEGER NOT NULL,
                 fraction REAL NOT NULL, updated INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS annotations (
                 id INTEGER PRIMARY KEY, book INTEGER NOT NULL, kind TEXT NOT NULL,
                 section INTEGER NOT NULL, block INTEGER NOT NULL,
                 start INTEGER NOT NULL, end INTEGER NOT NULL,
                 text TEXT NOT NULL, note TEXT NOT NULL DEFAULT '',
                 chapter TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL,
                 source TEXT UNIQUE);
             CREATE INDEX IF NOT EXISTS annotations_book ON annotations (book);",
        )?;
        Ok(conn)
    }

    fn has_reading_state(&self) -> bool {
        self.root.join(READING_FILE).is_file()
    }

    /// Where reading of a book stopped. Without a saved place, the furthest
    /// place calibre's viewer recorded is used.
    pub fn reading_position(&self, id: i64, book: &Book) -> Result<Option<ReadingPosition>> {
        if self.has_reading_state() {
            let saved = self
                .reading_connection()?
                .query_row(
                    "SELECT section, block, offset, fraction FROM positions WHERE book = ?1",
                    [id],
                    |row| {
                        Ok(ReadingPosition {
                            locator: Locator {
                                section: row.get::<_, i64>(0)? as usize,
                                block: row.get::<_, i64>(1)? as usize,
                                offset: row.get::<_, i64>(2)? as usize,
                            },
                            fraction: row.get(3)?,
                        })
                    },
                )
                .optional()?;
            if let Some(mut saved) = saved {
                // The file may have changed since: fall back to the fraction.
                if book.block(saved.locator).is_none() {
                    saved.locator = book.locate(saved.fraction);
                }
                return Ok(Some(saved));
            }
        }
        let fraction: Option<f64> = self
            .conn
            .query_row(
                "SELECT pos_frac FROM last_read_positions WHERE book = ?1
                 ORDER BY epoch DESC LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(fraction
            .filter(|fraction| *fraction > 0.0)
            .map(|fraction| ReadingPosition {
                locator: book.locate(fraction),
                fraction,
            }))
    }

    pub fn set_reading_position(&self, id: i64, book: &Book, locator: Locator) -> Result<()> {
        self.reading_connection()?.execute(
            "INSERT OR REPLACE INTO positions (book, section, block, offset, fraction, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                locator.section as i64,
                locator.block as i64,
                locator.offset as i64,
                book.fraction(locator),
                Utc::now().timestamp()
            ],
        )?;
        Ok(())
    }

    /// Bookmark a place (`end` = `None`) or highlight the text from
    /// `start` to `end` in one block.
    pub fn add_annotation(
        &self,
        id: i64,
        book: &Book,
        start: Locator,
        end: Option<usize>,
        note: &str,
    ) -> Result<Annotation> {
        let block = book
            .block(start)
            .ok_or_else(|| Error::Other("no such place in the book".into()))?;
        let (kind, end, text) = match end {
            Some(end) => {
                let text = block
                    .text
                    .get(start.offset..end)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| Error::Other("nothing to highlight there".into()))?;
                (AnnotationKind::Highlight, end, text.to_string())
            }
            None => {
                let rest = block.text.get(start.offset..).unwrap_or_default();
                let title: String = rest.chars().take(60).collect();
                (AnnotationKind::Bookmark, start.offset, title)
            }
        };
        let chapter = book
            .toc_index(start)
            .map(|index| book.toc[index].title.clone())
            .unwrap_or_default();
        let conn = self.reading_connection()?;
        conn.execute(
            "INSERT INTO annotations (book, kind, section, block, start, end, text, note, chapter, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                kind.as_str(),
                start.section as i64,
                start.block as i64,
                start.offset as i64,
                end as i64,
                text,
                note,
                chapter,
                Utc::now().timestamp()
            ],
        )?;
        let rowid = conn.last_insert_rowid();
        Ok(conn.query_row(
            &format!("SELECT {ANNOTATION_COLUMNS} FROM annotations WHERE id = ?1"),
            [rowid],
            annotation_from_row,
        )?)
    }

    /// Annotations of one book in reading order, or of every book.
    pub fn annotations(&self, id: Option<i64>) -> Result<Vec<Annotation>> {
        if !self.has_reading_state() {
            return Ok(Vec::new());
        }
        let conn = self.reading_connection()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {ANNOTATION_COLUMNS} FROM annotations
             WHERE ?1 IS NULL OR book = ?1 ORDER BY book, section, block, start, id"
        ))?;
        let rows = statement.query_map([id], annotation_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Annotations whose passage or note contains `query`, in every book.
    pub fn search_annotations(&self, query: &str) -> Result<Vec<Annotation>> {
        let query = query.to_lowercase();
        Ok(self
            .annotations(None)?
            .into_iter()
            .filter(|annotation| {
                annotation.text.to_lowercase().contains(&query)
                    || annotation.note.to_lowercase().contains(&query)
            })
            .collect())
    }

    pub fn set_annotation_note(&self, annotation: i64, note: &str) -> Result<()> {
        let changed = self.reading_connection()?.execute(
            "UPDATE annotations SET note = ?2 WHERE id = ?1",
            params![annotation, note],
        )?;
        if changed == 0 {
            return Err(Error::Other(format!("no annotation {annotation}")));
        }
        Ok(())
    }

    pub fn remove_annotation(&self, annotation: i64) -> Result<()> {
        let changed = self
            .reading_connection()?
            .execute("DELETE FROM annotations WHERE id = ?1", [annotation])?;
        if changed == 0 {
            return Err(Error::Other(format!("no annotation {annotation}")));
        }
        Ok(())
    }

    /// Move a book's annotations to where their passages are now, after the
    /// book's file changed. Returns how many could not be found; those keep
    /// their old place.
    pub fn relocate_annotations(&self, id: i64, book: &Book) -> Result<usize> {
        let mut lost = 0;
        let conn = self.reading_connection()?;
        for annotation in self.annotations(Some(id))? {
            if annotation.kind == AnnotationKind::Bookmark && book.block(annotation.start).is_some()
            {
                continue;
            }
            match book.relocate(&annotation.text, annotation.start) {
                Some(found) if found == annotation.start => {}
                Some(found) => {
                    let length = annotation.end.saturating_sub(annotation.start.offset);
                    conn.execute(
                        "UPDATE annotations SET section = ?2, block = ?3, start = ?4, end = ?5
                         WHERE id = ?1",
                        params![
                            annotation.id,
                            found.section as i64,
                            found.block as i64,
                            found.offset as i64,
                            (found.offset + length) as i64
                        ],
                    )?;
                }
                None => lost += 1,
            }
        }
        Ok(lost)
    }

    /// Bookmarks and highlights calibre's viewer stored for a book.
    pub fn calibre_annotations(&self, id: i64) -> Result<Vec<CalibreAnnotation>> {
        let mut statement = self.conn.prepare(
            "SELECT annot_id, annot_type, format, annot_data, timestamp FROM annotations
             WHERE book = ?1 AND annot_type IN ('bookmark', 'highlight') ORDER BY timestamp",
        )?;
        let rows = statement.query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        let mut annotations = Vec::new();
        for row in rows {
            let (id, kind, format, data, timestamp) = row?;
            let data: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
            if data["removed"].as_bool() == Some(true) {
                continue;
            }
            let field = |name: &str| data[name].as_str().unwrap_or_default().to_string();
            let kind = if kind == "bookmark" {
                AnnotationKind::Bookmark
            } else {
                AnnotationKind::Highlight
            };
            annotations.push(CalibreAnnotation {
                id,
                kind,
                format,
                text: match kind {
                    AnnotationKind::Bookmark => field("title"),
                    AnnotationKind::Highlight => field("highlighted_text"),
                },
                note: field("notes"),
                spine_index: data["spine_index"].as_u64().map(|index| index as usize),
                created: Utc.timestamp_opt(timestamp as i64, 0).single(),
            });
        }
        Ok(annotations)
    }

    /// Copy calibre's highlights of a book into the reading state, placing
    /// each by its text. Returns how many were added and how many could not
    /// be found in the book. Running it again adds nothing twice.
    pub fn import_calibre_annotations(&self, id: i64, book: &Book) -> Result<(usize, usize)> {
        let (mut added, mut missing) = (0, 0);
        let conn = self.reading_connection()?;
        for annotation in self.calibre_annotations(id)? {
            if annotation.kind != AnnotationKind::Highlight {
                continue;
            }
            // calibre joins a highlight's paragraphs; a block holds one.
            let first = annotation.text.lines().next().unwrap_or_default().trim();
            let near = Locator {
                section: annotation.spine_index.unwrap_or(0),
                ..Locator::default()
            };
            let Some(start) = book.relocate(first, near).filter(|_| !first.is_empty()) else {
                missing += 1;
                continue;
            };
            let chapter = book
                .toc_index(start)
                .map(|index| book.toc[index].title.clone())
                .unwrap_or_default();
            added += conn.execute(
                "INSERT OR IGNORE INTO annotations
                 (book, kind, section, block, start, end, text, note, chapter, created, source)
                 VALUES (?1, 'highlight', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    start.section as i64,
                    start.block as i64,
                    start.offset as i64,
                    (start.offset + first.len()) as i64,
                    first,
                    annotation.note,
                    chapter,
                    annotation.created.unwrap_or_else(Utc::now).timestamp(),
                    format!("calibre:{id}:{}", annotation.id)
                ],
            )?;
        }
        Ok((added, missing))
    }

    /// Annotations of one book, or of every book, as Markdown or JSON.
    pub fn export_annotations(&self, id: Option<i64>, format: AnnotationFormat) -> Result<String> {
        let annotations = self.annotations(id)?;
        let mut titles = std::collections::HashMap::new();
        for annotation in &annotations {
            if !titles.contains_key(&annotation.book) {
                let title = self
                    .get_metadata(annotation.book)
                    .map(|metadata| metadata.title)
                    .unwrap_or_else(|_| format!("Book {}", annotation.book));
                titles.insert(annotation.book, title);
            }
        }
        if format == AnnotationFormat::Json {
            let items: Vec<Value> = annotations
                .iter()
                .map(|annotation| {
                    json!({
                        "id": annotation.id,
                        "book_id": annotation.book,
                        "book": titles[&annotation.book],
                        "type": annotation.kind.as_str(),
                        "chapter": annotation.chapter,
                        "text": annotation.text,
                        "note": annotation.note,
                        "section": annotation.start.section,
                        "block": annotation.start.block,
                        "start": annotation.start.offset,
                        "end": annotation.end,
                        "created": annotation.created.to_rfc3339(),
                    })
                })
                .collect();
            return Ok(serde_json::to_string_pretty(&items).unwrap_or_default());
        }
        let mut output = String::new();
        let mut current = None;
        for annotation in &annotations {
            if current != Some(annotation.book) {
                current = Some(annotation.book);
                output.push_str(&format!("# {}\n\n", titles[&annotation.book]));
            }
            let place = if annotation.chapter.is_empty() {
                String::new()
            } else {
                format!(" ({})", annotation.chapter)
            };
            match annotation.kind {
                AnnotationKind::Highlight => {
                    output.push_str(&format!("> {}\n\n", annotation.text.replace('\n', "\n> ")));
                    if !place.is_empty() {
                        output.push_str(&format!("—{place}\n\n"));
                    }
                }
                AnnotationKind::Bookmark => {
                    output.push_str(&format!("- Bookmark{place}: {}\n\n", annotation.text));
                }
            }
            if !annotation.note.is_empty() {
                output.push_str(&format!("Note: {}\n\n", annotation.note));
            }
        }
        Ok(output)
    }

    /// Lay out a book of the library for reading.
    pub fn open_book(&self, id: i64, format: Option<&str>) -> Result<Book> {
        let format = match format {
            Some(format) => format.to_ascii_uppercase(),
            None => {
                let have = self.formats(id)?;
                crate::formats::INPUT_FORMATS
                    .iter()
                    .find(|format| have.iter().any(|have| have == *format))
                    .map(|format| format.to_string())
                    .ok_or_else(|| Error::Other(format!("book {id} has no readable format")))?
            }
        };
        let path = self
            .format_path(id, &format)?
            .ok_or_else(|| Error::Other(format!("book {id} has no {format} file")))?;
        let mut book = Book::open(&path)?;
        if let Ok(metadata) = self.get_metadata(id) {
            book.title = metadata.title;
        }
        Ok(book)
    }
}
