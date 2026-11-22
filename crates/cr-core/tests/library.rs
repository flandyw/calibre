use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use base64::Engine;
use cr_core::device::{self, Device, SendOptions};
use cr_core::formats;
use cr_core::library::{BookUpdate, Library, ListOptions};
use cr_core::opf;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn build_epub(path: &Path) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("mimetype", stored).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();

    let container = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
    zip.start_file("META-INF/container.xml", deflated).unwrap();
    zip.write_all(container.as_bytes()).unwrap();

    let opf = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="uuid_id" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:identifier opf:scheme="uuid" id="uuid_id">11111111-1111-1111-1111-111111111111</dc:identifier>
    <dc:identifier opf:scheme="ISBN">9780000000000</dc:identifier>
    <dc:title>The Hobbit</dc:title>
    <dc:creator opf:file-as="Tolkien, J. R. R." opf:role="aut">J. R. R. Tolkien</dc:creator>
    <dc:language>en</dc:language>
    <dc:publisher>Allen &amp; Unwin</dc:publisher>
    <dc:date>1937-09-21</dc:date>
    <dc:description>&lt;p&gt;A hobbit goes there and back again.&lt;/p&gt;</dc:description>
    <dc:subject>Fantasy</dc:subject>
    <dc:subject>Classic</dc:subject>
    <meta name="calibre:series" content="Middle Earth"/>
    <meta name="calibre:series_index" content="0.5"/>
    <meta name="calibre:rating" content="8"/>
    <meta name="cover" content="cover"/>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
  <spine><itemref idref="ch1"/><itemref idref="ch2"/></spine>
  <guide/>
</package>"#;
    zip.start_file("OEBPS/content.opf", deflated).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();

    let ch1 = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter One</title></head>
<body><h1>An Unexpected Party</h1><p>In a hole in the ground there lived a hobbit.</p></body></html>"#;
    zip.start_file("OEBPS/ch1.xhtml", deflated).unwrap();
    zip.write_all(ch1.as_bytes()).unwrap();

    let ch2 = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter Two</title></head>
<body><h1>Roast Mutton</h1><p>There and back again.</p></body></html>"#;
    zip.start_file("OEBPS/ch2.xhtml", deflated).unwrap();
    zip.write_all(ch2.as_bytes()).unwrap();

    let png = base64::engine::general_purpose::STANDARD
        .decode(PNG_1X1)
        .unwrap();
    zip.start_file("OEBPS/cover.png", deflated).unwrap();
    zip.write_all(&png).unwrap();
    zip.finish().unwrap();
}

#[test]
fn kindle_round_trip_preserves_chapters_and_cover() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source.epub");
    build_epub(&source);
    let expected = formats::extract_text(&source, None).unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();

    for extension in ["azw3", "mobi"] {
        let kindle = temp.path().join(format!("book.{extension}"));
        formats::convert(&source, &kindle).unwrap();
        let extensionless = temp.path().join("unnamed");
        fs::copy(&kindle, &extensionless).unwrap();
        assert_eq!(
            formats::detect_format(&extensionless),
            extension.to_uppercase()
        );

        let added = library.add_book(&kindle).unwrap();
        assert_eq!(added.title, "The Hobbit");
        assert_eq!(added.authors, ["J. R. R. Tolkien"]);
        assert_eq!(added.formats, [extension.to_uppercase()]);
        assert!(library.cover(added.id).unwrap().is_some());
        assert_eq!(library.read_book(added.id, None).unwrap(), expected);

        let recovered = temp.path().join(format!("recovered-{extension}.epub"));
        formats::convert(&kindle, &recovered).unwrap();
        assert_eq!(formats::extract_text(&recovered, None).unwrap(), expected);
        assert!(formats::import(&recovered)
            .unwrap()
            .metadata
            .cover
            .is_some());

        let plain = temp.path().join(format!("recovered-{extension}.txt"));
        formats::convert(&kindle, &plain).unwrap();
        let text = fs::read_to_string(&plain).unwrap();
        assert!(text.contains("An Unexpected Party\n\nIn a hole"));
        assert!(text.contains("Roast Mutton\n\nThere and back again."));
    }
}

