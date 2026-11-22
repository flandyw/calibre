//! Sending to, listing and removing from mounted devices.

use std::fs;
use std::path::{Path, PathBuf};

use cr_core::device::{self, Device, DeviceBookState, DeviceKind, SendOptions};
use cr_core::formats::convert;
use cr_core::library::Library;
use tempfile::tempdir;

fn make_epub(directory: &Path, title: &str) -> PathBuf {
    let html = directory.join(format!("{title}.html"));
    fs::write(
        &html,
        format!("<html><head><title>{title}</title></head><body><h1>One</h1><p>Text of {title}.</p></body></html>"),
    )
    .unwrap();
    let output = directory.join(format!("{title}.epub"));
    convert(&html, &output).unwrap();
    output
}

fn kindle(directory: &Path) -> Device {
    let root = directory.join("Kindle");
    fs::create_dir_all(root.join("system")).unwrap();
    fs::create_dir_all(root.join("documents")).unwrap();
    Device::detect_at(&root).unwrap()
}

fn generic(directory: &Path) -> Device {
    let root = directory.join("Reader");
    fs::create_dir_all(root.join("documents")).unwrap();
    Device::detect_at(&root).unwrap()
}

#[test]
fn kindle_gets_a_converted_copy_and_inventory_matches_it() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let id = library
        .add_book(&make_epub(temp.path(), "First"))
        .unwrap()
        .id;
    let device = kindle(temp.path());
    assert_eq!(device.kind, DeviceKind::Kindle);

    // Declining conversion skips the book and writes nothing.
    let declined = SendOptions {
        no_convert: true,
        ..SendOptions::default()
    };
    let report = device::send(&library, &device, &[id], &declined).unwrap();
    assert!(report.sent.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(device::books(Some(&library), &device).unwrap().is_empty());

    // A format the Kindle cannot read is refused, not converted to.
    let epub = SendOptions {
        format: Some("EPUB".into()),
        ..SendOptions::default()
    };
    assert!(device::send(&library, &device, &[id], &epub)
        .unwrap()
        .sent
        .is_empty());

    let report = device::send(&library, &device, &[id], &SendOptions::default()).unwrap();
    assert_eq!(report.sent.len(), 1);
    assert_eq!(report.sent[0].format, "AZW3");
    assert_eq!(report.sent[0].converted_from.as_deref(), Some("EPUB"));
    // The copy on the device reads back as the book.
    let document = cr_core::formats::read_document(&report.sent[0].path, false).unwrap();
    assert_eq!(document.metadata.title, "First");
    // The library itself was not given the converted format.
    assert_eq!(library.formats(id).unwrap(), ["EPUB"]);
    // No partial files are left behind.
    let leftovers: Vec<_> = fs::read_dir(device.documents_dir())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty());

    let books = device::books(Some(&library), &device).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].book_id, Some(id));
    assert_eq!(books[0].state, DeviceBookState::OtherFormat);
    assert_eq!(books[0].format, "AZW3");
    assert!(books[0].path.starts_with("documents/"));

    // Removing takes the Kindle's sidecar folder and the catalog entry too.
    let sidecar = device.root.join(&books[0].path).with_extension("sdr");
    fs::create_dir_all(&sidecar).unwrap();
    fs::write(sidecar.join("position"), "x").unwrap();
    assert!(device::remove(&device, &["documents/nope.azw3".to_string()]).is_err());
    assert!(device::remove(&device, &["../library/metadata.db".to_string()]).is_err());
    device::remove(&device, &[books[0].path.clone()]).unwrap();
    assert!(!sidecar.exists());
    assert!(device::books(Some(&library), &device).unwrap().is_empty());
    let catalog = fs::read_to_string(device.catalog_path()).unwrap();
    assert!(!catalog.contains("First"));
}

