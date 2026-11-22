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

#[test]
fn edits_keep_what_they_do_not_own() {
    let temp = tempdir().unwrap();
    let book = unpacked(temp.path());
    let first = book.spine()[0].path.clone();
    let image = book
        .items()
        .into_iter()
        .find(|item| item.media_type == "image/png")
        .unwrap();
    let opf = book.root().join(book.opf_path());

    // Unquoted attributes are not XML.
    let path = book.root().join(&first);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("<p>", "<p class=broken>")).unwrap();
    assert!(book
        .check()
        .iter()
        .any(|issue| issue.message.contains("well-formed") && issue.line.is_some()));
    fs::write(&path, &text).unwrap();

    // A remote resource is not a missing file, and fix leaves it alone.
    let package = fs::read_to_string(&opf).unwrap();
    let remote =
        "<item id=\"remote\" href=\"https://example.com/a.mp3\" media-type=\"audio/mpeg\"/>";
    fs::write(
        &opf,
        package.replace("</manifest>", &format!("{remote}</manifest>")),
    )
    .unwrap();
    let mut book = EpubDir::open(book.root()).unwrap();
    assert_eq!(book.check(), []);
    book.fix().unwrap();
    assert!(fs::read_to_string(&opf).unwrap().contains(remote));

    // Inline styles, quoted imports and guide references follow a rename.
    let image_href = image.path.rsplit_once("OEBPS/").unwrap().1.to_string();
    let styled = text.replace(
        "</body>",
        &format!("<p style=\"background: url({image_href})\">x</p><p>write url({image_href}) in CSS</p></body>"),
    );
    fs::write(&path, styled).unwrap();
    let css = temp.path().join("extra.css");
    fs::write(
        &css,
        format!("@import \"other.css\";\np {{ background: url('{image_href}') }}"),
    )
    .unwrap();
    let other = temp.path().join("other.css");
    fs::write(&other, "p { margin: 0 }").unwrap();
    let directory = first.rsplit_once('/').unwrap().0.to_string();
    book.add_file(&css, &format!("{directory}/extra.css"))
        .unwrap();
    book.add_file(&other, &format!("{directory}/other.css"))
        .unwrap();
    let package = fs::read_to_string(&opf).unwrap();
    let chapter_href = first.rsplit('/').next().unwrap();
    fs::write(
        &opf,
        package.replace(
            "</package>",
            &format!("<guide><reference type=\"text\" title=\"Start\" href=\"{chapter_href}\"/></guide></package>"),
        ),
    )
    .unwrap();
    let mut book = EpubDir::open(book.root()).unwrap();
    book.rename_file(&image.path, &format!("{directory}/art/new.png"))
        .unwrap();
    book.rename_file(
        &format!("{directory}/other.css"),
        &format!("{directory}/css/base.css"),
    )
    .unwrap();
    let moved = format!("{directory}/text/start.xhtml");
    book.rename_file(&first, &moved).unwrap();
    assert!(fs::read_to_string(book.root().join(&moved))
        .unwrap()
        .contains("url(../art/new.png)"));
    let css = fs::read_to_string(book.root().join(format!("{directory}/extra.css"))).unwrap();
    assert!(css.contains("@import \"css/base.css\""));
    assert!(css.contains("url('art/new.png')"));
    let package = fs::read_to_string(&opf).unwrap();
    assert!(
        package.contains("href=\"text/start.xhtml\"/></guide>"),
        "{package}"
    );
    assert_eq!(book.check(), []);

    // Prose that mentions url() is not a reference.
    let chapter = fs::read_to_string(book.root().join(&moved)).unwrap();
    assert!(chapter.contains(&format!("write url({image_href}) in CSS")));

    // Names that need escaping stay well-formed, in either quote style.
    let chapter = chapter.replace(
        "style=\"background: url(../art/new.png)\"",
        "style='background: url(../art/new.png)'",
    );
    assert!(chapter.contains("style='"));
    fs::write(book.root().join(&moved), chapter).unwrap();
    let odd = format!("{directory}/art/a&b's.png");
    book.rename_file(&format!("{directory}/art/new.png"), &odd)
        .unwrap();
    assert_eq!(book.check(), []);
    book.rename_file(&odd, &format!("{directory}/art/new.png"))
        .unwrap();
    assert_eq!(book.check(), []);

    // A new file never takes an id the package already uses.
    let package = fs::read_to_string(&opf).unwrap();
    fs::write(&opf, package.replace("id=\"remote\"", "id=\"more_css\"")).unwrap();
    let mut book = EpubDir::open(book.root()).unwrap();
    book.add_file(
        &temp.path().join("extra.css"),
        &format!("{directory}/more.css"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&opf)
            .unwrap()
            .matches("id=\"more_css\"")
            .count(),
        1
    );
    book.remove_file(&format!("{directory}/more.css")).unwrap();

    // Regenerating the contents keeps the rest of the navigation document.
    if let Some(nav) = book
        .items()
        .into_iter()
        .find(|item| item.properties.contains("nav"))
    {
        let path = book.root().join(&nav.path);
        let text = fs::read_to_string(&path).unwrap();
        let landmarks = "<nav epub:type=\"landmarks\"><ol><li><a epub:type=\"bodymatter\" href=\"text/start.xhtml\">Start</a></li></ol></nav>";
        // The epub prefix is bound on the elements themselves.
        let bound = "<nav xmlns:epub=\"http://www.idpf.org/2007/ops\" ";
        let landmarks = &landmarks.replace("<nav ", bound);
        let text = text
            .replacen(" xmlns:epub=\"http://www.idpf.org/2007/ops\"", "", 1)
            .replacen("<nav ", bound, 1)
            .replace("</body>", &format!("{landmarks}</body>"));
        fs::write(&path, text).unwrap();
        assert_eq!(book.check(), []);
        book.generate_toc(2).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(landmarks));
        assert_eq!(text.matches("epub:type=\"toc\"").count(), 1);
        assert_eq!(book.toc().unwrap().len(), 3);
        assert_eq!(book.check(), []);
    } else {
        panic!("converted books have a navigation document");
    }
}

