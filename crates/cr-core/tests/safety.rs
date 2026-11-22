//! Schema gating, atomic mutations and reader format selection.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use cr_core::jobs::JobContext;
use cr_core::library::{BookUpdate, Fault, Library};
use cr_core::Error;
use tempfile::tempdir;

fn text_book(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, "Chapter 1\n\nSome words here.\n").unwrap();
    path
}

/// Every file under the library except SQLite's own, with sizes.
fn tree(root: &Path) -> BTreeMap<String, u64> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let relative = e
                .path()
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .to_string();
            (!relative.starts_with("metadata.db")).then(|| (relative, e.metadata().unwrap().len()))
        })
        .collect()
}

fn set_user_version(root: &Path, version: i64) {
    let conn = rusqlite::Connection::open(root.join("metadata.db")).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
}

#[test]
fn newer_and_older_schemas_open_read_only() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let source = text_book(temp.path(), "Dune.txt");
    let id = Library::create(&root)
        .unwrap()
        .add_book(&source)
        .unwrap()
        .id;
    let before = tree(&root);

    for (version, hint) in [(29, "Update calibre-rust"), (25, "Open it once in calibre")] {
        set_user_version(&root, version);
        let mut library = Library::open(&root).unwrap();
        assert!(library.read_only_reason().unwrap().contains(hint));
        // Reading still works.
        assert_eq!(library.get_metadata(id).unwrap().title, "Dune");
        assert!(!library.read_book(id, None).unwrap().is_empty());
        // Nothing mutates.
        let patch = BookUpdate {
            title: Some("Changed".into()),
            ..Default::default()
        };
        assert!(matches!(
            library.update_book(id, &patch),
            Err(Error::ReadOnly(_))
        ));
        assert!(matches!(library.add_book(&source), Err(Error::ReadOnly(_))));
        assert!(matches!(
            library.remove_book(id, true),
            Err(Error::ReadOnly(_))
        ));
        assert!(matches!(
            library.replace_format(id, "TXT", &source),
            Err(Error::ReadOnly(_))
        ));
        assert!(matches!(library.sync_all(), Err(Error::ReadOnly(_))));
        assert_eq!(tree(&root), before);
    }

    set_user_version(&root, 28);
    assert!(Library::open(&root).unwrap().read_only_reason().is_none());
}

#[test]
fn rejects_databases_that_are_not_calibre() {
    let temp = tempdir().unwrap();
    let conn = rusqlite::Connection::open(temp.path().join("metadata.db")).unwrap();
    conn.execute_batch("CREATE TABLE books (id INTEGER); PRAGMA user_version=28;")
        .unwrap();
    drop(conn);
    let error = Library::open(temp.path()).err().unwrap().to_string();
    assert!(error.contains("missing the `data` table"), "{error}");

    fs::write(temp.path().join("metadata.db"), b"not sqlite at all").unwrap();
    assert!(matches!(
        Library::open(temp.path()),
        Err(Error::InvalidLibrary(_))
    ));
}

#[test]
fn failed_add_leaves_no_files_or_rows() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let source = text_book(temp.path(), "Dune.txt");
    let mut library = Library::create(&root).unwrap();

    library.inject_fault(Some(Fault::BeforeCommit));
    assert!(library.add_book(&source).is_err());
    assert!(tree(&root).is_empty(), "{:?}", tree(&root));
    assert!(library.all_book_ids().unwrap().is_empty());
    assert!(fs::read_dir(&root)
        .unwrap()
        .all(|e| e.unwrap().file_type().unwrap().is_file()));

    // The same call then succeeds.
    let added = library.add_book(&source).unwrap();
    assert_eq!(tree(&root).len(), 2);
    assert_eq!(library.formats(added.id).unwrap(), vec!["TXT"]);
}

#[test]
fn failed_update_restores_names_cover_and_opf() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let source = text_book(temp.path(), "Dune.txt");
    let mut library = Library::create(&root).unwrap();
    let id = library.add_book(&source).unwrap().id;
    let before = tree(&root);
    let opf_before = fs::read(
        root.join(library.book_path(id).unwrap())
            .join("metadata.opf"),
    )
    .unwrap();

    let jpeg = {
        let mut out = Vec::new();
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        out
    };
    let patch = BookUpdate {
        title: Some("Children of Dune".into()),
        authors: Some(vec!["Someone Else".into()]),
        cover: Some(Some(jpeg)),
        ..Default::default()
    };
    library.inject_fault(Some(Fault::BeforeCommit));
    assert!(library.update_book(id, &patch).is_err());

    assert_eq!(tree(&root), before);
    let directory = root.join(library.book_path(id).unwrap());
    assert_eq!(
        fs::read(directory.join("metadata.opf")).unwrap(),
        opf_before
    );
    assert_eq!(library.get_metadata(id).unwrap().title, "Dune");
    assert!(library.format_path(id, "TXT").unwrap().is_some());
    assert!(!root.join("Someone Else").exists());

    library.update_book(id, &patch).unwrap();
    assert_eq!(library.get_metadata(id).unwrap().title, "Children of Dune");
    assert!(library.format_path(id, "TXT").unwrap().is_some());
    assert!(library.cover(id).unwrap().is_some());
}

#[test]
fn batch_add_reports_each_failure_and_continues() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let sources = vec![
        text_book(temp.path(), "One - A.txt"),
        temp.path().join("missing.txt"),
        text_book(temp.path(), "Two - B.txt"),
    ];
    let results = library.add_books(&sources, &JobContext::detached());
    let ok: Vec<bool> = results.iter().map(|r| r.result.is_ok()).collect();
    assert_eq!(ok, vec![true, false, true]);
    assert_eq!(library.all_book_ids().unwrap().len(), 2);
}

#[test]
fn reading_prefers_a_supported_format() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let pdf = temp.path().join("Paper - Author.pdf");
    fs::write(&pdf, b"%PDF-1.4 not really").unwrap();
    let id = library.add_book(&pdf).unwrap().id;

    let error = library.read_book(id, None).err().unwrap();
    assert!(matches!(error, Error::UnsupportedFormat(_)));
    assert!(error.to_string().contains("only has PDF"), "{error}");

    // LIT sorts before TXT alphabetically; an unreadable-first choice would fail.
    let lit = temp.path().join("paper.lit");
    fs::write(&lit, b"ITOLITLS").unwrap();
    library.replace_format(id, "LIT", &lit).unwrap();
    library
        .replace_format(id, "TXT", &text_book(temp.path(), "paper.txt"))
        .unwrap();
    assert_eq!(library.preferred_readable_format(id).unwrap(), "TXT");
    assert!(!library.read_book(id, None).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn metadata_files_are_not_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let source = text_book(temp.path(), "Dune.txt");
    let mut library = Library::create(&root).unwrap();
    let id = library.add_book(&source).unwrap().id;
    let opf = |library: &Library| {
        root.join(library.book_path(id).unwrap())
            .join("metadata.opf")
    };
    let path = opf(&library);
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_ne!(mode & 0o044, 0, "new OPF is {mode:o}");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    let patch = BookUpdate {
        comments: Some(Some("A note".into())),
        ..Default::default()
    };
    library.update_book(id, &patch).unwrap();
    let mode = fs::metadata(opf(&library)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o664);
}