#[test]
fn kindle_conversion_keeps_existing_output_and_rejects_drm() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source.epub");
    let kindle = temp.path().join("book.azw3");
    build_epub(&source);
    formats::convert(&source, &kindle).unwrap();
    let original = fs::read(&kindle).unwrap();
    assert!(formats::convert(&source, &kindle).is_err());
    assert_eq!(fs::read(&kindle).unwrap(), original);

    let mut encrypted = original;
    let offset = u32::from_be_bytes(encrypted[78..82].try_into().unwrap()) as usize;
    encrypted[offset + 12..offset + 14].copy_from_slice(&2_u16.to_be_bytes());
    fs::write(&kindle, encrypted).unwrap();
    assert!(formats::extract_text(&kindle, None)
        .unwrap_err()
        .to_string()
        .contains("DRM-protected"));
    let destination = temp.path().join("recovered.epub");
    assert!(formats::convert(&kindle, &destination).is_err());
    assert!(!destination.exists());
}

#[test]
fn full_library_lifecycle() {
    let temp = tempdir().unwrap();
    let library_path = temp.path().join("library");
    let epub_path = temp.path().join("hobbit.epub");
    build_epub(&epub_path);

    let mut library = Library::create(&library_path).unwrap();
    assert_eq!(library.schema_version().unwrap(), 28);
    let library_id = library.library_id().unwrap();
    assert!(!library_id.is_empty());

    let added = library.add_book(&epub_path).unwrap();
    assert_eq!(added.title, "The Hobbit");
    assert_eq!(added.authors, vec!["J. R. R. Tolkien"]);
    assert_eq!(added.formats, vec!["EPUB"]);

    let meta = library.get_metadata(added.id).unwrap();
    assert_eq!(meta.authors, vec!["J. R. R. Tolkien"]);
    assert_eq!(meta.publisher.as_deref(), Some("Allen & Unwin"));
    assert_eq!(meta.series.as_deref(), Some("Middle Earth"));
    assert_eq!(meta.series_index, 0.5);
    assert_eq!(meta.rating, Some(8));
    assert_eq!(meta.tags, vec!["Fantasy", "Classic"]);
    assert_eq!(meta.languages, vec!["en"]);
    assert_eq!(
        meta.identifiers.get("isbn").map(String::as_str),
        Some("9780000000000")
    );
    assert_eq!(meta.computed_title_sort(), "Hobbit, The");
    assert_eq!(meta.computed_author_sort(), "Tolkien, J. R. R.");
    assert!(meta.cover.is_some());

    let summary = library.get_summary(added.id).unwrap();
    assert_eq!(summary.formats, vec!["EPUB"]);
    assert_eq!(summary.authors, vec!["J. R. R. Tolkien"]);

    let book_dir = library_path.join(&added.path);
    // calibre names the directory after the first author.
    assert_eq!(added.path, "J. R. R. Tolkien/The Hobbit (1)");
    assert!(book_dir.join("metadata.opf").is_file());
    assert!(book_dir.join("cover.jpg").is_file());
    assert!(book_dir
        .join("The Hobbit - J. R. R. Tolkien.epub")
        .is_file());
    let opf_meta = opf::parse_opf(&fs::read(book_dir.join("metadata.opf")).unwrap()).unwrap();
    assert_eq!(opf_meta.title, "The Hobbit");
    assert_eq!(opf_meta.series.as_deref(), Some("Middle Earth"));

    // Search the way the CLI does.
    let all = library.list_books(&ListOptions::default()).unwrap();
    assert_eq!(all.len(), 1);
    let by_title = library
        .list_books(&ListOptions {
            query: Some("Hobbit".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_title.len(), 1);
    let by_tag = library
        .list_books(&ListOptions {
            query: Some("tag:fantasy".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_tag.len(), 1);
    let by_missing_tag = library
        .list_books(&ListOptions {
            query: Some("!tag:romance".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_missing_tag.len(), 1);

    // Read spine-ordered text.
    let book = library.read_book(added.id, None).unwrap();
    assert_eq!(book.chapters.len(), 2);
    assert_eq!(book.chapters[0].title, "An Unexpected Party");
    assert!(book.chapters[0].text.contains("there lived a hobbit"));
    assert_eq!(book.chapters[1].title, "Roast Mutton");
    assert!(book.chapters[1].text.contains("There and back again"));
    assert_eq!(book.word_count(), 14);

    // Edit metadata and check the directory/OPF follow.
    let patch = BookUpdate {
        title: Some("The Hobbit (Revised)".into()),
        tags: Some(vec!["Fantasy".into(), "Adventure".into()]),
        rating: Some(Some(10)),
        ..Default::default()
    };
    library.update_book(added.id, &patch).unwrap();
    let meta = library.get_metadata(added.id).unwrap();
    assert_eq!(meta.title, "The Hobbit (Revised)");
    assert_eq!(meta.computed_title_sort(), "Hobbit (Revised), The");
    assert_eq!(meta.tags, vec!["Fantasy", "Adventure"]);
    assert_eq!(meta.rating, Some(10));
    assert!(meta.cover.is_some());
    let new_dir = library_path.join(library.book_path(added.id).unwrap());
    assert!(new_dir.join("metadata.opf").is_file());
    assert!(new_dir.join("cover.jpg").is_file());
    assert!(!book_dir.exists());
    let round_trip = opf::parse_opf(&fs::read(new_dir.join("metadata.opf")).unwrap()).unwrap();
    assert_eq!(round_trip.title, "The Hobbit (Revised)");
    assert_eq!(round_trip.tags, vec!["Fantasy", "Adventure"]);

    // Unpack, edit a chapter, repack and replace the library copy.
    let edit_dir = temp.path().join("edit");
    let count = library.unpack_book(added.id, "EPUB", &edit_dir).unwrap();
    assert!(count >= 5);
    assert!(edit_dir.join("OEBPS/ch1.xhtml").is_file());
    let chapter_path = edit_dir.join("OEBPS/ch1.xhtml");
    let mut chapter_html = fs::read_to_string(&chapter_path).unwrap();
    chapter_html = chapter_html.replace(
        "In a hole in the ground there lived a hobbit.",
        "In a hole in the ground there lived an edited hobbit.",
    );
    fs::write(&chapter_path, chapter_html).unwrap();
    let packed = temp.path().join("packed.epub");
    formats::epub::repack(&edit_dir, &packed).unwrap();
    library.replace_format(added.id, "EPUB", &packed).unwrap();
    let reread = library.read_book(added.id, None).unwrap();
    assert!(reread.chapters[0].text.contains("an edited hobbit"));

    // Export with a template.
    let export_dir = temp.path().join("export");
    let written = library
        .export_books(
            &[added.id],
            &export_dir,
            "{author_sort}/{title} ({id})/{title} - {author}",
            Some("EPUB"),
        )
        .unwrap();
    assert_eq!(written.len(), 1);
    assert!(written[0].is_file());
    assert!(written[0].to_string_lossy().contains("Tolkien, J. R. R"));

    // Send to a fake Kindle mount.
    let device_root = temp.path().join("kindle");
    fs::create_dir_all(device_root.join("system")).unwrap();
    fs::create_dir_all(device_root.join("documents")).unwrap();
    let device = Device::detect_at(&device_root).unwrap();
    let report = device::send(&library, &device, &[added.id], &SendOptions::default()).unwrap();
    assert_eq!(report.sent.len(), 1);
    assert!(report.sent[0].path.is_file());
    assert!(report.sent[0].path.to_string_lossy().contains("documents"));
    let catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(device.catalog_path()).unwrap()).unwrap();
    assert_eq!(catalog[0]["title"], "The Hobbit (Revised)");
    assert_eq!(catalog[0]["authors"][0], "J. R. R. Tolkien");
    // A Kindle cannot read EPUB, so the copy is converted for the send.
    assert_eq!(catalog[0]["format"], "AZW3");

    // Remove and delete the book folder.
    let last_dir = library_path.join(library.book_path(added.id).unwrap());
    library.remove_book(added.id, true).unwrap();
    assert!(!last_dir.exists());
    assert!(library
        .list_books(&ListOptions::default())
        .unwrap()
        .is_empty());
    assert!(library.get_metadata(added.id).is_err());
}

#[test]
fn imports_plain_text_and_opens_existing_library() {
    let temp = tempdir().unwrap();
    let library_path = temp.path().join("library");
    let text_path = temp.path().join("A Study in Scarlet.txt");
    fs::write(
        &text_path,
        "Chapter 1\nMr. Sherlock Holmes sat at breakfast.\n\nChapter 2\nThe science of deduction.\n",
    )
    .unwrap();

    {
        let mut library = Library::create(&library_path).unwrap();
        let added = library.add_book(&text_path).unwrap();
        assert_eq!(added.title, "A Study in Scarlet");
        let meta = library.get_metadata(added.id).unwrap();
        assert_eq!(meta.authors, vec!["Unknown"]);
        let read = library.read_book(added.id, None).unwrap();
        assert_eq!(read.chapters.len(), 2);
        assert_eq!(read.chapters[0].title, "Chapter 1");
        assert_eq!(read.chapters[1].title, "Chapter 2");
    }

    // Reopen from disk, as a separate process would.
    let library = Library::open(&library_path).unwrap();
    let books = library.list_books(&ListOptions::default()).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].formats, vec!["TXT"]);
    assert_eq!(library.stats().unwrap().books, 1);
}