#[test]
fn templates_changed_books_and_strangers() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let first = library
        .add_book(&make_epub(temp.path(), "First"))
        .unwrap()
        .id;
    let second = library
        .add_book(&make_epub(temp.path(), "Second"))
        .unwrap()
        .id;
    let device = generic(temp.path());

    let options = SendOptions {
        template: Some("{author}/../{title} [{id}]".into()),
        ..SendOptions::default()
    };
    let report = device::send(&library, &device, &[first, second], &options).unwrap();
    assert_eq!(report.sent.len(), 2);
    assert!(report.sent.iter().all(|sent| sent.converted_from.is_none()));
    // Folders come from the template and never leave the device.
    for sent in &report.sent {
        assert!(sent.path.starts_with(device.documents_dir()));
        assert!(sent.path.is_file());
    }
    assert!(report.sent[0]
        .path
        .to_string_lossy()
        .ends_with(&format!("First [{first}].epub")));

    // A file the library does not know, and a hidden one.
    fs::write(device.documents_dir().join("Stranger.epub"), "x").unwrap();
    fs::write(device.documents_dir().join(".hidden.epub"), "x").unwrap();
    // The library copy of the second book changes after it was sent.
    let replacement = make_epub(temp.path(), "Second edition with more text");
    library
        .replace_format(second, "EPUB", &replacement)
        .unwrap();

    let books = device::books(Some(&library), &device).unwrap();
    assert_eq!(books.len(), 3);
    let state = |id: Option<i64>| books.iter().find(|book| book.book_id == id).unwrap().state;
    assert_eq!(state(Some(first)), DeviceBookState::InSync);
    assert_eq!(state(Some(second)), DeviceBookState::Changed);
    assert_eq!(state(None), DeviceBookState::NotInLibrary);
    // Without a library everything is simply listed.
    assert_eq!(device::books(None, &device).unwrap().len(), 3);

    // Removing a templated book also removes the folder made for it.
    let path = books
        .iter()
        .find(|book| book.book_id == Some(first))
        .unwrap()
        .path
        .clone();
    let folder = device.root.join(&path).parent().unwrap().to_path_buf();
    device::remove(&device, &[path]).unwrap();
    let remaining = device::books(Some(&library), &device).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(
        folder == device.documents_dir()
            || !folder.exists()
            || fs::read_dir(&folder).unwrap().count() > 0
    );
    assert!(device.documents_dir().is_dir());
}

#[test]
fn a_vanished_device_is_reported_and_dry_runs_write_nothing() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let id = library
        .add_book(&make_epub(temp.path(), "First"))
        .unwrap()
        .id;
    let device = generic(temp.path());

    let dry = SendOptions {
        dry_run: true,
        ..SendOptions::default()
    };
    let report = device::send(&library, &device, &[id], &dry).unwrap();
    assert_eq!(report.sent.len(), 1);
    assert!(!report.sent[0].path.exists());
    assert!(!device.catalog_path().exists());

    fs::remove_dir_all(&device.root).unwrap();
    assert!(device::books(Some(&library), &device).is_err());
    assert!(device::remove(&device, &["documents/x.epub".to_string()]).is_err());
}

#[test]
fn kobo_progress_and_kindle_clippings_are_read_without_writing() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("KOBOeReader");
    fs::create_dir_all(root.join(".kobo")).unwrap();
    let database = root.join(".kobo/KoboReader.sqlite");
    {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE content (ContentID TEXT, ContentType TEXT, Title TEXT, Attribution TEXT,
                     ___PercentRead INTEGER, ReadStatus INTEGER, DateLastRead TEXT);
                 CREATE TABLE Bookmark (VolumeID TEXT, Text TEXT, Annotation TEXT, DateCreated TEXT);
                 INSERT INTO content VALUES ('file:///mnt/onboard/a.epub', '6', 'Alpha', 'Ann', 42, 1, '2026-03-01T10:00:00Z');
                 INSERT INTO content VALUES ('file:///mnt/onboard/b.epub', '6', 'Beta', '', 0, 2, NULL);
                 INSERT INTO content VALUES ('file:///mnt/onboard/c.epub', '6', 'Unopened', 'Cy', 0, 0, NULL);
                 INSERT INTO content VALUES ('file:///mnt/onboard/a.epub!ch1', '9', 'Chapter', NULL, 0, 0, NULL);
                 INSERT INTO Bookmark VALUES ('file:///mnt/onboard/a.epub', ' A highlighted line ', 'my note', '2026-03-01T10:05:00Z');
                 INSERT INTO Bookmark VALUES ('file:///mnt/onboard/a.epub', NULL, NULL, '2026-03-01T10:06:00Z');",
            )
            .unwrap();
    }
    let before = fs::read(&database).unwrap();
    let kobo = Device::detect_at(&root).unwrap();
    assert_eq!(kobo.kind, DeviceKind::Kobo);
    let reading = device::reading(&kobo).unwrap();
    assert_eq!(reading.len(), 2);
    assert_eq!(reading[0].title, "Alpha");
    assert_eq!(reading[0].percent, Some(42.0));
    assert_eq!(reading[0].annotations.len(), 1);
    assert_eq!(reading[0].annotations[0].text, "A highlighted line");
    assert_eq!(reading[0].annotations[0].note.as_deref(), Some("my note"));
    assert!(reading[1].finished);
    assert_eq!(reading[1].author, None);
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(fs::read_dir(root.join(".kobo")).unwrap().count(), 1);

    let device = kindle(temp.path());
    assert!(device::reading(&device).unwrap().is_empty());
    fs::write(
        device.documents_dir().join("My Clippings.txt"),
        "Dune (Frank Herbert)\r\n- Your Highlight on page 12 | Added on Monday\r\n\r\nFear is the mind-killer.\r\n==========\r\n",
    )
    .unwrap();
    let reading = device::reading(&device).unwrap();
    assert_eq!(reading.len(), 1);
    assert_eq!(reading[0].annotations[0].text, "Fear is the mind-killer.");

    assert!(device::reading(&generic(temp.path())).is_err());
}
