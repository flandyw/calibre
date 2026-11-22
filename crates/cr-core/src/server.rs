//! A read-only content server: browse, search, download and read a library
//! from a browser, and an OPDS catalog for reading apps.
//!
//! It speaks plain HTTP/1.1 over `std::net` and only answers `GET` and
//! `HEAD`. Nothing in the library is written. For access beyond the local
//! machine set a user and password and put it behind a TLS reverse proxy:
//! Basic authentication alone does not protect the password in transit.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};

use crate::error::{Error, Result};
use crate::formats::document::Document;
use crate::formats::html::strip_tags;
use crate::library::{BookSort, Library, ListOptions};
use crate::metadata::BookSummary;

const PAGE_SIZE: usize = 50;
const MAX_CONNECTIONS: usize = 32;
const MAX_HEAD_BYTES: u64 = 16 * 1024;
/// The whole request head must arrive within this time.
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
const SECURITY: &str = "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Address and port, e.g. `127.0.0.1:8080`. Port 0 picks a free port.
    pub bind: String,
    /// Require this user and password (HTTP Basic) for every request.
    pub credentials: Option<(String, String)>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".to_string(),
            credentials: None,
        }
    }
}

/// What a request is answered with.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

#[derive(Debug)]
pub enum Body {
    Bytes(Vec<u8>),
    /// Streamed from disk.
    File(PathBuf),
}

impl Response {
    fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: content_type.to_string(),
            headers: Vec::new(),
            body: Body::Bytes(body.into()),
        }
    }

    fn text(status: u16, message: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", message)
    }

    fn html(title: &str, body: &str) -> Self {
        Self::new(200, "text/html; charset=utf-8", page(title, body))
    }

    fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// The body as bytes, reading a file body from disk.
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        match &self.body {
            Body::Bytes(bytes) => Ok(bytes.clone()),
            Body::File(path) => std::fs::read(path),
        }
    }
}

/// The book most recently opened for reading, so its images do not each
/// parse the book again.
type Opened = Option<(i64, PathBuf, Option<SystemTime>, Arc<Document>)>;

pub struct Server {
    listener: TcpListener,
    state: Arc<State>,
}

struct State {
    root: PathBuf,
    /// The expected `Authorization` header value.
    authorization: Option<String>,
    opened: Mutex<Opened>,
    connections: AtomicUsize,
}

impl Server {
    /// Check the library opens and start listening.
    pub fn bind(root: &Path, options: &ServerOptions) -> Result<Self> {
        Library::open(root)?;
        let listener = TcpListener::bind(&options.bind)
            .map_err(|err| Error::Other(format!("cannot listen on {}: {err}", options.bind)))?;
        let authorization = options.credentials.as_ref().map(|(user, password)| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            )
        });
        Ok(Self {
            listener,
            state: Arc::new(State {
                root: root.to_path_buf(),
                authorization,
                opened: Mutex::new(None),
                connections: AtomicUsize::new(0),
            }),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// Answer one request without a socket.
    pub fn respond(&self, target: &str, authorization: Option<&str>) -> Response {
        self.state.respond(target, authorization)
    }

    /// Serve until the process ends.
    pub fn run(&self) -> Result<()> {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let state = Arc::clone(&self.state);
            if state.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                state.connections.fetch_sub(1, Ordering::SeqCst);
                let _ = write_response(&stream, &Response::text(503, "Server busy"), false);
                continue;
            }
            std::thread::spawn(move || {
                let _ = state.connection(stream);
                state.connections.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    }
}

/// Reads from a connection until a fixed time, however slowly bytes arrive.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buffer)
    }
}

