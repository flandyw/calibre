//! Everyday management: duplicates, formats, trash, transfers, bulk edits.

use std::fs;
use std::path::{Path, PathBuf};

use cr_core::jobs::JobContext;
use cr_core::library::{
    scan_sources, AddOutcome, BulkEdit, Collision, DuplicatePolicy, ExportOptions, ImportOptions,
    Library, SearchReplace, TextField,
};
use cr_core::Metadata;
use tempfile::tempdir;

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

fn html(dir: &Path, name: &str, title: &str) -> PathBuf {
    let body = format!("<html><head><title>{title}</title></head><body><p>Text</p></body></html>");
    write(dir, name, &body)
}

fn policy(duplicates: DuplicatePolicy) -> ImportOptions {
    ImportOptions {
        duplicates,
        ..Default::default()
    }
}

#[test]
fn two_formats_of_one_book_merge_into_one_record() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let txt = write(temp.path(), "Dune.txt", "Chapter 1\n\nSpice.\n");
    let page = html(temp.path(), "other.html", "The Dune!");

    let merge = policy(DuplicatePolicy::Merge);
    let first = library.import_book(&txt, &merge).unwrap();
    let id = first.book_id().unwrap();
    assert!(matches!(first, AddOutcome::Added(_)));

    let second = library.import_book(&page, &merge).unwrap();
    assert_eq!(
        second,
        AddOutcome::Merged {
            id,
            format: "HTML".into(),
            added: true
        }
    );
    assert_eq!(library.formats(id).unwrap(), ["HTML", "TXT"]);
    assert_eq!(library.all_book_ids().unwrap(), [id]);

    // The same format again changes nothing; skipping reports the match.
    let again = library.import_book(&txt, &merge).unwrap();
    assert!(matches!(again, AddOutcome::Merged { added: false, .. }));
    let skipped = library
        .import_book(&txt, &policy(DuplicatePolicy::Skip))
        .unwrap();
    assert_eq!(skipped, AddOutcome::Skipped { existing: vec![id] });
    assert_eq!(library.all_book_ids().unwrap().len(), 1);

    library.remove_format(id, "html").unwrap();
    assert_eq!(library.formats(id).unwrap(), ["TXT"]);
    library.restore_format(id, "HTML").unwrap();
    assert_eq!(library.formats(id).unwrap(), ["HTML", "TXT"]);
    assert!(library.format_path(id, "HTML").unwrap().unwrap().is_file());
}

#[test]
fn scans_folders_and_reads_metadata_from_file_names() {
    let temp = tempdir().unwrap();
    let inbox = temp.path().join("inbox");
    write(&inbox, "Frank Herbert - Dune.txt", "Spice.\n");
    write(
        &inbox,
        "nested/deep/Stanislaw Lem - Solaris.txt",
        "Ocean.\n",
    );
    write(&inbox, "notes.log", "ignore me");
    write(&inbox, ".hidden/Secret.txt", "ignore me");

    assert_eq!(scan_sources(&[inbox.clone()], false).len(), 1);
    let sources = scan_sources(&[inbox], true);
    assert_eq!(sources.len(), 2);

    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let options = ImportOptions {
        filename_pattern: Some(regex::Regex::new(r"^(?P<author>.+?) - (?P<title>.+)$").unwrap()),
        ..Default::default()
    };
    let results = library.import_books(&sources, &options, &JobContext::detached());
    let mut books: Vec<(String, Vec<String>)> = results
        .iter()
        .map(|item| {
            let id = item.result.as_ref().unwrap().book_id().unwrap();
            let meta = library.get_metadata(id).unwrap();
            (meta.title, meta.authors)
        })
        .collect();
    books.sort();
    assert_eq!(
        books,
        [
            ("Dune".to_string(), vec!["Frank Herbert".to_string()]),
            ("Solaris".to_string(), vec!["Stanislaw Lem".to_string()]),
        ]
    );
}

