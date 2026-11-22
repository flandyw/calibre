//! Organising a library: saved searches, virtual libraries, the category
//! browser and custom columns. Everything is stored the way calibre stores
//! it, so both programs see the same data.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};

use super::search::{CustomField, SearchContext};
use super::Library;
use crate::error::{Error, Result};

const SAVED_SEARCHES: &str = "saved_searches";
const VIRTUAL_LIBRARIES: &str = "virtual_libraries";

/// The kinds of value a custom column can hold (calibre's datatypes).
pub const CUSTOM_DATATYPES: [&str; 9] = [
    "text",
    "comments",
    "series",
    "enumeration",
    "rating",
    "int",
    "float",
    "bool",
    "datetime",
];

#[derive(Debug, Clone, PartialEq)]
pub struct CustomColumn {
    pub id: i64,
    /// The lookup name without `#`.
    pub label: String,
    /// The heading shown to people.
    pub name: String,
    pub datatype: String,
    pub is_multiple: bool,
    /// Allowed values of an enumeration column.
    pub choices: Vec<String>,
}

impl CustomColumn {
    fn field(&self) -> CustomField {
        CustomField {
            id: self.id,
            label: self.label.clone(),
            datatype: self.datatype.clone(),
        }
    }

    fn normalized(&self) -> bool {
        self.field().is_normalized()
    }
}

