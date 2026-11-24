//! Search public book catalogs, review candidate values and build selective updates.

use std::fmt;
use std::io::Cursor;

use serde_json::{json, Value};

use crate::metadata::parse_user_date;
use crate::network;
use crate::{BookUpdate, Error, Library, Metadata, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    OpenLibrary,
    GoogleBooks,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OpenLibrary => "Open Library",
            Self::GoogleBooks => "Google Books",
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub title: String,
    pub author: String,
    pub isbn: String,
}

impl Query {
    pub fn from_book(metadata: &Metadata) -> Self {
        Self {
            title: metadata.title.clone(),
            author: metadata
                .authors
                .iter()
                .find(|author| !author.eq_ignore_ascii_case("Unknown"))
                .cloned()
                .unwrap_or_default(),
            isbn: metadata
                .identifiers
                .get("isbn")
                .cloned()
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Authors,
    Publisher,
    Date,
    Description,
    Tags,
    Isbn,
    Cover,
}

impl Field {
    pub const ALL: [Self; 8] = [
        Self::Title,
        Self::Authors,
        Self::Publisher,
        Self::Date,
        Self::Description,
        Self::Tags,
        Self::Isbn,
        Self::Cover,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Authors => "Authors",
            Self::Publisher => "Publisher",
            Self::Date => "Published",
            Self::Description => "Description",
            Self::Tags => "Tags",
            Self::Isbn => "ISBN",
            Self::Cover => "Cover",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source: Source,
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub publisher: Option<String>,
    pub published: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub isbn: Option<String>,
    cover: Option<CoverId>,
    edition_links: Option<EditionLinks>,
}

#[derive(Debug, Clone)]
struct EditionLinks {
    authors: Option<Vec<EditionAuthor>>,
    work: Option<String>,
}

#[derive(Debug, Clone)]
enum EditionAuthor {
    Name(String),
    Id(String),
}

#[derive(Debug, Clone)]
enum CoverId {
    OpenLibrary(u64),
    GoogleBooks(String),
}

impl Candidate {
    pub fn has_cover(&self) -> bool {
        self.cover.is_some()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source.to_string(),
            "id": self.id,
            "title": self.title,
            "authors": self.authors,
            "publisher": self.publisher,
            "published": self.published,
            "description": self.description,
            "tags": self.tags,
            "isbn": self.isbn,
            "has_cover": self.has_cover()
        })
    }

    pub fn update(
        &self,
        current: &Metadata,
        fields: &[Field],
        cover: Option<Vec<u8>>,
    ) -> BookUpdate {
        let has = |field| fields.contains(&field);
        let pubdate = self.published.as_deref().and_then(parse_online_date);
        BookUpdate {
            title: has(Field::Title).then(|| self.title.clone()),
            authors: (has(Field::Authors) && !self.authors.is_empty())
                .then(|| self.authors.clone()),
            publisher: self
                .publisher
                .clone()
                .filter(|_| has(Field::Publisher))
                .map(Some),
            pubdate: pubdate.filter(|_| has(Field::Date)).map(Some),
            comments: self
                .description
                .as_deref()
                .filter(|_| has(Field::Description))
                .map(|value| Some(network::plain_comments(value))),
            tags: (has(Field::Tags) && !self.tags.is_empty()).then(|| self.tags.clone()),
            identifiers: self.isbn.as_ref().filter(|_| has(Field::Isbn)).map(|isbn| {
                let mut ids = current.identifiers.clone();
                ids.insert("isbn".into(), isbn.clone());
                ids
            }),
            cover: cover.filter(|_| has(Field::Cover)).map(Some),
            ..BookUpdate::default()
        }
    }

    pub fn update_for_book(
        &self,
        library: &Library,
        id: i64,
        fields: &[Field],
        cover: Option<Vec<u8>>,
    ) -> Result<BookUpdate> {
        Ok(self.update(&library.get_metadata(id)?, fields, cover))
    }
}

pub struct Catalog {
    agent: ureq::Agent,
    google_books_key: Option<String>,
    open_library: String,
    google_books: String,
    open_library_covers: String,
    google_books_covers: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            agent: network::agent(3),
            google_books_key: None,
            open_library: "https://openlibrary.org".into(),
            google_books: "https://www.googleapis.com".into(),
            open_library_covers: "https://covers.openlibrary.org".into(),
            google_books_covers: "https://books.google.com".into(),
        }
    }
}