#[test]
fn trash_restores_a_book_with_its_id_formats_and_extra_files() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("library");
    let mut library = Library::create(&root).unwrap();
    let txt = write(temp.path(), "Dune.txt", "Spice.\n");
    let id = library.add_book(&txt).unwrap().id;
    library
        .add_format(id, &html(temp.path(), "d.html", "Dune"), false)
        .unwrap();
    let notes = write(temp.path(), "notes.md", "# Notes");
    assert_eq!(library.add_extra_file(id, &notes).unwrap(), "notes.md");
    let before = library.get_metadata(id).unwrap();

    library.trash_book(id).unwrap();
    assert!(library.all_book_ids().unwrap().is_empty());
    assert!(root.join(".caltrash/b").join(id.to_string()).is_dir());
    let trash = library.list_trash().unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!((trash[0].id, trash[0].is_book), (id, true));
    assert_eq!(trash[0].title, "Dune");
    assert_eq!(trash[0].formats, ["HTML", "TXT"]);

    let restored = library.restore_book(id).unwrap();
    assert_eq!(restored.id, id);
    let after = library.get_metadata(id).unwrap();
    assert_eq!(after.uuid, before.uuid);
    assert_eq!(after.title, before.title);
    assert_eq!(library.formats(id).unwrap(), ["HTML", "TXT"]);
    assert_eq!(library.extra_files(id).unwrap(), ["notes.md"]);
    assert!(library.list_trash().unwrap().is_empty());

    // A removed format and the removed book are separate trash entries.
    library.remove_format(id, "HTML").unwrap();
    library.trash_book(id).unwrap();
    let kinds = |library: &Library| -> Vec<bool> {
        let trash = library.list_trash().unwrap();
        trash.iter().map(|entry| entry.is_book).collect()
    };
    assert_eq!(kinds(&library).len(), 2);
    library.purge_trash_entry(id, false).unwrap();
    assert_eq!(kinds(&library), [true]);
    assert!(library.purge_trash_entry(id, false).is_err());
    library.purge_trash_entry(id, true).unwrap();
    assert!(library.list_trash().unwrap().is_empty());

    let id = library.add_book(&txt).unwrap().id;
    library.trash_book(id).unwrap();
    assert_eq!(library.purge_trash(None).unwrap(), 1);
    assert!(library.restore_book(id).is_err());
}

#[test]
fn bulk_edit_changes_tags_series_and_text() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let ids: Vec<i64> = ["Dune.txt", "Dune Messiah.txt", "Children of Dune.txt"]
        .iter()
        .map(|name| {
            let path = write(temp.path(), name, "Spice.\n");
            library.add_book(&path).unwrap().id
        })
        .collect();
    let mut seed = BulkEdit::default();
    seed.add_tags = vec!["old".into(), "sf".into()];
    library.bulk_update(&ids, &seed, &JobContext::detached());

    let edit = BulkEdit {
        authors: Some(vec!["Frank Herbert".into()]),
        series: Some(Some("Dune".into())),
        series_start: Some(1.0),
        remove_tags: vec!["OLD".into()],
        add_tags: vec!["classic".into(), "SF".into()],
        replace: Some(SearchReplace {
            field: TextField::Title,
            pattern: "dune".into(),
            replacement: "Arrakis".into(),
            regex: false,
            ignore_case: true,
        }),
        ..Default::default()
    };
    let preview = library.bulk_patch(ids[1], 1, &edit).unwrap();
    assert_eq!(preview.title.as_deref(), Some("Arrakis Messiah"));
    assert_eq!(preview.series_index, Some(2.0));

    let results = library.bulk_update(&ids, &edit, &JobContext::detached());
    assert!(results.iter().all(|item| *item.result.as_ref().unwrap()));
    let last = library.get_metadata(ids[2]).unwrap();
    assert_eq!(last.title, "Children of Arrakis");
    assert_eq!(last.authors, ["Frank Herbert"]);
    assert_eq!(last.series.as_deref(), Some("Dune"));
    assert_eq!(last.series_index, 3.0);
    assert_eq!(last.tags, ["sf", "classic"]);

    // Applying it again finds nothing to change except the renumbering.
    let unchanged = BulkEdit {
        add_tags: vec!["classic".into()],
        ..Default::default()
    };
    let results = library.bulk_update(&ids, &unchanged, &JobContext::detached());
    assert!(results.iter().all(|item| !*item.result.as_ref().unwrap()));
}

#[test]
fn books_move_between_libraries_with_everything_attached() {
    let temp = tempdir().unwrap();
    let mut source = Library::create(&temp.path().join("a")).unwrap();
    let mut target = Library::create(&temp.path().join("b")).unwrap();
    let id = source
        .add_book(&write(temp.path(), "Dune.txt", "Spice.\n"))
        .unwrap()
        .id;
    source
        .add_format(id, &html(temp.path(), "d.html", "Dune"), false)
        .unwrap();
    source
        .add_extra_file(id, &write(temp.path(), "map.txt", "Arrakis"))
        .unwrap();
    let uuid = source.get_metadata(id).unwrap().uuid;

    let context = JobContext::detached();
    let copied = source.transfer_books(&[id], &mut target, DuplicatePolicy::Skip, false, &context);
    let new_id = copied[0].result.as_ref().unwrap().book_id().unwrap();
    assert_eq!(target.formats(new_id).unwrap(), ["HTML", "TXT"]);
    assert_eq!(target.extra_files(new_id).unwrap(), ["map.txt"]);
    assert_eq!(target.get_metadata(new_id).unwrap().uuid, uuid);
    assert_eq!(source.all_book_ids().unwrap(), [id]);

    // A second copy is recognised; moving a skipped book leaves it in place.
    let again = source.transfer_books(&[id], &mut target, DuplicatePolicy::Skip, true, &context);
    assert!(matches!(
        again[0].result.as_ref().unwrap(),
        AddOutcome::Skipped { .. }
    ));
    assert_eq!(source.all_book_ids().unwrap(), [id]);

    let moved = source.transfer_books(&[id], &mut target, DuplicatePolicy::Add, true, &context);
    assert!(moved[0].result.is_ok());
    assert!(source.all_book_ids().unwrap().is_empty());
    assert_eq!(source.list_trash().unwrap().len(), 1);
    assert_eq!(target.all_book_ids().unwrap().len(), 2);
}

