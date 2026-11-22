//! The content server, catalog formats and library maintenance.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use cr_core::formats::convert;
use cr_core::library::{BookUpdate, CatalogFormat, Library, Problem};
use cr_core::server::{Server, ServerOptions};
use tempfile::tempdir;

fn make_epub(directory: &Path, title: &str, body: &str) -> PathBuf {
    let html = directory.join(format!("{title}.html"));
    fs::write(
        &html,
        format!("<html><head><title>{title}</title></head><body><h1>One</h1>{body}</body></html>"),
    )
    .unwrap();
    let output = directory.join(format!("{title}.epub"));
    convert(&html, &output).unwrap();
    output
}

fn library_with_books(directory: &Path) -> (Library, i64, i64) {
    let mut library = Library::create(&directory.join("library")).unwrap();
    let first = library
        .add_book(&make_epub(
            directory,
            "Alpha",
            "<p onclick=\"steal()\">Hello reader.</p><script>steal()</script>",
        ))
        .unwrap()
        .id;
    let second = library
        .add_book(&make_epub(directory, "Beta", "<p>Second.</p>"))
        .unwrap()
        .id;
    library
        .update_book(
            first,
            &BookUpdate {
                title: Some("Alpha <b>&".into()),
                authors: Some(vec!["Ann O'Hara".into()]),
                comments: Some(Some("<p>Good <script>alert(1)</script>book</p>".into())),
                tags: Some(vec!["sci_fi".into()]),
                ..Default::default()
            },
        )
        .unwrap();
    (library, first, second)
}

fn text(server: &Server, target: &str) -> (u16, String) {
    let response = server.respond(target, None);
    (
        response.status,
        String::from_utf8_lossy(&response.bytes().unwrap()).to_string(),
    )
}

#[test]
fn browse_search_download_read_and_opds() {
    let temp = tempdir().unwrap();
    let (library, first, second) = library_with_books(temp.path());
    let options = ServerOptions {
        bind: "127.0.0.1:0".into(),
        credentials: None,
    };
    let server = Server::bind(library.root(), &options).unwrap();

    let (status, home) = text(&server, "/");
    assert_eq!(status, 200);
    assert!(home.contains("Alpha &lt;b&gt;&amp;"));
    assert!(home.contains("Beta"));
    assert!(!home.contains("<b>&"));

    let (_, found) = text(&server, "/?q=title%3ABeta");
    assert!(found.contains("Beta") && !found.contains("Alpha"));

    // Comments are shown as text, never as markup.
    let (status, details) = text(&server, &format!("/book/{first}"));
    assert_eq!(status, 200);
    assert!(details.contains("Ann O&#x27;Hara") || details.contains("Ann O'Hara"));
    assert!(!details.contains("<script"));
    assert_eq!(text(&server, "/book/999").0, 404);
    assert_eq!(text(&server, "/book/abc").0, 404);
    assert_eq!(text(&server, "/nothing").0, 404);

    // Downloads are the library's bytes.
    let download = server.respond(&format!("/get/{second}/epub"), None);
    assert_eq!(download.status, 200);
    assert_eq!(download.content_type, "application/epub+zip");
    assert_eq!(
        download.bytes().unwrap(),
        fs::read(library.format_path(second, "EPUB").unwrap().unwrap()).unwrap()
    );
    assert!(download
        .headers
        .iter()
        .any(|(name, value)| name == "Content-Disposition" && value.starts_with("attachment")));
    assert_eq!(text(&server, &format!("/get/{second}/pdf")).0, 404);
    // Nothing outside the library's records is reachable.
    assert_eq!(text(&server, "/get/1/..%2F..%2Fmetadata.db").0, 404);
    assert_eq!(text(&server, "/cover/../metadata.db").0, 404);

    // The reading page carries the book and a policy that refuses its scripts.
    let page = server.respond(&format!("/read/{first}/"), None);
    assert_eq!(page.status, 200);
    let policy = &page
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Security-Policy")
        .unwrap()
        .1;
    assert!(policy.contains("default-src 'none'"));
    assert!(policy.contains("script-src 'nonce-"));
    assert!(!policy.contains("unsafe-inline' 'nonce"));
    let body = String::from_utf8(page.bytes().unwrap()).unwrap();
    assert!(body.contains("Hello reader."));
    let redirect = server.respond(&format!("/read/{first}"), None);
    assert_eq!(redirect.status, 302);

    let (status, root) = text(&server, "/opds");
    assert_eq!(status, 200);
    assert!(root.contains("kind=acquisition"));
    cr_core::xml::parse(root.as_bytes()).unwrap();
    let feed = server.respond("/opds/books?sort=title", None);
    assert!(feed.content_type.contains("kind=acquisition"));
    let feed = String::from_utf8(feed.bytes().unwrap()).unwrap();
    let parsed = cr_core::xml::parse(feed.as_bytes()).unwrap();
    assert_eq!(parsed.descendants("entry").len(), 2);
    assert!(feed.contains(&format!("href=\"/get/{first}/EPUB\"")));
    assert!(feed.contains("http://opds-spec.org/acquisition"));
    assert!(feed.contains("Good"));
    assert!(!feed.contains("<script"));
    let (_, search) = text(&server, "/opds/search.xml");
    assert!(search.contains("{searchTerms}"));
    cr_core::xml::parse(search.as_bytes()).unwrap();
}