impl State {
    fn connection(&self, stream: TcpStream) -> io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;
        let head = Deadline {
            stream: &stream,
            until: Instant::now() + HEAD_TIMEOUT,
        };
        let mut reader = BufReader::new(head.take(MAX_HEAD_BYTES));
        let mut request = String::new();
        reader.read_line(&mut request)?;
        let mut authorization = None;
        let mut complete = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                complete = true;
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                }
            }
        }
        let mut parts = request.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let response = if !complete || target.is_empty() {
            Response::text(400, "Bad request")
        } else if method != "GET" && method != "HEAD" {
            Response::text(405, "Only GET and HEAD are supported").header("Allow", "GET, HEAD")
        } else {
            self.respond(target, authorization.as_deref())
        };
        write_response(&stream, &response, method == "HEAD")
    }

    fn respond(&self, target: &str, authorization: Option<&str>) -> Response {
        if let Some(expected) = &self.authorization {
            if !authorization.is_some_and(|given| same(given.as_bytes(), expected.as_bytes())) {
                return Response::text(401, "Authentication required").header(
                    "WWW-Authenticate",
                    "Basic realm=\"calibre-rust\", charset=\"UTF-8\"",
                );
            }
        }
        match self.route(target) {
            Ok(response) => response,
            Err(Error::BookNotFound(_)) => Response::text(404, "No such book"),
            // Details of a failure stay on the server.
            Err(_) => Response::text(500, "The request could not be completed"),
        }
    }

    fn route(&self, target: &str) -> Result<Response> {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = Query::parse(query);
        let decoded: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| percent_decode_str(part).decode_utf8_lossy().to_string())
            .collect();
        let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();
        let library = Library::open(&self.root)?;
        let id = |text: &str| text.parse::<i64>().map_err(|_| Error::BookNotFound(0));
        Ok(match parts.as_slice() {
            [] => browse(&library, &query)?,
            ["book", book] => details(&library, id(book)?)?,
            ["cover", book] => match library.cover(id(book)?)? {
                Some(cover) => Response::new(200, "image/jpeg", cover)
                    .header("Cache-Control", "private, max-age=3600"),
                None => Response::text(404, "No cover"),
            },
            ["get", book, format] => {
                let book = id(book)?;
                match library.format_path(book, format)? {
                    Some(file) => {
                        let name = file
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        Response {
                            status: 200,
                            content_type: mime(format).to_string(),
                            headers: vec![(
                                "Content-Disposition".to_string(),
                                format!(
                                    "attachment; filename*=UTF-8''{}",
                                    utf8_percent_encode(&name, NON_ALPHANUMERIC)
                                ),
                            )],
                            body: Body::File(file),
                        }
                    }
                    None => Response::text(404, "No such format"),
                }
            }
            ["read", book] if path.ends_with('/') => self.read(&library, id(book)?)?,
            ["read", book] => {
                Response::text(302, "").header("Location", format!("/read/{}/", id(book)?))
            }
            ["read", book, "images", name] => {
                let document = self.open(&library, id(book)?)?;
                match document.images.iter().find(|image| image.name == *name) {
                    Some(image) => Response::new(200, image_mime(name), image.data.clone())
                        .header("Cache-Control", "private, max-age=3600"),
                    None => Response::text(404, "No such image"),
                }
            }
            ["opds"] => opds_root(&library)?,
            ["opds", "books"] => opds_books(&library, &query)?,
            ["opds", "search.xml"] => Response::new(
                200,
                "application/opensearchdescription+xml",
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\">",
                    "<ShortName>Library</ShortName><Description>Search the library</Description>",
                    "<InputEncoding>UTF-8</InputEncoding>",
                    "<Url type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\" ",
                    "template=\"/opds/books?q={searchTerms}\"/>",
                    "</OpenSearchDescription>"
                ),
            ),
            _ => Response::text(404, "Not found"),
        })
    }

    fn open(&self, library: &Library, id: i64) -> Result<Arc<Document>> {
        let format = library.preferred_readable_format(id)?;
        let file = library
            .format_path(id, &format)?
            .ok_or(Error::BookNotFound(id))?;
        let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let mut opened = self.opened.lock().unwrap_or_else(|lock| lock.into_inner());
        if let Some((book, path, time, document)) = opened.as_ref() {
            if *book == id && *path == file && *time == modified {
                return Ok(Arc::clone(document));
            }
        }
        let document = Arc::new(crate::formats::read_document(&file, true)?);
        *opened = Some((id, file, modified, Arc::clone(&document)));
        Ok(document)
    }

    /// The whole book as one page. Its markup is the book's own, so scripts
    /// are refused by the content security policy; only the position script
    /// below, named by a fresh nonce, may run.
    fn read(&self, library: &Library, id: i64) -> Result<Response> {
        let summary = library.get_summary(id)?;
        let document = match self.open(library, id) {
            Ok(document) => document,
            Err(Error::BookNotFound(id)) => return Err(Error::BookNotFound(id)),
            Err(_) => {
                return Ok(Response::text(
                    415,
                    "This book cannot be read in the browser",
                ))
            }
        };
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut body = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>body{{max-width:42em;margin:0 auto;padding:1em;font:1.1em/1.6 Georgia,serif;color:#222;background:#fbfaf7}}img{{max-width:100%;height:auto}}nav{{font:0.85em sans-serif;margin-bottom:2em}}section{{margin-bottom:3em}}@media(prefers-color-scheme:dark){{body{{color:#ddd;background:#181818}}a{{color:#8ab4f8}}}}</style></head><body><nav><a href=\"/book/{id}\">&larr; {}</a></nav>",
            escape(&summary.title),
            escape(&summary.title)
        );
        for section in &document.sections {
            body.push_str("<section>");
            body.push_str(&section.html);
            body.push_str("</section>");
        }
        body.push_str(&format!(
            "<script nonce=\"{nonce}\">(function(){{var k='cr-position-{id}',d=document.documentElement;function h(){{return d.scrollHeight-window.innerHeight}}var s=parseFloat(localStorage.getItem(k));if(s>0&&!location.hash)window.scrollTo(0,s*h());var t;window.addEventListener('scroll',function(){{clearTimeout(t);t=setTimeout(function(){{if(h()>0)localStorage.setItem(k,window.scrollY/h())}},300)}})}})();</script></body></html>"
        ));
        let mut response = Response::new(200, "text/html; charset=utf-8", body);
        response.headers.push((
            "Content-Security-Policy".to_string(),
            format!("{SECURITY}; script-src 'nonce-{nonce}'"),
        ));
        Ok(response)
    }
}

