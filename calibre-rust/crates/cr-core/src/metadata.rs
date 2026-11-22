//! The canonical metadata model shared by every format reader and the library.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde_json::{json, Value};

use crate::text;

/// Metadata for one ebook, modelled on calibre's `Metadata` object.
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub title: String,
    pub title_sort: Option<String>,
    pub authors: Vec<String>,
    pub author_sort: Option<String>,
    pub publisher: Option<String>,
    pub series: Option<String>,
    pub series_index: f64,
    pub tags: Vec<String>,
    /// calibre stores ratings on a 0-10 scale (2 points per star).
    pub rating: Option<u8>,
    /// HTML, exactly as calibre stores it.
    pub comments: Option<String>,
    pub identifiers: BTreeMap<String, String>,
    pub languages: Vec<String>,
    pub pubdate: Option<DateTime<Utc>>,
    pub timestamp: Option<DateTime<Utc>>,
    pub last_modified: Option<DateTime<Utc>>,
    pub uuid: Option<String>,
    pub cover: Option<Vec<u8>>,
    /// Upper-case format name of the source file, e.g. `EPUB`.
    pub format: Option<String>,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            title: "Unknown".to_string(),
            title_sort: None,
            authors: Vec::new(),
            author_sort: None,
            publisher: None,
            series: None,
            series_index: 1.0,
            tags: Vec::new(),
            rating: None,
            comments: None,
            identifiers: BTreeMap::new(),
            languages: Vec::new(),
            pubdate: None,
            timestamp: None,
            last_modified: None,
            uuid: None,
            cover: None,
            format: None,
        }
    }
}

impl Metadata {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Default::default()
        }
    }

    /// Fill in defaults calibre relies on and remove empties/duplicates.
    pub fn normalize(&mut self) {
        self.title = self.title.trim().to_string();
        if self.title.is_empty() {
            self.title = "Unknown".to_string();
        }
        self.authors = dedupe(self.authors.drain(..).map(|a| a.trim().to_string()));
        if self.authors.is_empty() {
            self.authors.push("Unknown".to_string());
        }
        self.tags = dedupe(self.tags.drain(..).map(|t| t.trim().to_string()));
        self.languages = dedupe(self.languages.drain(..).map(|l| l.trim().to_string()));
        self.author_sort = self
            .author_sort
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.publisher = clean_optional(self.publisher.take());
        self.series = clean_optional(self.series.take());
        self.comments = self
            .comments
            .take()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        if !self.series_index.is_finite() || self.series_index <= 0.0 {
            self.series_index = 1.0;
        }
        self.uuid = clean_optional(self.uuid.take());
        self.identifiers.retain(|k, v| {
            let k = k.trim();
            let v = v.trim();
            !k.is_empty() && !v.is_empty()
        });
        self.identifiers = self
            .identifiers
            .iter()
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        if self.rating == Some(0) {
            self.rating = None;
        }
        if let Some(rating) = self.rating {
            if rating > 10 {
                self.rating = Some(10);
            }
        }
    }

    /// The sort key calibre would store for the title.
    pub fn computed_title_sort(&self) -> String {
        self.title_sort
            .clone()
            .unwrap_or_else(|| text::title_sort(&self.title))
    }

    /// The sort key calibre would store on the book row.
    pub fn computed_author_sort(&self) -> String {
        self.author_sort
            .clone()
            .unwrap_or_else(|| text::book_author_sort(&self.authors))
    }

    pub fn first_author(&self) -> &str {
        self.authors
            .first()
            .map(String::as_str)
            .unwrap_or("Unknown")
    }

    /// Serialise for `--json` output and device catalogs.
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "title_sort": self.computed_title_sort(),
            "authors": self.authors,
            "author_sort": self.computed_author_sort(),
            "publisher": self.publisher,
            "series": self.series,
            "series_index": self.series_index,
            "tags": self.tags,
            "rating": self.rating,
            "comments": self.comments,
            "identifiers": self.identifiers,
            "languages": self.languages,
            "pubdate": self.pubdate.as_ref().map(format_datetime),
            "timestamp": self.timestamp.as_ref().map(format_datetime),
            "last_modified": self.last_modified.as_ref().map(format_datetime),
            "uuid": self.uuid,
            "format": self.format,
            "has_cover": self.cover.is_some(),
        })
    }
}