#[test]
fn password_and_sockets() {
    let temp = tempdir().unwrap();
    let (library, ..) = library_with_books(temp.path());
    let options = ServerOptions {
        bind: "127.0.0.1:0".into(),
        credentials: Some(("ann".into(), "s3cret".into())),
    };
    let server = Server::bind(library.root(), &options).unwrap();
    assert_eq!(server.respond("/", None).status, 401);
    assert_eq!(
        server.respond("/opds", Some("Basic YW5uOndyb25n")).status,
        401
    );
    // ann:s3cret
    assert_eq!(
        server.respond("/", Some("Basic YW5uOnMzY3JldA==")).status,
        200
    );

    let address = server.local_addr().unwrap();
    std::thread::spawn(move || server.run());
    let request = |text: &str| {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(text.as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    };
    let denied = request("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(denied.starts_with("HTTP/1.1 401"));
    assert!(denied.contains("WWW-Authenticate: Basic"));
    let allowed =
        request("GET / HTTP/1.1\r\nHost: x\r\nauthorization: Basic YW5uOnMzY3JldA==\r\n\r\n");
    assert!(allowed.starts_with("HTTP/1.1 200"));
    assert!(allowed.contains("X-Content-Type-Options: nosniff"));
    assert!(allowed.contains("Beta"));
    let head = request("HEAD / HTTP/1.1\r\nAuthorization: Basic YW5uOnMzY3JldA==\r\n\r\n");
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(head.ends_with("\r\n\r\n"));
    assert!(request("POST / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 405"));
    assert!(request("nonsense\r\n\r\n").starts_with("HTTP/1.1 400"));
}

#[test]
fn catalog_formats() {
    let temp = tempdir().unwrap();
    let (library, first, _) = library_with_books(temp.path());

    let xml = library.catalog("", CatalogFormat::Xml).unwrap();
    let parsed = cr_core::xml::parse(xml.as_bytes()).unwrap();
    assert_eq!(parsed.descendants("record").len(), 2);
    assert!(xml.contains("<title>Alpha &lt;b&gt;&amp;</title>"));
    assert!(xml.contains("<authors><item>Ann O"));

    let bibtex = library.catalog("", CatalogFormat::Bibtex).unwrap();
    assert_eq!(bibtex.matches("@book{").count(), 2);
    assert!(bibtex.contains(&format!("_{first},")));
    assert!(bibtex.contains("title = {Alpha <b>\\&}"));
    assert!(bibtex.contains("keywords = {sci\\_fi}"));
    assert!(bibtex.contains("author = {Ann O'Hara}"));

    let output = temp.path().join("catalog.epub");
    library
        .catalog_epub("title:Beta", &mut fs::File::create(&output).unwrap())
        .unwrap();
    let document = cr_core::formats::read_document(&output, false).unwrap();
    assert_eq!(document.metadata.title, "Library catalog");
    let listing = document
        .sections
        .iter()
        .map(|s| s.html.as_str())
        .collect::<String>();
    assert!(listing.contains("Beta") && !listing.contains("Alpha"));
}

#[test]
fn check_repair_and_rebuild() {
    let temp = tempdir().unwrap();
    let (mut library, first, second) = library_with_books(temp.path());
    let root = library.root().to_path_buf();
    assert_eq!(library.check().unwrap(), []);
    let uuid = library.get_metadata(first).unwrap().uuid;

    let first_dir = root.join(library.book_path(first).unwrap());
    let second_dir = root.join(library.book_path(second).unwrap());
    let epub = library.format_path(first, "EPUB").unwrap().unwrap();
    fs::remove_file(&epub).unwrap();
    // The second book's file was renamed by hand and a copy left beside it.
    let second_epub = library.format_path(second, "EPUB").unwrap().unwrap();
    let second_bytes = fs::read(&second_epub).unwrap();
    fs::write(first_dir.join("Stray.pdf"), "pdf").unwrap();
    fs::write(first_dir.join("notes.json"), "{}").unwrap();
    fs::remove_file(second_dir.join("metadata.opf")).unwrap();
    fs::write(second_dir.join("cover.jpg"), "jpg").unwrap();
    fs::create_dir_all(root.join("Nobody/Lost (99)")).unwrap();

    let problems = library.check().unwrap();
    assert!(problems.contains(&Problem::MissingFormat {
        id: first,
        format: "EPUB".into()
    }));
    assert!(problems.contains(&Problem::UntrackedFormat {
        id: first,
        file: "Stray.pdf".into()
    }));
    assert!(problems.contains(&Problem::MissingOpf { id: second }));
    assert!(problems.contains(&Problem::UnrecordedCover { id: second }));
    assert!(problems.contains(&Problem::UnknownFolder {
        path: "Nobody/Lost (99)".into()
    }));
    assert_eq!(problems.len(), 5);
    let renamed = second_dir.join("Renamed.epub");
    fs::rename(&second_epub, &renamed).unwrap();
    let problems = library.check().unwrap();
    assert!(problems.contains(&Problem::UntrackedFormat {
        id: second,
        file: "Renamed.epub".into()
    }));
    assert_eq!(library.repair(&problems).unwrap(), 6);
    assert_eq!(
        library.format_path(second, "EPUB").unwrap().unwrap(),
        renamed
    );
    fs::rename(&renamed, &second_epub).unwrap();
    fs::write(second_dir.join("A copy.epub"), "copy").unwrap();
    let problems = library.check().unwrap();

    library.repair(&problems).unwrap();
    assert_eq!(library.formats(first).unwrap(), ["PDF"]);
    assert_eq!(
        fs::read(library.format_path(first, "PDF").unwrap().unwrap()).unwrap(),
        b"pdf"
    );
    assert!(second_dir.join("metadata.opf").is_file());
    assert!(library.get_summary(second).unwrap().has_cover);
    // Only the folder that belongs to no book is left to the user.
    assert_eq!(
        library.check().unwrap(),
        [Problem::UnknownFolder {
            path: "Nobody/Lost (99)".into()
        }]
    );
    assert!(root.join("Nobody/Lost (99)").is_dir());

    // Lose the database and rebuild it from the OPF files.
    let titles = |library: &Library| {
        let mut titles: Vec<(i64, String)> = [first, second]
            .iter()
            .map(|id| (*id, library.get_metadata(*id).unwrap().title))
            .collect();
        titles.sort();
        titles
    };
    let before = titles(&library);
    let authors = library.get_metadata(first).unwrap().authors;
    drop(library);
    fs::write(root.join("metadata.db"), "damaged").unwrap();
    assert!(Library::open(&root).is_err());

    let (rebuilt, report) = Library::rebuild(&root).unwrap();
    assert_eq!(report.restored, 2, "{:?}", report.failed);
    assert!(report.failed.is_empty());
    assert_eq!(fs::read(report.backup.unwrap()).unwrap(), b"damaged");
    assert_eq!(titles(&rebuilt), before);
    assert_eq!(rebuilt.get_metadata(first).unwrap().authors, authors);
    assert_eq!(rebuilt.get_metadata(first).unwrap().uuid, uuid);
    assert_eq!(rebuilt.formats(first).unwrap(), ["PDF"]);
    assert_eq!(rebuilt.formats(second).unwrap(), ["EPUB"]);
    assert_eq!(
        fs::read(rebuilt.format_path(second, "EPUB").unwrap().unwrap()).unwrap(),
        second_bytes
    );
    assert_eq!(fs::read(second_dir.join("A copy.epub")).unwrap(), b"copy");
    assert!(rebuilt
        .format_path(second, "EPUB")
        .unwrap()
        .unwrap()
        .is_file());
    assert!(first_dir.join("notes.json").is_file());
    assert_eq!(
        rebuilt.check().unwrap(),
        [Problem::UnknownFolder {
            path: "Nobody/Lost (99)".into()
        }]
    );
    drop(rebuilt);
    assert!(Library::open(&root).is_ok());

    // A second rebuild keeps the first backup.
    let (_, again) = Library::rebuild(&root).unwrap();
    assert_eq!(again.restored, 2);
    assert_eq!(
        fs::read(root.join("metadata_pre_restore.db")).unwrap(),
        b"damaged"
    );
    assert!(root.join("metadata_pre_restore_1.db").is_file());

    // Nothing to rebuild from is an error that leaves the folder alone.
    let empty = temp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert!(Library::rebuild(&empty).is_err());
    assert!(!empty.join("metadata.db").exists());
}
