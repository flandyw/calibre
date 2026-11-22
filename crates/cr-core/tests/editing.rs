//! Editing unpacked EPUBs and saving them back.

use std::fs;
use std::path::{Path, PathBuf};

use cr_core::formats::editing::{self, ChangeKind, EpubDir, SearchOptions, Severity};
use cr_core::formats::{convert, epub};
use cr_core::library::{Library, SaveOutcome};
use tempfile::tempdir;

const PAGE: &str = r##"<html><head><title>Edited</title></head><body>
<h1>One</h1><p>First <a href="#two">link</a>. <img src="pic.png" alt=""/></p>
<h2>Inner</h2><p>Colour and colour.</p>
<h1 id="two">Two</h1><p>Second.</p>
</body></html>"##;

fn make_epub(directory: &Path) -> PathBuf {
    let image = image::RgbImage::from_fn(600, 400, |x, y| image::Rgb([x as u8, y as u8, 90]));
    image.save(directory.join("pic.png")).unwrap();
    let html = directory.join("edited.html");
    fs::write(&html, PAGE).unwrap();
    let output = directory.join("edited.epub");
    convert(&html, &output).unwrap();
    output
}

fn unpacked(directory: &Path) -> EpubDir {
    let root = directory.join("unpacked");
    epub::unpack(&make_epub(directory), &root).unwrap();
    EpubDir::open(&root).unwrap()
}

fn errors(book: &EpubDir) -> Vec<String> {
    book.check()
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .map(|issue| format!("{}: {}", issue.file, issue.message))
        .collect()
}

#[test]
fn a_converted_book_is_clean_and_problems_are_found_and_fixed() {
    let temp = tempdir().unwrap();
    let mut book = unpacked(temp.path());
    assert_eq!(book.check(), []);

    let first = book.spine()[0].path.clone();
    let image = book
        .items()
        .into_iter()
        .find(|item| item.media_type == "image/png")
        .unwrap();
    // Break it: stray file, missing file, bad markup, dead link.
    fs::write(book.root().join("stray.css"), "p { color: red }").unwrap();
    fs::remove_file(book.root().join(&image.path)).unwrap();
    fs::remove_file(book.root().join("mimetype")).unwrap();
    let path = book.root().join(&first);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        text.replace("</body>", "<p><a href=\"gone.xhtml\">x</a><b></p></body>"),
    )
    .unwrap();

    let issues = book.check();
    let has = |needle: &str| issues.iter().any(|issue| issue.message.contains(needle));
    assert!(has("is missing"));
    assert!(has("the file is missing"));
    assert!(has("not well-formed XML"));
    assert!(has("gone.xhtml points to a file that does not exist"));
    assert!(issues
        .iter()
        .any(|issue| issue.file == "stray.css" && issue.fixable));
    let malformed = issues
        .iter()
        .find(|issue| issue.message.contains("well-formed"))
        .unwrap();
    assert_eq!(malformed.file, first);
    assert!(malformed.line.is_some());

    let done = book.fix().unwrap();
    assert_eq!(done.len(), 3);
    let after = book.check();
    assert!(after.iter().all(|issue| !issue.fixable));
    assert!(book.items().iter().any(|item| item.path == "stray.css"));
    assert!(!book.items().iter().any(|item| item.path == image.path));
    // What needs a person is still reported.
    assert!(after
        .iter()
        .any(|issue| issue.message.contains("well-formed")));
}

#[test]
fn search_replace_rename_and_reorder() {
    let temp = tempdir().unwrap();
    let mut book = unpacked(temp.path());
    let options = SearchOptions::default();
    let found = book.search("colour", &options).unwrap();
    assert_eq!(found.len(), 2);
    assert!(found[0].text.contains("Colour and colour"));
    let exact = SearchOptions {
        case_sensitive: true,
        ..SearchOptions::default()
    };
    assert_eq!(book.search("colour", &exact).unwrap().len(), 1);

    let changed = book.replace("colour", "color", &options).unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].1, 2);
    let pattern = SearchOptions {
        regex: true,
        ..SearchOptions::default()
    };
    book.replace(r"<p>(Second)\.</p>", "<p><em>$1</em>!</p>", &pattern)
        .unwrap();
    assert_eq!(book.search("<em>Second</em>!", &options).unwrap().len(), 1);
    assert!(book.replace("(", "x", &pattern).is_err());

    // Renaming an image and a chapter keeps every reference working.
    let image = book
        .items()
        .into_iter()
        .find(|item| item.media_type == "image/png")
        .unwrap();
    let moved = "art/renamed.png";
    assert!(book.rename_file(&image.path, moved).unwrap() >= 1);
    let second = book.spine()[1].path.clone();
    let renamed = second.replace(".xhtml", "-two.xhtml");
    assert!(book.rename_file(&second, &renamed).unwrap() >= 1);
    assert_eq!(errors(&book), Vec::<String>::new());
    assert!(book.check().is_empty());
    assert!(book.rename_file("missing.xhtml", "x.xhtml").is_err());
    assert!(book.rename_file(&renamed, "../escape.xhtml").is_err());

    // Reading order.
    let order: Vec<String> = book.spine().into_iter().map(|item| item.path).collect();
    book.move_in_spine(&order[1], 0).unwrap();
    let reordered: Vec<String> = book.spine().into_iter().map(|item| item.path).collect();
    assert_eq!(reordered[..2], [order[1].clone(), order[0].clone()]);

    // Add and remove.
    let extra = temp.path().join("extra.xhtml");
    fs::write(
        &extra,
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>x</title></head><body><h1>Extra</h1></body></html>",
    )
    .unwrap();
    let destination = format!("{}/extra.xhtml", order[0].rsplit_once('/').unwrap().0);
    book.add_file(&extra, &destination).unwrap();
    assert_eq!(book.spine().last().unwrap().path, destination);
    assert!(book.add_file(&extra, &destination).is_err());
    assert!(book.check().is_empty());
    book.remove_file(&destination).unwrap();
    assert_eq!(book.spine().len(), 3);
    assert!(book.check().is_empty());

    // The result is still a readable EPUB.
    let output = temp.path().join("out.epub");
    editing::save(book.root(), &output).unwrap();
    let (metadata, _) = epub::metadata(&output).unwrap();
    assert_eq!(metadata.title, "Edited");
}