/// Lightweight book row used by list/search results.
#[derive(Debug, Clone, PartialEq)]
pub struct BookSummary {
    pub id: i64,
    pub title: String,
    pub authors: Vec<String>,
    pub author_sort: String,
    pub publisher: Option<String>,
    pub series: Option<String>,
    pub series_index: f64,
    pub tags: Vec<String>,
    pub rating: Option<u8>,
    pub formats: Vec<String>,
    pub has_cover: bool,
    pub timestamp: Option<DateTime<Utc>>,
    pub pubdate: Option<DateTime<Utc>>,
    pub path: String,
}

impl BookSummary {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "authors": self.authors,
            "author_sort": self.author_sort,
            "publisher": self.publisher,
            "series": self.series,
            "series_index": self.series_index,
            "tags": self.tags,
            "rating": self.rating,
            "formats": self.formats,
            "has_cover": self.has_cover,
            "timestamp": self.timestamp.as_ref().map(format_datetime),
            "pubdate": self.pubdate.as_ref().map(format_datetime),
            "path": self.path,
        })
    }
}

/// Result of importing a file: extracted metadata plus the source format.
#[derive(Debug, Clone)]
pub struct ImportedBook {
    pub metadata: Metadata,
    pub format: String,
    /// Original file path supplied by the caller.
    pub source: PathBuf,
}

/// Parse calibre-style timestamps.
pub fn parse_datetime(input: &str) -> Option<DateTime<Utc>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let patterns = [
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for pattern in &patterns {
        if let Ok(dt) = DateTime::parse_from_str(input, pattern) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(Utc.from_utc_datetime(&dt));
    }
    if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map(|dt| Utc.from_utc_datetime(&dt));
    }
    None
}

/// Format a timestamp the way calibre writes it to SQLite.
pub fn format_datetime(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S+00:00").to_string()
}

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Parse loose user-provided dates (also accepts full timestamps).
pub fn parse_user_date(input: &str) -> Option<DateTime<Utc>> {
    parse_datetime(input)
}

/// Convert plain text comments into the simple HTML calibre stores.
pub fn htmlize_comments(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if looks_like_html(trimmed) {
        return trimmed.to_string();
    }
    trimmed
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let escaped = html_escape::encode_text(p.trim());
            let with_breaks = escaped.replace('\n', "<br>");
            format!("<p>{with_breaks}</p>")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when the timestamp is one calibre considers "unset".
pub fn is_unset_date(dt: &DateTime<Utc>) -> bool {
    dt.timestamp() <= 0
}

fn looks_like_html(input: &str) -> bool {
    let mut search_from = 0usize;
    while let Some(offset) = input[search_from..].find('<') {
        let index = search_from + offset;
        let next = input[index + 1..].chars().next();
        if matches!(next, Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!')
            && input[index..].contains('>')
        {
            return true;
        }
        search_from = index + 1;
        if search_from >= input.len() {
            break;
        }
    }
    false
}

fn dedupe(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        if value.is_empty() {
            continue;
        }
        if !out
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            out.push(value);
        }
    }
    out
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_metadata() {
        let mut m = Metadata::new("  ");
        m.authors = vec!["A".into(), "a".into(), " ".into()];
        m.tags = vec!["SF".into(), "sf".into(), "Fantasy".into()];
        m.rating = Some(0);
        m.series_index = -1.0;
        m.normalize();
        assert_eq!(m.title, "Unknown");
        assert_eq!(m.authors, vec!["A"]);
        assert_eq!(m.tags, vec!["SF", "Fantasy"]);
        assert_eq!(m.rating, None);
        assert_eq!(m.series_index, 1.0);
    }

    #[test]
    fn computes_sorts() {
        let mut m = Metadata::new("The Hobbit");
        m.authors = vec!["J. R. R. Tolkien".into()];
        assert_eq!(m.computed_title_sort(), "Hobbit, The");
        assert_eq!(m.computed_author_sort(), "Tolkien, J. R. R.");
    }

    #[test]
    fn parses_and_formats_dates() {
        let dt = parse_datetime("2024-03-05 10:20:30+00:00").unwrap();
        assert_eq!(format_datetime(&dt), "2024-03-05 10:20:30+00:00");
        assert!(parse_user_date("2024-03-05").is_some());
        assert!(parse_datetime("nonsense").is_none());
    }

    #[test]
    fn htmlizes_comments() {
        assert_eq!(htmlize_comments("A < B"), "<p>A &lt; B</p>");
        assert_eq!(htmlize_comments("one\ntwo"), "<p>one<br>two</p>");
        assert_eq!(htmlize_comments("<p>already</p>"), "<p>already</p>");
    }
}
