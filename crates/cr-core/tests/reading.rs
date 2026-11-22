//! The reading model: layout, table of contents, search, positions and
//! annotations.

use std::fs;

use cr_core::formats::reading::{BlockKind, Book, Locator, Style};
use cr_core::library::{AnnotationFormat, AnnotationKind, Library};
use tempfile::tempdir;

const PAGE: &str = r##"<html><head><title>Reader</title></head><body>
<h1 id="one">One</h1>
<p>Plain <b>bold <i>both</i></b> and a <a href="#two">link</a>.</p>
<ul><li>first</li><li>second</li></ul>
<ol><li>uno</li></ol>
<blockquote><p>Quoted words.</p></blockquote>
<table><tr><td>a</td><td>b</td></tr></table>
<pre>fn  main()
  body</pre>
<hr>
<h2 id="two">Two</h2>
<p>Second chapter text with Needle and needle.</p>
<h3>Deeper</h3><p>Tail.</p>
</body></html>"##;

fn book(directory: &std::path::Path) -> Book {
    let path = directory.join("reader.html");
    fs::write(&path, PAGE).unwrap();
    Book::open(&path).unwrap()
}

#[test]
fn layout_keeps_structure_styles_and_links() {
    let temp = tempdir().unwrap();
    let book = book(temp.path());
    assert_eq!(book.sections.len(), 2);
    let blocks = &book.sections[0].blocks;
    let kinds: Vec<&BlockKind> = blocks.iter().map(|block| &block.kind).collect();
    assert_eq!(kinds[0], &BlockKind::Heading(1));
    assert_eq!(kinds[1], &BlockKind::Paragraph);
    assert_eq!(
        kinds[2],
        &BlockKind::ListItem {
            marker: "\u{2022}".into()
        }
    );
    assert_eq!(
        kinds[4],
        &BlockKind::ListItem {
            marker: "1.".into()
        }
    );
    assert!(blocks[5].quoted);
    assert_eq!(kinds[6], &BlockKind::TableRow);
    assert_eq!(blocks[6].text, "a\tb");
    assert_eq!(kinds[7], &BlockKind::Preformatted);
    assert_eq!(blocks[7].text, "fn  main()\n  body");
    assert_eq!(kinds[8], &BlockKind::Rule);

    let paragraph = &blocks[1];
    assert_eq!(paragraph.text, "Plain bold both and a link.");
    let styled = |style: &Style| {
        paragraph
            .spans
            .iter()
            .find(|span| &span.style == style)
            .map(|span| &paragraph.text[span.start..span.end])
    };
    assert_eq!(styled(&Style::Bold), Some("bold both"));
    assert_eq!(styled(&Style::Italic), Some("both"));
    assert_eq!(styled(&Style::Link("#two".into())), Some("link"));
    let target = book.resolve_link("#two").unwrap();
    assert_eq!((target.section, target.block), (1, 0));
    assert!(book.resolve_link("https://example.com/").is_none());

    let toc: Vec<(&str, u8)> = book
        .toc
        .iter()
        .map(|entry| (entry.title.as_str(), entry.level))
        .collect();
    assert_eq!(toc, [("One", 1), ("Two", 2), ("Deeper", 3)]);
}

#[test]
fn search_progress_and_relocation() {
    let temp = tempdir().unwrap();
    let book = book(temp.path());
    let hits = book.search("needle", 0);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].start.section, 1);
    assert!(hits[0].snippet.contains("with [Needle] and"));
    assert_eq!(book.search("needle", 1).len(), 1);
    assert!(book.search("  ", 0).is_empty());

    let start = Locator::default();
    assert_eq!(book.fraction(start), 0.0);
    let place = hits[0].start;
    let fraction = book.fraction(place);
    assert!(fraction > 0.5 && fraction < 1.0);
    let found = book.locate(fraction);
    assert_eq!((found.section, found.block), (place.section, place.block));
    assert_eq!(book.toc[book.toc_index(place).unwrap()].title, "Two");

    // A passage is found again when it is no longer where it was saved.
    let moved = Locator {
        section: 0,
        block: 2,
        offset: 3,
    };
    assert_eq!(book.relocate("Needle", moved), Some(place));
    assert_eq!(book.relocate("not in the book", moved), None);
}