#[test]
fn table_of_contents_is_read_and_rebuilt() {
    let temp = tempdir().unwrap();
    let mut book = unpacked(temp.path());
    let before: Vec<String> = book.toc().unwrap().into_iter().map(|e| e.title).collect();
    assert_eq!(before, ["One", "Inner", "Two"]);

    assert_eq!(book.generate_toc(2).unwrap(), 3);
    let toc = book.toc().unwrap();
    let shape: Vec<(&str, u8)> = toc.iter().map(|e| (e.title.as_str(), e.level)).collect();
    assert_eq!(shape, [("One", 1), ("Inner", 2), ("Two", 1)]);
    assert!(toc[1].target.contains('#'));
    assert!(book.check().is_empty());
    assert_eq!(book.generate_toc(1).unwrap(), 2);

    // The reader sees the rebuilt contents.
    let output = temp.path().join("toc.epub");
    book.generate_toc(2).unwrap();
    editing::save(book.root(), &output).unwrap();
    let read = cr_core::formats::reading::Book::open(&output).unwrap();
    let titles: Vec<(&str, u8)> = read
        .toc
        .iter()
        .map(|e| (e.title.as_str(), e.level))
        .collect();
    assert_eq!(titles, shape);
}

#[test]
fn images_compress_and_versions_compare() {
    let temp = tempdir().unwrap();
    let original = make_epub(temp.path());
    let root = temp.path().join("unpacked");
    epub::unpack(&original, &root).unwrap();
    let book = EpubDir::open(&root).unwrap();
    let saved = book.compress_images(80, 200).unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].1 > 0);
    assert!(book.compress_images(80, 200).unwrap().is_empty());

    let output = temp.path().join("small.epub");
    editing::save(&root, &output).unwrap();
    let changes = editing::compare(&original, &output).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Modified);
    assert!(editing::compare(&original, &original).unwrap().is_empty());
}

#[test]
fn library_saves_keep_the_original_and_refuse_broken_books() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let id = library.add_book(&make_epub(temp.path())).unwrap().id;
    let root = temp.path().join("work");
    library.unpack_book(id, "EPUB", &root).unwrap();

    // Nothing edited: nothing written, no backup made.
    assert_eq!(
        library.save_edited_book(id, &root, false).unwrap(),
        SaveOutcome::Unchanged
    );
    assert_eq!(library.formats(id).unwrap(), ["EPUB"]);

    let mut book = EpubDir::open(&root).unwrap();
    book.replace("Second", "Changed", &SearchOptions::default())
        .unwrap();
    let outcome = library.save_edited_book(id, &root, false).unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved { ref changes } if changes.len() == 1));
    let mut formats = library.formats(id).unwrap();
    formats.sort();
    assert_eq!(formats, ["EPUB", "ORIGINAL_EPUB"]);
    assert_eq!(library.compare_with_original(id, "EPUB").unwrap().len(), 1);

    // A broken book is refused and the library copy stays as it was.
    let current = library.format_path(id, "EPUB").unwrap().unwrap();
    let saved = fs::read(&current).unwrap();
    let first = book.spine()[0].path.clone();
    fs::remove_file(root.join(&first)).unwrap();
    let outcome = library.save_edited_book(id, &root, false).unwrap();
    assert!(matches!(outcome, SaveOutcome::Invalid { ref errors } if !errors.is_empty()));
    assert_eq!(fs::read(&current).unwrap(), saved);

    library.restore_original(id, "EPUB").unwrap();
    assert_eq!(library.formats(id).unwrap(), ["EPUB"]);
    let text = library.read_book(id, None).unwrap();
    assert!(text.chapters.iter().any(|c| c.text.contains("Second")));
    assert!(library.restore_original(id, "EPUB").is_err());
}