/// Compare without stopping at the first difference.
fn same(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn write_response(mut stream: &TcpStream, response: &Response, head_only: bool) -> io::Result<()> {
    let mut file = None;
    let length = match &response.body {
        Body::Bytes(bytes) => bytes.len() as u64,
        Body::File(path) => {
            let opened = File::open(path)?;
            let length = opened.metadata()?.len();
            file = Some(opened);
            length
        }
    };
    let reason = match response.status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n",
        response.status, response.content_type
    );
    if !response
        .headers
        .iter()
        .any(|(name, _)| name == "Content-Security-Policy")
    {
        head.push_str(&format!("Content-Security-Policy: {SECURITY}\r\n"));
    }
    for (name, value) in &response.headers {
        // A header value never carries a line break.
        let value: String = value.chars().filter(|c| !c.is_control()).collect();
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if !head_only {
        match (&response.body, file) {
            (Body::Bytes(bytes), _) => stream.write_all(bytes)?,
            (Body::File(_), Some(mut file)) => {
                io::copy(&mut file, &mut stream)?;
            }
            (Body::File(_), None) => {}
        }
    }
    stream.flush()
}

struct Query(Vec<(String, String)>);

impl Query {
    fn parse(query: &str) -> Self {
        Self(
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                    let decode = |text: &str| {
                        percent_decode_str(&text.replace('+', " "))
                            .decode_utf8_lossy()
                            .to_string()
                    };
                    (decode(name), decode(value))
                })
                .collect(),
        )
    }

    fn get(&self, name: &str) -> &str {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    }

    fn page(&self) -> usize {
        self.get("page").parse().unwrap_or(1).max(1)
    }
}

fn escape(text: &str) -> String {
    html_escape::encode_double_quoted_attribute(text).to_string()
}

fn encode(text: &str) -> String {
    utf8_percent_encode(text, NON_ALPHANUMERIC).to_string()
}

fn mime(format: &str) -> &'static str {
    match format.to_ascii_uppercase().as_str() {
        "EPUB" | "KEPUB" => "application/epub+zip",
        "MOBI" | "PRC" | "AZW" => "application/x-mobipocket-ebook",
        "AZW3" => "application/vnd.amazon.ebook",
        "PDF" => "application/pdf",
        "TXT" => "text/plain; charset=utf-8",
        "FB2" => "application/x-fictionbook+xml",
        "DOCX" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "RTF" => "application/rtf",
        "CBZ" => "application/vnd.comicbook+zip",
        "CBR" => "application/vnd.comicbook-rar",
        // Anything else is offered as a download, never rendered.
        _ => "application/octet-stream",
    }
}