#[test]
fn prefixed_packages_and_other_encodings_stay_valid() {
    let temp = tempdir().unwrap();
    let mut book = unpacked(temp.path());
    let opf = book.root().join(book.opf_path());
    let package = fs::read_to_string(&opf).unwrap();
    let prefixed =
        regex::Regex::new(r"<(/?)(package|metadata|manifest|spine|item|itemref|guide|reference)\b")
            .unwrap()
            .replace_all(&package, "<${1}opf:$2")
            .replace(
                "xmlns=\"http://www.idpf.org/2007/opf\"",
                "xmlns:opf=\"http://www.idpf.org/2007/opf\"",
            );
    fs::write(&opf, prefixed).unwrap();
    let mut prefixed = EpubDir::open(book.root()).unwrap();
    let extra = temp.path().join("extra.xhtml");
    fs::write(&extra, "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>x</title></head><body><p>x</p></body></html>").unwrap();
    let directory = prefixed.spine()[0]
        .path
        .rsplit_once('/')
        .unwrap()
        .0
        .to_string();
    prefixed
        .add_file(&extra, &format!("{directory}/extra.xhtml"))
        .unwrap();
    let text = fs::read_to_string(&opf).unwrap();
    assert!(text.contains("<opf:item id=\"extra_xhtml\""), "{text}");
    assert!(text.contains("<opf:itemref idref=\"extra_xhtml\""));
    fs::write(&opf, package).unwrap();
    book = EpubDir::open(book.root()).unwrap();

    // A UTF-16 chapter is rewritten as UTF-8 and says so.
    let first = book.spine()[0].path.clone();
    let path = book.root().join(&first);
    let text = fs::read_to_string(&path)
        .unwrap()
        .replacen("utf-8", "utf-16", 1)
        .replacen("UTF-8", "utf-16", 1);
    assert!(text.contains("encoding=\"utf-16\""));
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    fs::write(&path, bytes).unwrap();
    book.replace("First", "Erste", &SearchOptions::default())
        .unwrap();
    let written = fs::read_to_string(&path).unwrap();
    assert!(written.contains("Erste"));
    assert!(written.contains("encoding=\"utf-8\""));
}