impl Catalog {
    pub fn with_google_books_key(mut self, key: Option<&str>) -> Self {
        self.google_books_key = key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        self
    }

    pub fn search(&self, source: Source, query: &Query) -> Result<Vec<Candidate>> {
        let title = query.title.trim();
        let author = query.author.trim();
        let isbn = query.isbn.trim();
        if title.is_empty() && author.is_empty() && isbn.is_empty() {
            return Err(Error::Other("Enter a title, author, or ISBN".into()));
        }
        if source == Source::OpenLibrary && !isbn.is_empty() {
            return self.open_library_isbn(isbn);
        }
        let value: Value = match source {
            Source::OpenLibrary => {
                let mut request = self
                    .agent
                    .get(format!("{}/search.json", self.open_library))
                    .query("limit", "8")
                    .query(
                        "fields",
                        "key,title,author_name,publisher,first_publish_year,publish_date,subject,isbn,cover_i",
                    );
                if !title.is_empty() {
                    request = request.query("title", title);
                }
                if !author.is_empty() {
                    request = request.query("author", author);
                }
                network::json("Open Library", request.call())?
            }
            Source::GoogleBooks => {
                let query = if isbn.is_empty() {
                    [
                        (!title.is_empty()).then(|| format!("intitle:{title}")),
                        (!author.is_empty()).then(|| format!("inauthor:{author}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ")
                } else {
                    format!("isbn:{isbn}")
                };
                self.google_books_search(&query)?
            }
        };
        let (results, parse): (&Value, fn(&Value) -> Option<Candidate>) = match source {
            Source::OpenLibrary => (&value["docs"], open_library_candidate),
            Source::GoogleBooks if value["totalItems"].as_u64() == Some(0) => return Ok(Vec::new()),
            Source::GoogleBooks => (&value["items"], google_books_candidate),
        };
        let results = results
            .as_array()
            .ok_or_else(|| Error::Other(format!("{source}: missing search results in response")))?;
        Ok(results.iter().filter_map(parse).take(8).collect())
    }

    fn google_books_search(&self, query: &str) -> Result<Value> {
        let env_key = std::env::var("GOOGLE_BOOKS_API_KEY").ok();
        let key = self.google_books_key.as_deref().or_else(|| {
            env_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
        });
        let mut request = self
            .agent
            .get(format!("{}/books/v1/volumes", self.google_books))
            .query("maxResults", "8")
            .query("printType", "books")
            .query("q", query);
        if let Some(key) = key {
            request = request.query("key", key);
        }
        let response = request
            .config()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .call()
            .map_err(|error| network::response_error("Google Books", error))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return network::json("Google Books", Ok(response));
        }
        let quota = status == 429
            || (status == 403
                && network::json::<Value>("Google Books", Ok(response)).is_ok_and(|body| {
                    body["error"]["status"] == "RESOURCE_EXHAUSTED"
                        || body["error"]["errors"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|error| {
                                matches!(
                                    error["reason"].as_str(),
                                    Some(
                                        "quotaExceeded"
                                            | "dailyLimitExceeded"
                                            | "rateLimitExceeded"
                                            | "userRateLimitExceeded"
                                    )
                                )
                            })
                }));
        if quota {
            let advice = if key.is_some() {
                "check the key's Google Cloud quota or try again later"
            } else {
                "set GOOGLE_BOOKS_API_KEY, supply a key, or try again later"
            };
            return Err(Error::Other(format!(
                "Google Books: quota or rate limit exceeded (HTTP {status}); {advice}"
            )));
        }
        Err(network::response_error(
            "Google Books",
            ureq::Error::StatusCode(status),
        ))
    }

    fn open_library_isbn(&self, isbn: &str) -> Result<Vec<Candidate>> {
        let isbn = isbn
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        let valid = match isbn.len() {
            10 => {
                isbn.bytes().take(9).all(|c| c.is_ascii_digit())
                    && isbn
                        .bytes()
                        .last()
                        .is_some_and(|c| c.is_ascii_digit() || c == b'X')
            }
            13 => isbn.bytes().all(|c| c.is_ascii_digit()),
            _ => false,
        };
        if !valid {
            return Err(Error::Other(
                "Open Library: enter a 10- or 13-digit ISBN".into(),
            ));
        }
        let response = self
            .agent
            .get(format!("{}/isbn/{isbn}.json", self.open_library))
            .call();
        if matches!(response, Err(ureq::Error::StatusCode(404))) {
            return Ok(Vec::new());
        }
        let value: Value = network::json("Open Library", response)?;
        let edition = open_library_edition(&value, isbn)
            .ok_or_else(|| Error::Other("Open Library: missing edition in ISBN response".into()))?;
        Ok(vec![edition])
    }

    pub fn enrich(&self, candidate: &mut Candidate) -> Result<()> {
        if candidate.source != Source::OpenLibrary {
            return Ok(());
        }
        if open_library_id(&candidate.id, 'W') {
            if candidate.description.is_none() {
                candidate.description =
                    open_library_description(&self.open_library_record("works", &candidate.id)?);
            }
            return Ok(());
        }
        if !open_library_id(&candidate.id, 'M') {
            return Ok(());
        }
        if candidate.edition_links.is_none() {
            let edition = self.open_library_record("books", &candidate.id)?;
            candidate.description = candidate
                .description
                .take()
                .or_else(|| open_library_description(&edition));
            candidate.edition_links = Some(EditionLinks::from_record(&edition));
        }
        let Some(links) = &candidate.edition_links else {
            return Ok(());
        };
        let description = if candidate.description.is_none() {
            links
                .work
                .as_deref()
                .map(|work| {
                    self.open_library_record("works", work)
                        .map(|record| open_library_description(&record))
                })
                .transpose()
        } else {
            Ok(None)
        };
        if let Ok(Some(Some(description))) = &description {
            candidate.description = Some(description.clone());
        }
        if candidate.authors.is_empty() {
            if let Some(authors) = &links.authors {
                candidate.authors = authors
                    .iter()
                    .map(|author| match author {
                        EditionAuthor::Name(name) => Ok(name.clone()),
                        EditionAuthor::Id(id) => {
                            string(&self.open_library_record("authors", id)?["name"]).ok_or_else(
                                || Error::Other("Open Library: author record has no name".into()),
                            )
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
            }
        }
        description.map(|_| ())
    }

    fn open_library_record(&self, path: &str, id: &str) -> Result<Value> {
        network::json(
            "Open Library",
            self.agent
                .get(format!("{}/{path}/{id}.json", self.open_library))
                .call(),
        )
    }

    pub fn cover(&self, candidate: &Candidate, thumbnail: bool) -> Result<Option<Vec<u8>>> {
        let request = match &candidate.cover {
            None => return Ok(None),
            Some(CoverId::OpenLibrary(id)) => {
                let size = if thumbnail { "S" } else { "L" };
                self.agent
                    .get(format!("{}/b/id/{id}-{size}.jpg", self.open_library_covers))
                    .query("default", "false")
            }
            Some(CoverId::GoogleBooks(id)) => self
                .agent
                .get(format!("{}/books/content", self.google_books_covers))
                .query("id", id)
                .query("printsec", "frontcover")
                .query("img", "1")
                .query("zoom", if thumbnail { "5" } else { "1" }),
        };
        let response = request.call();
        if matches!(response, Err(ureq::Error::StatusCode(404))) {
            return Ok(None);
        }
        let bytes = network::read("Book cover", response, 8 * 1024 * 1024)?;
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| Error::Other("Book cover: invalid image".into()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(5000);
        limits.max_image_height = Some(5000);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|_| Error::Other("Book cover: couldn't decode image".into()))?;
        if image.width() < 2 || image.height() < 2 {
            return Ok(None);
        }
        let image = if thumbnail {
            image.thumbnail(100, 150)
        } else {
            image
        };
        let mut jpeg = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .map_err(|_| Error::Other("Book cover: couldn't encode image".into()))?;
        Ok(Some(jpeg))
    }
}

fn open_library_id(id: &str, kind: char) -> bool {
    id.strip_prefix("OL")
        .and_then(|id| id.strip_suffix(kind))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

fn open_library_link(value: &Value, prefix: &str, suffix: char) -> Option<String> {
    let id = value.as_str()?.strip_prefix(prefix)?;
    open_library_id(id, suffix).then(|| id.to_string())
}

fn open_library_description(value: &Value) -> Option<String> {
    let description = &value["description"];
    string(description).or_else(|| string(&description["value"]))
}

impl EditionLinks {
    fn from_record(value: &Value) -> Self {
        let authors = value["authors"]
            .as_array()
            .filter(|authors| authors.len() <= 20)
            .and_then(|authors| {
                authors
                    .iter()
                    .map(|author| {
                        string(&author["name"])
                            .map(EditionAuthor::Name)
                            .or_else(|| {
                                open_library_link(&author["key"], "/authors/", 'A')
                                    .or_else(|| {
                                        open_library_link(
                                            &author["author"]["key"],
                                            "/authors/",
                                            'A',
                                        )
                                    })
                                    .map(EditionAuthor::Id)
                            })
                    })
                    .collect()
            });
        let work = value["works"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|work| open_library_link(&work["key"], "/works/", 'W'));
        Self { authors, work }
    }
}

fn open_library_candidate(value: &Value) -> Option<Candidate> {
    let key = value["key"].as_str()?;
    let id = key
        .strip_prefix("/works/")
        .or_else(|| key.strip_prefix("/books/"))
        .unwrap_or(key);
    if !open_library_id(id, 'W') && !open_library_id(id, 'M') {
        return None;
    }
    Some(Candidate {
        source: Source::OpenLibrary,
        id: id.into(),
        title: string(&value["title"])?,
        authors: strings(&value["author_name"], 20),
        publisher: strings(&value["publisher"], 1).pop(),
        published: value["publish_date"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(string)
            .chain(string(&value["publish_date"]))
            .find(|date| parse_online_date(date).is_some())
            .or_else(|| {
                value["first_publish_year"]
                    .as_u64()
                    .map(|year| year.to_string())
            }),
        description: None,
        tags: strings(&value["subject"], 12),
        isbn: strings(&value["isbn"], 1).pop(),
        cover: value["cover_i"].as_u64().map(CoverId::OpenLibrary),
        edition_links: None,
    })
}

fn open_library_edition(value: &Value, isbn: String) -> Option<Candidate> {
    let id = value["key"].as_str()?.strip_prefix("/books/")?;
    if !open_library_id(id, 'M') {
        return None;
    }
    let links = EditionLinks::from_record(value);
    let authors = links
        .authors
        .as_deref()
        .and_then(|rows| {
            rows.iter()
                .map(|author| match author {
                    EditionAuthor::Name(name) => Some(name.clone()),
                    EditionAuthor::Id(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Candidate {
        source: Source::OpenLibrary,
        id: id.into(),
        title: string(&value["title"])?,
        authors,
        publisher: strings(&value["publishers"], 1).pop(),
        published: string(&value["publish_date"]),
        description: open_library_description(value),
        tags: strings(&value["subjects"], 12),
        isbn: Some(isbn),
        cover: value["covers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .find(|id| *id != 0)
            .map(CoverId::OpenLibrary),
        edition_links: Some(links),
    })
}

fn google_books_candidate(value: &Value) -> Option<Candidate> {
    let id = string(&value["id"])?;
    let info = &value["volumeInfo"];
    let identifiers = info["industryIdentifiers"].as_array();
    let isbn = ["ISBN_13", "ISBN_10"].into_iter().find_map(|kind| {
        identifiers?.iter().find_map(|identifier| {
            (identifier["type"].as_str() == Some(kind))
                .then(|| string(&identifier["identifier"]))
                .flatten()
        })
    });
    Some(Candidate {
        source: Source::GoogleBooks,
        title: string(&info["title"])?,
        authors: strings(&info["authors"], 20),
        publisher: string(&info["publisher"]),
        published: string(&info["publishedDate"]),
        description: string(&info["description"]),
        tags: strings(&info["categories"], 12),
        isbn,
        cover: info["imageLinks"]
            .is_object()
            .then(|| CoverId::GoogleBooks(id.clone())),
        id,
        edition_links: None,
    })
}

fn string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(8000).collect())
}

fn strings(value: &Value, max: usize) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(string)
        .take(max)
        .collect()
}

fn parse_online_date(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let standard = match value.len() {
        4 => parse_user_date(&format!("{value}-01-01")),
        7 => parse_user_date(&format!("{value}-01")),
        _ => parse_user_date(value),
    };
    standard.or_else(|| {
        ["%B %e, %Y", "%b %e, %Y", "%e %B %Y", "%e %b %Y"]
            .into_iter()
            .find_map(|pattern| chrono::NaiveDate::parse_from_str(value, pattern).ok())
            .or_else(|| {
                ["%e %B %Y", "%e %b %Y"].into_iter().find_map(|pattern| {
                    chrono::NaiveDate::parse_from_str(&format!("1 {value}"), pattern).ok()
                })
            })
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|date| date.and_utc())
    })
}

#[cfg(test)]
fn proposed_date(candidate: &Candidate) -> Option<String> {
    candidate
        .update(&Metadata::new("Book"), &[Field::Date], None)
        .pubdate
        .flatten()
        .map(|date| date.date_naive().to_string())
}

#[cfg(test)]
#[test]
fn open_library_month_names_and_year_fallback_produce_applicable_dates() {
    for (date, expected) in [
        ("October 1, 1988", "1988-10-01"),
        ("Oct 1988", "1988-10-01"),
        ("unknown", "1891-01-01"),
    ] {
        let candidate = open_library_candidate(&json!({
            "key": "/works/OL1W", "title":"Book", "publish_date":[date], "first_publish_year":1891
        }))
        .unwrap();
        assert_eq!(proposed_date(&candidate).as_deref(), Some(expected));
    }
    let edition = open_library_edition(
        &json!({
            "key": "/books/OL1M", "title":"Book", "publish_date":"1 October 1988"
        }),
        "9780140328721".into(),
    )
    .unwrap();
    assert_eq!(proposed_date(&edition).as_deref(), Some("1988-10-01"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::test::serve;

    #[test]
    fn applying_an_isbn_uses_identifiers_changed_since_preview() {
        let dir = tempfile::tempdir().unwrap();
        let mut library = Library::create(dir.path()).unwrap();
        let mut metadata = Metadata::new("Book");
        metadata.identifiers.insert("asin".into(), "old".into());
        let id = library.add_empty(metadata).unwrap().id;
        let candidate = open_library_candidate(&json!({
            "key":"/works/OL123W","title":"Book","isbn":["9780140328721"]
        }))
        .unwrap();
        let snapshot = library.get_metadata(id).unwrap();
        let mut latest = snapshot.identifiers;
        latest.insert("asin".into(), "new".into());
        latest.insert("doi".into(), "extra".into());
        library
            .update_book(
                id,
                &BookUpdate {
                    identifiers: Some(latest),
                    ..Default::default()
                },
            )
            .unwrap();
        let patch = candidate
            .update_for_book(&library, id, &[Field::Isbn], None)
            .unwrap();
        library.update_book(id, &patch).unwrap();
        let ids = library.get_metadata(id).unwrap().identifiers;
        assert_eq!(ids["asin"], "new");
        assert_eq!(ids["doi"], "extra");
        assert_eq!(ids["isbn"], "9780140328721");
    }

    #[test]
    fn open_library_search_and_preview_keep_edits_selective() {
        let (open_library, server) = serve(
            "200 OK",
            r#"{"docs":[{"key":"/works/OL123W","title":"A & B","author_name":["Author"],"first_publish_year":1999,"isbn":["1234"],"cover_i":42}]}"#,
            |headers, _| {
                let request = headers.lines().next().unwrap();
                assert!(request.starts_with("GET /search.json?"));
                assert!(request.contains("title=A%20%26%20B") || request.contains("title=A+%26+B"));
                assert!(request.contains("author=Author"));
            },
        );
        let mut catalog = Catalog {
            open_library,
            ..Catalog::default()
        };
        let mut results = catalog
            .search(
                Source::OpenLibrary,
                &Query {
                    title: "A & B".into(),
                    author: "Author".into(),
                    ..Query::default()
                },
            )
            .unwrap();
        server.join().unwrap();
        let candidate = &mut results[0];
        assert!(candidate.has_cover());

        let (base, server) = serve(
            "200 OK",
            r#"{"description":{"value":"Text <b>markup</b>"}}"#,
            |headers, _| assert!(headers.starts_with("GET /works/OL123W.json HTTP/1.1")),
        );
        catalog.open_library = base;
        catalog.enrich(candidate).unwrap();
        server.join().unwrap();
        let mut original = Metadata::new("Original title");
        original.identifiers.insert("asin".into(), "keep me".into());
        let patch = candidate.update(
            &original,
            &[Field::Isbn, Field::Description, Field::Date],
            None,
        );
        assert_eq!(patch.title, None);
        assert_eq!(patch.authors, None);
        let ids = patch.identifiers.unwrap();
        assert_eq!(ids["asin"], "keep me");
        assert_eq!(ids["isbn"], "1234");
        assert_eq!(
            patch.comments,
            Some(Some("Text &lt;b&gt;markup&lt;/b&gt;".into()))
        );
        assert_eq!(
            patch.pubdate.flatten().unwrap().date_naive().to_string(),
            "1999-01-01"
        );
    }

    #[test]
    fn open_library_search_keeps_edition_keys_and_enriches_them() {
        let (open_library, server) = serve(
            "200 OK",
            r#"{"docs":[{"key":"/books/OL12M","title":"First","publish_date":["2002"]},{"key":"OL34M","title":"Second"},{"key":"/books/../x","title":"Invalid"}]}"#,
            |headers, _| assert!(headers.starts_with("GET /search.json?")),
        );
        let mut catalog = Catalog {
            open_library,
            ..Default::default()
        };
        let mut candidates = catalog
            .search(
                Source::OpenLibrary,
                &Query {
                    title: "Book".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["OL12M", "OL34M"]
        );
        assert_eq!(candidates[0].published.as_deref(), Some("2002"));
        let (base, server) = serve(
            "200 OK",
            r#"{"description":"Edition description"}"#,
            |headers, _| assert!(headers.starts_with("GET /books/OL12M.json HTTP/1.1")),
        );
        catalog.open_library = base;
        catalog.enrich(&mut candidates[0]).unwrap();
        assert_eq!(
            candidates[0].description.as_deref(),
            Some("Edition description")
        );
        server.join().unwrap();
    }

    #[test]
    fn open_library_isbn_uses_edition_metadata_and_the_requested_isbn() {
        let (open_library, server) = serve(
            "200 OK",
            r#"{"key":"/books/OL12M","title":"Edition title","isbn_10":["0140328726"],"isbn_13":["9780140328721"],"publishers":["Edition publisher"],"publish_date":"1988-10-01","covers":[-1,42],"works":[{"key":"/works/OL321W"}]}"#,
            |headers, _| assert!(headers.starts_with("GET /isbn/9780140328721.json HTTP/1.1")),
        );
        let catalog = Catalog {
            open_library,
            ..Default::default()
        };
        let candidates = catalog
            .search(
                Source::OpenLibrary,
                &Query {
                    isbn: "978-0-14-032872-1".into(),
                    title: "Work title".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let [candidate] = candidates.as_slice() else {
            panic!("expected exactly one edition")
        };
        assert_eq!(candidate.id, "OL12M");
        assert_eq!(candidate.title, "Edition title");
        assert_eq!(candidate.isbn.as_deref(), Some("9780140328721"));
        assert_eq!(candidate.publisher.as_deref(), Some("Edition publisher"));
        assert_eq!(candidate.published.as_deref(), Some("1988-10-01"));
        assert!(matches!(candidate.cover, Some(CoverId::OpenLibrary(42))));
        server.join().unwrap();
    }

    #[test]
    fn isbn_preview_resolves_edition_author_and_work_links_without_refetching_the_edition() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let open_library = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (endpoint, body) in [
                (
                    "/isbn/9780140328721.json",
                    r#"{"key":"/books/OL12M","title":"Edition","authors":[{"key":"/authors/OL42A"},{"name":"Second Author"}],"works":[{"key":"/works/OL3W"}]}"#,
                ),
                (
                    "/works/OL3W.json",
                    r#"{"description":{"value":"Work description"}}"#,
                ),
                ("/authors/OL42A.json", r#"{"name":"Linked Author"}"#),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), format!("GET {endpoint} HTTP/1.1"));
                while !line.trim().is_empty() {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                }
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let catalog = Catalog {
            open_library,
            ..Default::default()
        };
        let mut candidates = catalog
            .search(
                Source::OpenLibrary,
                &Query {
                    isbn: "9780140328721".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        catalog.enrich(&mut candidates[0]).unwrap();
        assert_eq!(candidates[0].authors, ["Linked Author", "Second Author"]);
        assert_eq!(
            candidates[0].description.as_deref(),
            Some("Work description")
        );
        assert_eq!(candidates[0].title, "Edition");
        server.join().unwrap();
    }

    #[test]
    fn google_books_key_is_used_on_search_and_quota_errors_are_sanitized() {
        for (status, body) in [
            (
                "429 Too Many Requests",
                r#"{"error":{"message":"test-only-key"}}"#,
            ),
            (
                "403 Forbidden",
                r#"{"error":{"errors":[{"reason":"dailyLimitExceeded"}],"message":"test-only-key"}}"#,
            ),
        ] {
            let (google_books, server) = serve(status, body, |headers, _| {
                assert!(headers
                    .lines()
                    .next()
                    .unwrap()
                    .contains("key=test-only-key"));
            });
            let catalog = Catalog {
                google_books,
                ..Default::default()
            }
            .with_google_books_key(Some(" test-only-key "));
            let error = catalog
                .search(
                    Source::GoogleBooks,
                    &Query {
                        title: "Book".into(),
                        ..Default::default()
                    },
                )
                .unwrap_err()
                .to_string();
            assert!(error.contains("quota or rate limit exceeded"));
            assert!(error.contains(&status[..3]));
            assert!(!error.contains("test-only-key"));
            server.join().unwrap();
        }
    }

    #[test]
    fn google_isbn_search_and_cover_use_fixed_provider_endpoints() {
        let (google_books, server) = serve(
            "200 OK",
            r#"{"totalItems":1,"items":[{"id":"ABC","volumeInfo":{"title":"Book","industryIdentifiers":[{"type":"ISBN_10","identifier":"10"},{"type":"ISBN_13","identifier":"13"}],"imageLinks":{"thumbnail":"https://invalid.example/never-follow"}}}]}"#,
            |headers, _| {
                let request = headers.lines().next().unwrap();
                assert!(request.starts_with("GET /books/v1/volumes?"));
                assert!(request.contains("q=isbn%3A13"));
                assert!(!request.contains("intitle"));
            },
        );
        let mut catalog = Catalog {
            google_books,
            ..Catalog::default()
        };
        let results = catalog
            .search(
                Source::GoogleBooks,
                &Query {
                    isbn: "13".into(),
                    title: "Ignored".into(),
                    ..Query::default()
                },
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(results[0].isbn.as_deref(), Some("13"));

        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(200, 300)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let (base, server) = serve("200 OK", png.into_inner(), |headers, _| {
            let request = headers.lines().next().unwrap();
            assert!(request.starts_with("GET /books/content?"));
            assert!(request.contains("id=ABC"));
        });
        catalog.google_books_covers = base;
        let bytes = catalog.cover(&results[0], true).unwrap().unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (100, 150));
        server.join().unwrap();
    }
}