fn image_mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        // SVG can carry scripts; send unknown types as plain data.
        _ => "application/octet-stream",
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>body{{font:15px/1.5 system-ui,sans-serif;max-width:60em;margin:0 auto;padding:1em;color:#222}}a{{color:#0a58ca;text-decoration:none}}form{{margin:1em 0}}input[type=search]{{width:60%;padding:.4em}}ul.books{{list-style:none;padding:0;display:grid;grid-template-columns:repeat(auto-fill,minmax(9em,1fr));gap:1em}}ul.books img,.none{{width:100%;aspect-ratio:2/3;object-fit:cover;background:#ddd;display:block}}.small{{color:#666;font-size:.85em}}.cover{{float:left;width:12em;margin:0 1.5em 1em 0}}.pages a{{margin-right:1em}}@media(prefers-color-scheme:dark){{body{{background:#181818;color:#ddd}}a{{color:#8ab4f8}}}}</style></head><body>{body}</body></html>",
        escape(title)
    )
}

fn listing(library: &Library, query: &Query) -> Result<(Vec<BookSummary>, usize)> {
    let sort = BookSort::parse(query.get("sort"));
    let options = ListOptions {
        query: Some(query.get("q").to_string()).filter(|q| !q.trim().is_empty()),
        // Newest first unless a sort was asked for.
        descending: sort.is_none() || query.get("order") == "desc",
        sort: Some(sort.unwrap_or(BookSort::Date)),
        limit: None,
    };
    let mut books = library.list_books(&options)?;
    let total = books.len();
    let start = (query.page() - 1).saturating_mul(PAGE_SIZE).min(total);
    books.truncate(start.saturating_add(PAGE_SIZE).min(total));
    books.drain(..start);
    Ok((books, total))
}

fn page_link(base: &str, query: &Query, page: usize) -> String {
    let mut link = format!("{base}?page={page}");
    for name in ["q", "sort", "order"] {
        if !query.get(name).is_empty() {
            link.push_str(&format!("&{name}={}", encode(query.get(name))));
        }
    }
    link
}

fn browse(library: &Library, query: &Query) -> Result<Response> {
    let (books, total) = match listing(library, query) {
        Ok(found) => found,
        // A search that does not parse is the visitor's mistake.
        Err(error) if !query.get("q").is_empty() => {
            return Ok(Response::new(
                400,
                "text/html; charset=utf-8",
                page(
                    "Search",
                    &format!(
                    "<p>That search could not be understood: {}</p><p><a href=\"/\">Back</a></p>",
                    escape(&error.to_string())
                ),
                ),
            ))
        }
        Err(error) => return Err(error),
    };
    let mut body = format!(
        "<form action=\"/\" method=\"get\"><input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"Search\"> <select name=\"sort\">",
        escape(query.get("q"))
    );
    for (value, label) in [
        ("", "Newest"),
        ("title", "Title"),
        ("author", "Author"),
        ("series", "Series"),
        ("pubdate", "Published"),
        ("rating", "Rating"),
    ] {
        let selected = if query.get("sort") == value {
            " selected"
        } else {
            ""
        };
        body.push_str(&format!(
            "<option value=\"{value}\"{selected}>{label}</option>"
        ));
    }
    body.push_str(&format!(
        "</select> <button>Search</button></form><p class=\"small\">{total} book(s) &middot; <a href=\"/opds\">OPDS catalog</a></p><ul class=\"books\">"
    ));
    for book in &books {
        let cover = if book.has_cover {
            format!("<img src=\"/cover/{}\" alt=\"\" loading=\"lazy\">", book.id)
        } else {
            "<span class=\"none\"></span>".to_string()
        };
        body.push_str(&format!(
            "<li><a href=\"/book/{}\">{cover}{}</a><div class=\"small\">{}</div></li>",
            book.id,
            escape(&book.title),
            escape(&book.authors.join(", "))
        ));
    }
    body.push_str("</ul><p class=\"pages\">");
    let current = query.page();
    if current > 1 {
        body.push_str(&format!(
            "<a href=\"{}\">&larr; Previous</a>",
            escape(&page_link("/", query, current - 1))
        ));
    }
    if current * PAGE_SIZE < total {
        body.push_str(&format!(
            "<a href=\"{}\">Next &rarr;</a>",
            escape(&page_link("/", query, current + 1))
        ));
    }
    body.push_str("</p>");
    Ok(Response::html("Library", &body))
}