#[test]
fn positions_and_annotations_live_beside_the_library() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let path = temp.path().join("reader.html");
    fs::write(&path, PAGE).unwrap();
    let id = library.add_book(&path).unwrap().id;
    let stored = library.format_path(id, "HTML").unwrap().unwrap();
    let before = fs::read(&stored).unwrap();
    let book = library.open_book(id, None).unwrap();

    assert!(library.reading_position(id, &book).unwrap().is_none());
    assert!(library.annotations(Some(id)).unwrap().is_empty());
    let hit = book.search("Needle", 1).remove(0);
    library.set_reading_position(id, &book, hit.start).unwrap();
    let position = library.reading_position(id, &book).unwrap().unwrap();
    assert_eq!(position.locator, hit.start);

    let highlight = library
        .add_annotation(id, &book, hit.start, Some(hit.end), "why")
        .unwrap();
    assert_eq!(highlight.kind, AnnotationKind::Highlight);
    assert_eq!(highlight.text, "Needle");
    assert_eq!(highlight.chapter, "Two");
    let bookmark = library
        .add_annotation(id, &book, Locator::default(), None, "")
        .unwrap();
    assert_eq!(bookmark.kind, AnnotationKind::Bookmark);
    assert_eq!(bookmark.text, "One");
    assert!(library
        .add_annotation(id, &book, hit.start, Some(hit.start.offset), "")
        .is_err());

    // In reading order, searchable, exportable.
    let all = library.annotations(Some(id)).unwrap();
    assert_eq!(
        all.iter().map(|a| a.id).collect::<Vec<_>>(),
        [bookmark.id, highlight.id]
    );
    assert_eq!(library.search_annotations("WHY").unwrap().len(), 1);
    library
        .set_annotation_note(highlight.id, "because")
        .unwrap();
    let markdown = library
        .export_annotations(None, AnnotationFormat::Markdown)
        .unwrap();
    assert!(markdown.contains("> Needle"));
    assert!(markdown.contains("Note: because"));
    let json = library
        .export_annotations(Some(id), AnnotationFormat::Json)
        .unwrap();
    assert!(json.contains("\"type\": \"highlight\""));

    // The book changed: the highlight follows its text.
    let edited = PAGE.replace(
        "<h2 id=\"two\">",
        "<p>New opening paragraph.</p><h2 id=\"two\">",
    );
    fs::write(
        &stored,
        edited.replace("Second chapter text with", "Now with"),
    )
    .unwrap();
    let changed = library.open_book(id, None).unwrap();
    assert_eq!(library.relocate_annotations(id, &changed).unwrap(), 0);
    let moved = &library.annotations(Some(id)).unwrap()[1];
    let block = changed.block(moved.start).unwrap();
    assert_eq!(&block.text[moved.start.offset..moved.end], "Needle");
    fs::write(&stored, &before).unwrap();

    library.remove_annotation(bookmark.id).unwrap();
    assert!(library.remove_annotation(bookmark.id).is_err());
    assert_eq!(library.annotations(None).unwrap().len(), 1);
    // Reading left the book and calibre's database alone.
    assert_eq!(fs::read(&stored).unwrap(), before);
    assert!(library.calibre_annotations(id).unwrap().is_empty());
}

#[test]
fn calibre_highlights_and_positions_are_read() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let mut library = Library::create(&root).unwrap();
    let path = temp.path().join("reader.html");
    fs::write(&path, PAGE).unwrap();
    let id = library.add_book(&path).unwrap().id;
    drop(library);
    let conn = rusqlite::Connection::open(root.join("metadata.db")).unwrap();
    conn.execute(
        "INSERT INTO annotations (book, format, user_type, user, timestamp, annot_id, annot_type, annot_data, searchable_text)
         VALUES (?1, 'EPUB', 'local', 'viewer', 1700000000.0, 'abc', 'highlight', ?2, '')",
        rusqlite::params![
            id,
            r#"{"type":"highlight","highlighted_text":"Quoted words.","notes":"from calibre","spine_index":0}"#
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO last_read_positions (book, format, user, device, cfi, epoch, pos_frac)
         VALUES (?1, 'EPUB', 'viewer', 'local', 'epubcfi(/2)', 1700000000.0, 0.9)",
        [id],
    )
    .unwrap();
    drop(conn);

    let library = Library::open(&root).unwrap();
    let book = library.open_book(id, None).unwrap();
    let theirs = library.calibre_annotations(id).unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].note, "from calibre");
    let position = library.reading_position(id, &book).unwrap().unwrap();
    assert_eq!(position.locator.section, 1);

    assert_eq!(
        library.import_calibre_annotations(id, &book).unwrap(),
        (1, 0)
    );
    assert_eq!(
        library.import_calibre_annotations(id, &book).unwrap(),
        (0, 0)
    );
    let ours = library.annotations(Some(id)).unwrap();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].text, "Quoted words.");
    assert_eq!(ours[0].note, "from calibre");
    // calibre's rows are untouched.
    assert_eq!(library.calibre_annotations(id).unwrap().len(), 1);
}