#[test]
fn export_honours_formats_sidecars_and_collisions() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let id = library
        .add_book(&write(temp.path(), "Dune.txt", "Spice.\n"))
        .unwrap()
        .id;
    library
        .add_format(id, &html(temp.path(), "d.html", "Dune"), false)
        .unwrap();
    let out = temp.path().join("out");
    let mut options = ExportOptions {
        template: "{title} ({id})".into(),
        formats: vec!["txt".into()],
        opf: true,
        ..Default::default()
    };
    let names = |paths: Vec<PathBuf>| -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect()
    };
    let first = library.export_book(id, &out, &options).unwrap();
    assert_eq!(names(first), ["Dune (1).txt", "Dune (1).opf"]);

    options.collision = Collision::Skip;
    assert!(library.export_book(id, &out, &options).unwrap().is_empty());
    options.collision = Collision::Rename;
    let renamed = library.export_book(id, &out, &options).unwrap();
    assert_eq!(names(renamed), ["Dune (1) (2).txt", "Dune (1) (2).opf"]);
    options.collision = Collision::Overwrite;
    let replaced = library.export_book(id, &out, &options).unwrap();
    assert_eq!(names(replaced), ["Dune (1).txt", "Dune (1).opf"]);
}

#[test]
fn empty_records_take_formats_later() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let mut meta = Metadata::new("Placeholder");
    meta.authors = vec!["Someone".into()];
    let id = library.add_empty(meta).unwrap().id;
    assert!(library.formats(id).unwrap().is_empty());
    let txt = write(temp.path(), "x.txt", "Body\n");
    assert!(library.add_format(id, &txt, false).unwrap());
    assert!(!library.add_format(id, &txt, false).unwrap());
    assert_eq!(library.formats(id).unwrap(), ["TXT"]);
}

#[test]
fn merged_moves_keep_extra_files_and_refuse_the_same_library() {
    let temp = tempdir().unwrap();
    let mut source = Library::create(&temp.path().join("a")).unwrap();
    let mut target = Library::create(&temp.path().join("b")).unwrap();
    let txt = write(temp.path(), "Dune.txt", "Spice.\n");
    let id = source.add_book(&txt).unwrap().id;
    source
        .add_extra_file(id, &write(temp.path(), "map.txt", "Arrakis"))
        .unwrap();
    let existing = target.add_book(&txt).unwrap().id;

    let context = JobContext::detached();
    let moved = source.transfer_books(&[id], &mut target, DuplicatePolicy::Merge, true, &context);
    assert!(matches!(
        moved[0].result.as_ref().unwrap(),
        AddOutcome::Merged { .. }
    ));
    assert_eq!(target.extra_files(existing).unwrap(), ["map.txt"]);

    // The same library reached through a different path is still the same.
    let alias = temp.path().join("b").join("..").join("b");
    let mut again = Library::open(&alias).unwrap();
    let result = target.copy_book_to(existing, &mut again, DuplicatePolicy::Merge);
    assert!(result.is_err());
}

#[test]
fn restore_keeps_sort_keys_and_scans_start_at_dot_folders() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let mut meta = Metadata::new("The Long Title");
    meta.authors = vec!["Frank Herbert".into(), "Brian Herbert".into()];
    let id = library.add_empty(meta).unwrap().id;
    let before = library.get_metadata(id).unwrap();
    library.trash_book(id).unwrap();
    library.restore_book(id).unwrap();
    let after = library.get_metadata(id).unwrap();
    assert_eq!(after.author_sort, before.author_sort);
    assert_eq!(after.title_sort, before.title_sort);

    let hidden = temp.path().join(".inbox");
    write(&hidden, "Dune.txt", "Spice.\n");
    assert_eq!(scan_sources(&[hidden], true).len(), 1);
}