fn details(library: &Library, id: i64) -> Result<Response> {
    let meta = library.get_metadata(id)?;
    let formats = library.formats(id)?;
    let mut body = String::from("<p><a href=\"/\">&larr; Library</a></p>");
    if library.cover(id)?.is_some() {
        body.push_str(&format!(
            "<img class=\"cover\" src=\"/cover/{id}\" alt=\"\">"
        ));
    }
    body.push_str(&format!(
        "<h1>{}</h1><p>{}</p>",
        escape(&meta.title),
        escape(&meta.authors.join(", "))
    ));
    let mut facts = Vec::new();
    if let Some(series) = &meta.series {
        facts.push(format!("{} #{}", escape(series), meta.series_index));
    }
    if let Some(publisher) = &meta.publisher {
        facts.push(escape(publisher));
    }
    if let Some(date) = meta.pubdate {
        facts.push(date.format("%Y").to_string());
    }
    if !meta.tags.is_empty() {
        facts.push(escape(&meta.tags.join(", ")));
    }
    if !facts.is_empty() {
        body.push_str(&format!(
            "<p class=\"small\">{}</p>",
            facts.join(" &middot; ")
        ));
    }
    body.push_str("<p>");
    if library.preferred_readable_format(id).is_ok() {
        body.push_str(&format!("<a href=\"/read/{id}/\">Read</a> &middot; "));
    }
    let downloads: Vec<String> = formats
        .iter()
        .map(|format| {
            format!(
                "<a href=\"/get/{id}/{}\">{}</a>",
                encode(format),
                escape(format)
            )
        })
        .collect();
    body.push_str(&format!("Download: {}</p>", downloads.join(" ")));
    if let Some(comments) = &meta.comments {
        // Shown as text: the stored comment is HTML from any source.
        body.push_str(&format!("<p>{}</p>", escape(strip_tags(comments).trim())));
    }
    Ok(Response::html(&meta.title, &body))
}

const ATOM: &str = "application/atom+xml;profile=opds-catalog";

fn feed_head(id: &str, title: &str, updated: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:dc=\"http://purl.org/dc/terms/\" xmlns:opds=\"http://opds-spec.org/2010/catalog\"><id>{id}</id><title>{}</title><updated>{updated}</updated><author><name>calibre-rust</name></author><link rel=\"start\" href=\"/opds\" type=\"{ATOM};kind=navigation\"/><link rel=\"search\" href=\"/opds/search.xml\" type=\"application/opensearchdescription+xml\"/>",
        escape(title)
    )
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn opds_root(library: &Library) -> Result<Response> {
    let library_id = library.library_id()?;
    let updated = now();
    let mut feed = feed_head(
        &format!("urn:uuid:{}", escape(&library_id)),
        "Library",
        &updated,
    );
    feed.push_str(&format!(
        "<link rel=\"self\" href=\"/opds\" type=\"{ATOM};kind=navigation\"/>"
    ));
    for (name, title, link) in [
        ("newest", "Newest", "/opds/books"),
        ("title", "By title", "/opds/books?sort=title"),
        ("author", "By author", "/opds/books?sort=author"),
        ("series", "By series", "/opds/books?sort=series"),
    ] {
        let relation = if name == "newest" {
            "http://opds-spec.org/sort/new"
        } else {
            "subsection"
        };
        feed.push_str(&format!(
            "<entry><id>urn:uuid:{}:{name}</id><title>{title}</title><updated>{updated}</updated><content type=\"text\">{title}</content><link rel=\"{relation}\" href=\"{}\" type=\"{ATOM};kind=acquisition\"/></entry>",
            escape(&library_id),
            escape(link)
        ));
    }
    feed.push_str("</feed>");
    Ok(Response::new(200, &format!("{ATOM};kind=navigation"), feed))
}