/// A group of books in the category browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Authors,
    Tags,
    Series,
    Publishers,
    Languages,
    Formats,
    Ratings,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Self::Authors,
        Self::Series,
        Self::Tags,
        Self::Publishers,
        Self::Languages,
        Self::Formats,
        Self::Ratings,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "author" | "authors" => Some(Self::Authors),
            "tag" | "tags" => Some(Self::Tags),
            "series" => Some(Self::Series),
            "publisher" | "publishers" => Some(Self::Publishers),
            "language" | "languages" => Some(Self::Languages),
            "format" | "formats" => Some(Self::Formats),
            "rating" | "ratings" => Some(Self::Ratings),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Authors => "authors",
            Self::Tags => "tags",
            Self::Series => "series",
            Self::Publishers => "publishers",
            Self::Languages => "languages",
            Self::Formats => "formats",
            Self::Ratings => "rating",
        }
    }

    /// `(item table, name column, link table, link column)`; formats have no
    /// item table.
    fn tables(self) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
        match self {
            Self::Authors => Some(("authors", "name", "books_authors_link", "author")),
            Self::Tags => Some(("tags", "name", "books_tags_link", "tag")),
            Self::Series => Some(("series", "name", "books_series_link", "series")),
            Self::Publishers => Some(("publishers", "name", "books_publishers_link", "publisher")),
            Self::Languages => Some((
                "languages",
                "lang_code",
                "books_languages_link",
                "lang_code",
            )),
            Self::Ratings => Some(("ratings", "rating", "books_ratings_link", "rating")),
            Self::Formats => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryItem {
    pub name: String,
    pub books: usize,
}

impl Library {
    // -----------------------------------------------------------------------
    // Preferences
    // -----------------------------------------------------------------------

    fn text_map_pref(&self, key: &str) -> Result<BTreeMap<String, String>> {
        let stored: Option<String> = self
            .conn
            .query_row("SELECT val FROM preferences WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        let parsed: serde_json::Value = stored
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let entries = parsed.as_object().into_iter().flatten();
        Ok(entries
            .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
            .collect())
    }

    fn set_text_map_entry(&mut self, key: &str, name: &str, value: Option<&str>) -> Result<()> {
        self.writable()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::InvalidMetadata("the name cannot be empty".into()));
        }
        let mut entries = self.text_map_pref(key)?;
        match value {
            Some(value) => {
                entries.insert(name.to_string(), value.trim().to_string());
            }
            None => {
                if entries.remove(name).is_none() {
                    return Err(Error::Other(format!("nothing is saved as `{name}`")));
                }
            }
        }
        let text = serde_json::to_string_pretty(&entries)
            .map_err(|error| Error::Other(error.to_string()))?;
        self.conn.execute(
            "INSERT INTO preferences (key, val) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET val=excluded.val",
            params![key, text],
        )?;
        Ok(())
    }

    /// Named queries, usable in a search as `search:name`.
    pub fn saved_searches(&self) -> Result<BTreeMap<String, String>> {
        self.text_map_pref(SAVED_SEARCHES)
    }

    /// Save a query under a name, or with `None` forget it.
    pub fn set_saved_search(&mut self, name: &str, query: Option<&str>) -> Result<()> {
        self.set_text_map_entry(SAVED_SEARCHES, name, query)
    }

    /// Named subsets of the library, usable in a search as `vl:name`.
    pub fn virtual_libraries(&self) -> Result<BTreeMap<String, String>> {
        self.text_map_pref(VIRTUAL_LIBRARIES)
    }

    pub fn set_virtual_library(&mut self, name: &str, query: Option<&str>) -> Result<()> {
        self.set_text_map_entry(VIRTUAL_LIBRARIES, name, query)
    }

    pub(crate) fn search_context(&self) -> Result<SearchContext> {
        let lowered = |entries: BTreeMap<String, String>| {
            entries
                .into_iter()
                .map(|(name, query)| (name.to_lowercase(), query))
                .collect()
        };
        Ok(SearchContext {
            custom: self
                .custom_columns()?
                .iter()
                .map(CustomColumn::field)
                .collect(),
            saved: lowered(self.saved_searches()?),
            virtual_libraries: lowered(self.virtual_libraries()?),
        })
    }

    // -----------------------------------------------------------------------
    // Category browser
    // -----------------------------------------------------------------------

    /// The values of one category with how many books carry each.
    pub fn category_items(&self, category: Category) -> Result<Vec<CategoryItem>> {
        let sql = match category.tables() {
            Some((table, column, link, link_column)) => format!(
                "SELECT CAST(t.{column} AS TEXT), COUNT(l.book) FROM {table} t
                 JOIN {link} l ON l.{link_column}=t.id
                 GROUP BY t.id ORDER BY t.{column} COLLATE NOCASE"
            ),
            None => "SELECT format, COUNT(DISTINCT book) FROM data
                     GROUP BY format ORDER BY format"
                .to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(CategoryItem {
                name: row.get::<_, String>(0)?,
                books: row.get::<_, i64>(1)? as usize,
            })
        })?;
        let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if category == Category::Authors {
            for item in &mut items {
                item.name = item.name.replace('|', ",");
            }
        }
        Ok(items)
    }

    /// Rename a tag, series or publisher everywhere. When the new name is
    /// already in use the two are merged. Returns the books affected.
    pub fn rename_category_item(
        &mut self,
        category: Category,
        from: &str,
        to: &str,
    ) -> Result<Vec<i64>> {
        self.writable()?;
        let renameable = matches!(
            category,
            Category::Tags | Category::Series | Category::Publishers
        );
        let Some((table, column, link, link_column)) = category.tables().filter(|_| renameable)
        else {
            return Err(Error::Other(format!(
                "{} cannot be renamed here; edit the books instead",
                category.name()
            )));
        };
        let to = to.trim();
        if to.is_empty() {
            return Err(Error::InvalidMetadata(
                "the new name cannot be empty".into(),
            ));
        }
        let find = |conn: &rusqlite::Connection, name: &str| -> Result<Option<i64>> {
            let sql = format!("SELECT id FROM {table} WHERE {column}=?1 COLLATE NOCASE");
            Ok(conn.query_row(&sql, [name], |row| row.get(0)).optional()?)
        };
        let tx = self.conn.transaction()?;
        let old = find(&tx, from)?
            .ok_or_else(|| Error::Other(format!("no {} named `{from}`", category.name())))?;
        let books: Vec<i64> = {
            let sql = format!("SELECT book FROM {link} WHERE {link_column}=?1 ORDER BY book");
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map([old], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        match find(&tx, to)?.filter(|existing| *existing != old) {
            Some(existing) => {
                // Books carrying both keep one link.
                tx.execute(
                    &format!(
                        "DELETE FROM {link} WHERE {link_column}=?1 AND book IN
                         (SELECT book FROM {link} WHERE {link_column}=?2)"
                    ),
                    params![old, existing],
                )?;
                tx.execute(
                    &format!("UPDATE {link} SET {link_column}=?2 WHERE {link_column}=?1"),
                    params![old, existing],
                )?;
                tx.execute(&format!("DELETE FROM {table} WHERE id=?1"), [old])?;
            }
            None => {
                tx.execute(
                    &format!("UPDATE {table} SET {column}=?2 WHERE id=?1"),
                    params![old, to],
                )?;
            }
        }
        tx.commit()?;
        for &book in &books {
            self.sync_book(book)?;
        }
        Ok(books)
    }

    // -----------------------------------------------------------------------
    // Custom columns
    // -----------------------------------------------------------------------

    pub fn custom_columns(&self) -> Result<Vec<CustomColumn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, name, datatype, is_multiple, display FROM custom_columns
             WHERE mark_for_delete=0 ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let display: String = row.get(5)?;
            let display: serde_json::Value = serde_json::from_str(&display).unwrap_or_default();
            let choices = display["enum_values"].as_array().into_iter().flatten();
            Ok(CustomColumn {
                id: row.get(0)?,
                label: row.get(1)?,
                name: row.get(2)?,
                datatype: row.get(3)?,
                is_multiple: row.get(4)?,
                choices: choices
                    .filter_map(|choice| choice.as_str().map(str::to_string))
                    .collect(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn custom_column(&self, label: &str) -> Result<CustomColumn> {
        let label = label.trim_start_matches('#');
        self.custom_columns()?
            .into_iter()
            .find(|column| column.label.eq_ignore_ascii_case(label))
            .ok_or_else(|| Error::Other(format!("no custom column `#{label}`")))
    }

    /// Add a custom column. `label` is the lookup name (letters, digits and
    /// underscores); only text columns can hold several values.
    pub fn create_custom_column(
        &mut self,
        label: &str,
        name: &str,
        datatype: &str,
        is_multiple: bool,
        choices: &[String],
    ) -> Result<CustomColumn> {
        self.writable()?;
        let label = label.trim_start_matches('#').to_ascii_lowercase();
        let valid = label.starts_with(|c: char| c.is_ascii_lowercase())
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(Error::InvalidMetadata(
                "a column's lookup name starts with a letter and uses only \
                 lower-case letters, digits and underscores"
                    .into(),
            ));
        }
        if !CUSTOM_DATATYPES.contains(&datatype) {
            return Err(Error::InvalidMetadata(format!(
                "unknown column type `{datatype}` (use one of: {})",
                CUSTOM_DATATYPES.join(", ")
            )));
        }
        if is_multiple && datatype != "text" {
            return Err(Error::InvalidMetadata(
                "only text columns can hold several values".into(),
            ));
        }
        if datatype == "enumeration" && choices.is_empty() {
            return Err(Error::InvalidMetadata(
                "an enumeration column needs its allowed values".into(),
            ));
        }
        if self.custom_column(&label).is_ok() {
            return Err(Error::Other(format!("column `#{label}` already exists")));
        }
        let normalized = matches!(datatype, "text" | "series" | "enumeration" | "rating");
        let display = match datatype {
            "enumeration" => serde_json::json!({"enum_values": choices}),
            _ => serde_json::json!({}),
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO custom_columns (label, name, datatype, is_multiple, editable, display, normalized)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
            params![label, name.trim(), datatype, is_multiple, display.to_string(), normalized],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute_batch(&custom_column_ddl(id, datatype, normalized))?;
        tx.commit()?;
        self.custom_column(&label)
    }

    /// Delete a custom column and every value in it.
    pub fn delete_custom_column(&mut self, label: &str) -> Result<()> {
        self.writable()?;
        let column = self.custom_column(label)?;
        let id = column.id;
        let tx = self.conn.transaction()?;
        tx.execute_batch(&format!(
            "DROP VIEW IF EXISTS tag_browser_custom_column_{id};
             DROP TABLE IF EXISTS books_custom_column_{id}_link;
             DROP TABLE IF EXISTS custom_column_{id};"
        ))?;
        tx.execute("DELETE FROM custom_columns WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// A book's custom values by column label. Multi-valued columns list
    /// every value; a series carries its index as `Name [2]`; ratings are in
    /// stars.
    pub fn custom_values(&self, book: i64) -> Result<BTreeMap<String, Vec<String>>> {
        let mut values = BTreeMap::new();
        for column in self.custom_columns()? {
            let id = column.id;
            let sql = if column.normalized() {
                let extra = if column.datatype == "series" {
                    "l.extra"
                } else {
                    "NULL"
                };
                format!(
                    "SELECT CAST(x.value AS TEXT), {extra} FROM books_custom_column_{id}_link l
                     JOIN custom_column_{id} x ON x.id=l.value WHERE l.book=?1 ORDER BY l.id"
                )
            } else {
                format!("SELECT CAST(value AS TEXT), NULL FROM custom_column_{id} WHERE book=?1")
            };
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([book], |row| {
                let value: String = row.get(0)?;
                Ok(match row.get::<_, Option<f64>>(1)? {
                    Some(index) => format!("{value} [{index}]"),
                    None => value,
                })
            })?;
            let mut found: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
            if column.datatype == "rating" {
                // Stored doubled; shown in stars.
                for value in &mut found {
                    let stars = value.parse::<f64>().unwrap_or_default() / 2.0;
                    *value = stars.to_string();
                }
            }
            if !found.is_empty() {
                values.insert(column.label, found);
            }
        }
        Ok(values)
    }

    /// Give a book the values another copy of it had (see
    /// [`Library::custom_values`]). Columns this library lacks, or that no
    /// longer accept a value, are left out.
    pub(crate) fn apply_custom_values(
        &mut self,
        book: i64,
        values: &BTreeMap<String, Vec<String>>,
    ) -> Result<()> {
        for (label, values) in values {
            if self.custom_column(label).is_err() {
                continue;
            }
            match self.set_custom_value(book, label, &values.join(",")) {
                Ok(()) | Err(Error::InvalidMetadata(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Set a book's value in a custom column; an empty value clears it.
    /// Multi-valued columns take a comma-separated list.
    pub fn set_custom_value(&mut self, book: i64, label: &str, value: &str) -> Result<()> {
        self.writable()?;
        self.ensure_exists(book)?;
        let column = self.custom_column(label)?;
        let id = column.id;
        let value = value.trim();
        let invalid = |what: &str| Error::InvalidMetadata(format!("`{value}` is not {what}"));
        let tx = self.conn.transaction()?;
        if !column.normalized() {
            tx.execute(
                &format!("DELETE FROM custom_column_{id} WHERE book=?1"),
                [book],
            )?;
            if !value.is_empty() {
                let stored: rusqlite::types::Value = match column.datatype.as_str() {
                    "int" => value
                        .parse::<i64>()
                        .map_err(|_| invalid("a whole number"))?
                        .into(),
                    "float" => value
                        .parse::<f64>()
                        .map_err(|_| invalid("a number"))?
                        .into(),
                    "bool" => match value.to_ascii_lowercase().as_str() {
                        "true" | "yes" | "1" => 1i64.into(),
                        "false" | "no" | "0" => 0i64.into(),
                        _ => return Err(invalid("yes or no")),
                    },
                    "datetime" => {
                        let date = crate::metadata::parse_user_date(value)
                            .ok_or_else(|| invalid("a date"))?;
                        super::format_datetime(&date).into()
                    }
                    _ => value.to_string().into(),
                };
                tx.execute(
                    &format!("INSERT INTO custom_column_{id} (book, value) VALUES (?1, ?2)"),
                    params![book, stored],
                )?;
            }
        } else {
            tx.execute(
                &format!("DELETE FROM books_custom_column_{id}_link WHERE book=?1"),
                [book],
            )?;
            let items: Vec<&str> = if column.is_multiple {
                value.split(',').map(str::trim).collect()
            } else {
                vec![value]
            };
            for item in items.into_iter().filter(|item| !item.is_empty()) {
                let (item, index) = match column.datatype.as_str() {
                    "series" => split_series_index(item),
                    _ => (item, None),
                };
                let stored: rusqlite::types::Value = match column.datatype.as_str() {
                    "rating" => {
                        let stars: f64 = item.parse().map_err(|_| invalid("a rating"))?;
                        if !(0.0..=5.0).contains(&stars) {
                            return Err(invalid("a rating from 0 to 5"));
                        }
                        ((stars * 2.0).round() as i64).into()
                    }
                    "enumeration" => {
                        let choice = column.choices.iter().find(|c| c.eq_ignore_ascii_case(item));
                        choice
                            .ok_or_else(|| invalid("one of the allowed values"))?
                            .clone()
                            .into()
                    }
                    _ => item.to_string().into(),
                };
                tx.execute(
                    &format!("INSERT OR IGNORE INTO custom_column_{id} (value) VALUES (?1)"),
                    [&stored],
                )?;
                let value_id: i64 = tx.query_row(
                    &format!("SELECT id FROM custom_column_{id} WHERE value=?1"),
                    [&stored],
                    |row| row.get(0),
                )?;
                if column.datatype == "series" {
                    tx.execute(
                        &format!(
                            "INSERT OR IGNORE INTO books_custom_column_{id}_link
                             (book, value, extra) VALUES (?1, ?2, ?3)"
                        ),
                        params![book, value_id, index.unwrap_or(1.0)],
                    )?;
                } else {
                    tx.execute(
                        &format!(
                            "INSERT OR IGNORE INTO books_custom_column_{id}_link (book, value)
                             VALUES (?1, ?2)"
                        ),
                        params![book, value_id],
                    )?;
                }
            }
            // Values no book uses any more.
            tx.execute(
                &format!(
                    "DELETE FROM custom_column_{id} WHERE id NOT IN
                     (SELECT value FROM books_custom_column_{id}_link)"
                ),
                [],
            )?;
        }
        // calibre rewrites the book's OPF, custom values included, from this.
        tx.execute(
            "INSERT OR IGNORE INTO metadata_dirtied (book) VALUES (?1)",
            [book],
        )?;
        tx.execute(
            "UPDATE books SET last_modified=?1 WHERE id=?2",
            params![super::format_datetime(&super::now()), book],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// Split `Name [2]` into the name and index.
fn split_series_index(value: &str) -> (&str, Option<f64>) {
    let parsed = value.strip_suffix(']').and_then(|rest| {
        let (name, index) = rest.rsplit_once('[')?;
        Some((name.trim(), index.trim().parse::<f64>().ok()?))
    });
    match parsed {
        Some((name, index)) if !name.is_empty() => (name, Some(index)),
        _ => (value, None),
    }
}

/// The tables calibre creates for a custom column.
fn custom_column_ddl(id: i64, datatype: &str, normalized: bool) -> String {
    let table = format!("custom_column_{id}");
    let link = format!("books_custom_column_{id}_link");
    let book_check = "SELECT CASE WHEN (SELECT id from books WHERE id=NEW.book) IS NULL
                      THEN RAISE(ABORT, 'Foreign key violation: book not in books') END;";
    if !normalized {
        let kind = match datatype {
            "int" => "INT",
            "float" => "REAL",
            "bool" => "BOOL",
            "datetime" => "timestamp",
            _ => "TEXT",
        };
        return format!(
            "CREATE TABLE {table}(
                 id    INTEGER PRIMARY KEY AUTOINCREMENT,
                 book  INTEGER,
                 value {kind} NOT NULL,
                 UNIQUE(book));
             CREATE INDEX {table}_idx ON {table} (book);
             CREATE TRIGGER fkc_insert_{table} BEFORE INSERT ON {table}
                 BEGIN {book_check} END;
             CREATE TRIGGER fkc_update_{table} BEFORE UPDATE OF book ON {table}
                 BEGIN {book_check} END;"
        );
    }
    let kind = if datatype == "rating" {
        "INT NOT NULL"
    } else {
        "TEXT NOT NULL COLLATE NOCASE"
    };
    let extra = if datatype == "series" {
        "extra REAL,"
    } else {
        ""
    };
    format!(
        "CREATE TABLE {table}(
             id    INTEGER PRIMARY KEY AUTOINCREMENT,
             value {kind},
             link TEXT NOT NULL DEFAULT \"\",
             UNIQUE(value));
         CREATE INDEX {table}_idx ON {table} (value COLLATE NOCASE);
         CREATE TABLE {link}(
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             book INTEGER NOT NULL,
             value INTEGER NOT NULL,
             {extra}
             UNIQUE(book, value));
         CREATE INDEX {link}_aidx ON {link} (value);
         CREATE INDEX {link}_bidx ON {link} (book);
         CREATE TRIGGER fkc_update_{link}_a BEFORE UPDATE OF book ON {link}
             BEGIN {book_check} END;
         CREATE TRIGGER fkc_insert_{link} BEFORE INSERT ON {link}
             BEGIN
                 SELECT CASE
                     WHEN (SELECT id from books WHERE id=NEW.book) IS NULL
                     THEN RAISE(ABORT, 'Foreign key violation: book not in books')
                     WHEN (SELECT id from {table} WHERE id=NEW.value) IS NULL
                     THEN RAISE(ABORT, 'Foreign key violation: value not in {table}')
                 END;
             END;
         CREATE TRIGGER fkc_delete_{link} AFTER DELETE ON {table}
             BEGIN DELETE FROM {link} WHERE value=OLD.id; END;
         CREATE VIEW tag_browser_{table} AS SELECT
             id,
             value,
             (SELECT COUNT(id) FROM {link} WHERE value={table}.id) count,
             (SELECT AVG(r.rating) FROM {link}, books_ratings_link as bl, ratings as r
              WHERE {link}.value={table}.id and bl.book={link}.book and
                    r.id = bl.rating and r.rating <> 0) avg_rating,
             value AS sort
         FROM {table};"
    )
}

/// The file format of a catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFormat {
    Csv,
    Json,
}

impl CatalogFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

const CATALOG_FIELDS: [&str; 13] = [
    "id",
    "title",
    "authors",
    "series",
    "series_index",
    "tags",
    "publisher",
    "rating",
    "languages",
    "identifiers",
    "pubdate",
    "timestamp",
    "formats",
];

impl Library {
    /// A catalog of the books matching `query` (all books when empty), one
    /// row per book with every custom column as `#label`.
    pub fn catalog(&self, query: &str, format: CatalogFormat) -> Result<String> {
        let options = super::ListOptions {
            query: Some(query.to_string()),
            ..Default::default()
        };
        let columns = self.custom_columns()?;
        let mut rows = Vec::new();
        for book in self.list_books(&options)? {
            let meta = self.get_metadata(book.id)?;
            let date = |value: Option<chrono::DateTime<chrono::Utc>>| {
                value.map(|date| date.format("%Y-%m-%d").to_string())
            };
            let identifiers: Vec<String> = meta
                .identifiers
                .iter()
                .map(|(kind, value)| format!("{kind}:{value}"))
                .collect();
            let mut row = serde_json::json!({
                "id": book.id,
                "title": meta.title,
                "authors": meta.authors,
                "series": meta.series,
                "series_index": meta.series.as_ref().map(|_| meta.series_index),
                "tags": meta.tags,
                "publisher": meta.publisher,
                "rating": meta.rating.map(|rating| f64::from(rating) / 2.0),
                "languages": meta.languages,
                "identifiers": identifiers,
                "pubdate": date(meta.pubdate),
                "timestamp": date(meta.timestamp),
                "formats": self.formats(book.id)?,
            });
            let mut custom = self.custom_values(book.id)?;
            for column in &columns {
                let values = custom.remove(&column.label).unwrap_or_default();
                row[format!("#{}", column.label)] = if column.is_multiple {
                    serde_json::json!(values)
                } else {
                    serde_json::json!(values.first())
                };
            }
            rows.push(row);
        }
        if format == CatalogFormat::Json {
            return serde_json::to_string_pretty(&rows)
                .map_err(|error| Error::Other(error.to_string()));
        }
        let mut header: Vec<String> = CATALOG_FIELDS.iter().map(|f| f.to_string()).collect();
        header.extend(columns.iter().map(|column| format!("#{}", column.label)));
        let mut out = header.join(",");
        out.push_str("\r\n");
        for row in &rows {
            let cells: Vec<String> = header
                .iter()
                .map(|field| {
                    csv_cell(&match &row[field.as_str()] {
                        serde_json::Value::Null => String::new(),
                        serde_json::Value::String(text) => text.clone(),
                        serde_json::Value::Array(items) => items
                            .iter()
                            .map(|item| item.as_str().unwrap_or_default())
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    })
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push_str("\r\n");
        }
        Ok(out)
    }
}

fn csv_cell(value: &str) -> String {
    // A leading formula character would be run by spreadsheet programs.
    let risky = value.starts_with(['=', '+', '-', '@']) && value.parse::<f64>().is_err();
    let value = if risky {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}