fn opds_books(library: &Library, query: &Query) -> Result<Response> {
    let (books, total) = match listing(library, query) {
        Ok(found) => found,
        Err(_) if !query.get("q").is_empty() => {
            return Ok(Response::text(400, "That search could not be understood"))
        }
        Err(error) => return Err(error),
    };
    let library_id = library.library_id()?;
    let current = query.page();
    let mut feed = feed_head(
        &format!("urn:uuid:{}:books", escape(&library_id)),
        "Books",
        &now(),
    );
    feed.push_str(&format!(
        "<link rel=\"self\" href=\"{}\" type=\"{ATOM};kind=acquisition\"/>",
        escape(&page_link("/opds/books", query, current))
    ));
    if current > 1 {
        feed.push_str(&format!(
            "<link rel=\"previous\" href=\"{}\" type=\"{ATOM};kind=acquisition\"/>",
            escape(&page_link("/opds/books", query, current - 1))
        ));
    }
    if current * PAGE_SIZE < total {
        feed.push_str(&format!(
            "<link rel=\"next\" href=\"{}\" type=\"{ATOM};kind=acquisition\"/>",
            escape(&page_link("/opds/books", query, current + 1))
        ));
    }
    for book in &books {
        let meta = library.get_metadata(book.id)?;
        let updated = meta
            .last_modified
            .or(meta.timestamp)
            .map(|date| date.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(now);
        feed.push_str(&format!(
            "<entry><id>urn:uuid:{}</id><title>{}</title><updated>{updated}</updated>",
            escape(
                meta.uuid
                    .as_deref()
                    .unwrap_or(&format!("{library_id}:{}", book.id))
            ),
            escape(&meta.title)
        ));
        for author in &meta.authors {
            feed.push_str(&format!("<author><name>{}</name></author>", escape(author)));
        }
        for language in &meta.languages {
            feed.push_str(&format!("<dc:language>{}</dc:language>", escape(language)));
        }
        if let Some(publisher) = &meta.publisher {
            feed.push_str(&format!(
                "<dc:publisher>{}</dc:publisher>",
                escape(publisher)
            ));
        }
        for tag in &meta.tags {
            feed.push_str(&format!(
                "<category term=\"{0}\" label=\"{0}\"/>",
                escape(tag)
            ));
        }
        if let Some(comments) = &meta.comments {
            feed.push_str(&format!(
                "<summary type=\"text\">{}</summary>",
                escape(strip_tags(comments).trim())
            ));
        }
        if book.has_cover {
            feed.push_str(&format!(
                "<link rel=\"http://opds-spec.org/image\" href=\"/cover/{0}\" type=\"image/jpeg\"/><link rel=\"http://opds-spec.org/image/thumbnail\" href=\"/cover/{0}\" type=\"image/jpeg\"/>",
                book.id
            ));
        }
        for format in &book.formats {
            feed.push_str(&format!(
                "<link rel=\"http://opds-spec.org/acquisition\" href=\"/get/{}/{}\" type=\"{}\"/>",
                book.id,
                encode(format),
                escape(mime(format))
            ));
        }
        feed.push_str("</entry>");
    }
    feed.push_str("</feed>");
    Ok(Response::new(
        200,
        &format!("{ATOM};kind=acquisition"),
        feed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_strings_decode() {
        let query = Query::parse("q=tag%3A%22a+b%22&page=3&empty");
        assert_eq!(query.get("q"), "tag:\"a b\"");
        assert_eq!(query.page(), 3);
        assert_eq!(query.get("missing"), "");
        assert_eq!(Query::parse("page=0").page(), 1);
    }

    #[test]
    fn comparison_and_types() {
        assert!(same(b"Basic abc", b"Basic abc"));
        assert!(!same(b"Basic abd", b"Basic abc"));
        assert!(!same(b"Basic ab", b"Basic abc"));
        assert_eq!(mime("epub"), "application/epub+zip");
        assert_eq!(mime("HTML"), "application/octet-stream");
        assert_eq!(image_mime("a.svg"), "application/octet-stream");
    }
}
