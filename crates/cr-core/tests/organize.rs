//! Search language golden fixture, saved searches, categories, custom columns.

use std::fs;

use cr_core::library::{BookUpdate, CatalogFormat, Category, Library, ListOptions};
use tempfile::tempdir;

fn fixture() -> (tempfile::TempDir, Library) {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let books: [(&str, &str, &[&str], Option<u8>, Option<&str>); 4] = [
        (
            "Dune",
            "Frank Herbert",
            &["sf", "classic"],
            Some(10),
            Some("Dune"),
        ),
        ("Solaris", "Stanislaw Lem", &["sf"], Some(8), None),
        (
            "Emma",
            "Jane Austen",
            &["romance", "classic"],
            Some(6),
            None,
        ),
        (
            "The Cyberiad",
            "Stanislaw Lem",
            &["sf", "humour"],
            None,
            None,
        ),
    ];
    for (title, author, tags, rating, series) in books {
        let path = temp.path().join(format!("{title}.txt"));
        fs::write(&path, "x".repeat(title.len() * 400)).unwrap();
        let id = library.add_book(&path).unwrap().id;
        let patch = BookUpdate {
            authors: Some(vec![author.to_string()]),
            tags: Some(tags.iter().map(|tag| tag.to_string()).collect()),
            rating: Some(rating),
            series: Some(series.map(str::to_string)),
            ..Default::default()
        };
        library.update_book(id, &patch).unwrap();
    }
    (temp, library)
}

fn titles(library: &Library, query: &str) -> Vec<String> {
    let options = ListOptions {
        query: Some(query.to_string()),
        ..Default::default()
    };
    let books = library.list_books(&options).unwrap();
    books.into_iter().map(|book| book.title).collect()
}

#[test]
fn search_language_matches_the_golden_fixture() {
    let (_temp, mut library) = fixture();
    let golden: &[(&str, &[&str])] = &[
        ("", &["The Cyberiad", "Dune", "Emma", "Solaris"]),
        ("lem", &["The Cyberiad", "Solaris"]),
        ("tag:sf tag:classic", &["Dune"]),
        ("tag:romance or tag:humour", &["The Cyberiad", "Emma"]),
        ("tag:classic and not author:austen", &["Dune"]),
        ("(tag:romance or tag:humour) -title:emma", &["The Cyberiad"]),
        ("not (tag:sf or tag:romance)", &[]),
        ("tags:=class", &[]),
        ("tags:=CLASSIC", &["Dune", "Emma"]),
        ("title:~^s", &["Solaris"]),
        ("title:\"the cyb\"", &["The Cyberiad"]),
        ("rating:>=4", &["Dune", "Solaris"]),
        ("rating:3", &["Emma"]),
        ("rating:false", &["The Cyberiad"]),
        ("series:true", &["Dune"]),
        ("series:false author:lem", &["The Cyberiad", "Solaris"]),
        ("format:txt size:>2k", &["The Cyberiad", "Solaris"]),
        ("date:today", &["The Cyberiad", "Dune", "Emma", "Solaris"]),
        ("date:<2000", &[]),
        ("id:>3", &["The Cyberiad"]),
        ("search:favourites", &["Dune", "Solaris"]),
        ("vl:Lem search:favourites", &["Solaris"]),
        ("search:nothing", &[]),
        ("#pages:>400", &["Dune"]),
        ("#genre:=space", &["Dune", "Solaris"]),
        ("#genre:false", &["The Cyberiad", "Emma"]),
        ("#read:true", &["Solaris"]),
        ("#missing:x", &[]),
    ];
    library
        .set_saved_search("Favourites", Some("rating:>=4"))
        .unwrap();
    library
        .set_virtual_library("Lem", Some("author:lem"))
        .unwrap();
    library
        .create_custom_column("pages", "Pages", "int", false, &[])
        .unwrap();
    library
        .create_custom_column("genre", "Genre", "text", true, &[])
        .unwrap();
    library
        .create_custom_column("read", "Read", "bool", false, &[])
        .unwrap();
    library.set_custom_value(1, "pages", "412").unwrap();
    library.set_custom_value(2, "#pages", "204").unwrap();
    library
        .set_custom_value(1, "genre", "space, desert")
        .unwrap();
    library.set_custom_value(2, "genre", "Space").unwrap();
    library.set_custom_value(2, "read", "yes").unwrap();
    library.set_custom_value(1, "read", "no").unwrap();

    for (query, expected) in golden {
        assert_eq!(titles(&library, query), *expected, "query: {query}");
    }
}

#[test]
fn custom_columns_validate_and_round_trip() {
    let (_temp, mut library) = fixture();
    let choices = vec!["Paper".to_string(), "Digital".to_string()];
    library
        .create_custom_column("medium", "Medium", "enumeration", false, &choices)
        .unwrap();
    library
        .create_custom_column("saga", "Saga", "series", false, &[])
        .unwrap();
    library
        .create_custom_column("mine", "My rating", "rating", false, &[])
        .unwrap();
    assert!(library
        .create_custom_column("saga", "Again", "text", false, &[])
        .is_err());
    assert!(library
        .create_custom_column("Bad Name", "x", "text", false, &[])
        .is_err());
    assert!(library
        .create_custom_column("many", "x", "int", true, &[])
        .is_err());

    library.set_custom_value(1, "medium", "paper").unwrap();
    assert!(library.set_custom_value(1, "medium", "vinyl").is_err());
    library.set_custom_value(1, "saga", "Arrakis [3]").unwrap();
    library.set_custom_value(1, "mine", "4.5").unwrap();
    assert!(library.set_custom_value(1, "mine", "9").is_err());
    let values = library.custom_values(1).unwrap();
    assert_eq!(values["medium"], ["Paper"]);
    assert_eq!(values["saga"], ["Arrakis [3]"]);
    assert_eq!(values["mine"], ["4.5"]);
    assert_eq!(titles(&library, "#mine:>4"), ["Dune"]);

    library.set_custom_value(1, "saga", "").unwrap();
    assert!(!library.custom_values(1).unwrap().contains_key("saga"));
    library.delete_custom_column("medium").unwrap();
    assert_eq!(library.custom_columns().unwrap().len(), 2);
    // Custom values survive the trash and a copy to another library.
    library.set_custom_value(1, "saga", "Arrakis [3]").unwrap();
    library.trash_book(1).unwrap();
    library.restore_book(1).unwrap();
    let values = library.custom_values(1).unwrap();
    assert_eq!(values["saga"], ["Arrakis [3]"]);
    assert_eq!(values["mine"], ["4.5"]);

    let mut other = Library::create(&_temp.path().join("other")).unwrap();
    other
        .create_custom_column("saga", "Saga", "series", false, &[])
        .unwrap();
    let copied = library
        .copy_book_to(1, &mut other, cr_core::library::DuplicatePolicy::Add)
        .unwrap();
    let cr_core::library::AddOutcome::Added(copied) = copied else {
        panic!("not added");
    };
    let values = other.custom_values(copied.id).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values["saga"], ["Arrakis [3]"]);
}

#[test]
fn categories_count_rename_and_merge() {
    let (_temp, mut library) = fixture();
    let tags = library.category_items(Category::Tags).unwrap();
    let counts: Vec<(&str, usize)> = tags.iter().map(|t| (t.name.as_str(), t.books)).collect();
    assert_eq!(
        counts,
        [("classic", 2), ("humour", 1), ("romance", 1), ("sf", 3)]
    );
    let authors = library.category_items(Category::Authors).unwrap();
    assert_eq!(authors.len(), 3);
    assert_eq!(
        library.category_items(Category::Formats).unwrap()[0].books,
        4
    );

    let renamed = library
        .rename_category_item(Category::Tags, "humour", "comedy")
        .unwrap();
    assert_eq!(renamed, [4]);
    // Merging into a tag the book already has leaves a single link.
    let merged = library
        .rename_category_item(Category::Tags, "comedy", "SF")
        .unwrap();
    assert_eq!(merged, [4]);
    assert_eq!(library.get_metadata(4).unwrap().tags, ["sf"]);
    assert_eq!(titles(&library, "tag:=sf").len(), 3);
    assert!(library
        .rename_category_item(Category::Tags, "nope", "x")
        .is_err());
    assert!(library
        .rename_category_item(Category::Formats, "TXT", "x")
        .is_err());

    assert!(library.set_saved_search("gone", None).is_err());
    library.set_saved_search("a", Some("tag:sf")).unwrap();
    library.set_saved_search("a", None).unwrap();
    assert!(library.saved_searches().unwrap().is_empty());
}

#[test]
fn full_text_index_finds_passages_and_tracks_changes() {
    use cr_core::jobs::JobContext;

    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let add = |library: &mut Library, name: &str, text: &str| {
        let path = temp.path().join(name);
        fs::write(&path, text).unwrap();
        library.add_book(&path).unwrap().id
    };
    let dune = add(
        &mut library,
        "Dune.txt",
        "The spice must flow.\n\nFear is the mind-killer.",
    );
    let emma = add(
        &mut library,
        "Emma.txt",
        "Emma Woodhouse, handsome, clever, and rich.",
    );
    assert!(library.fts_search("spice", 10).unwrap().is_empty());

    let context = JobContext::detached();
    let report = library.fts_index(&context).unwrap();
    assert_eq!((report.indexed, report.unchanged), (2, 0));
    let hits = library.fts_search("spice", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].book, hits[0].title.as_str()), (dune, "Dune"));
    assert!(hits[0].snippet.contains("[spice]"), "{}", hits[0].snippet);
    assert_eq!(
        library.fts_search("handsome clever", 10).unwrap()[0].book,
        emma
    );
    assert_eq!(library.fts_search("\"mind killer\"", 10).unwrap().len(), 1);
    // Not FTS syntax, still a search.
    assert_eq!(library.fts_search("mind-killer (", 10).unwrap().len(), 1);
    assert!(library.fts_search("absent", 10).unwrap().is_empty());

    let again = library.fts_index(&context).unwrap();
    assert_eq!((again.indexed, again.unchanged), (0, 2));

    let replacement = temp.path().join("new.txt");
    fs::write(
        &replacement,
        "A much longer replacement text about sandworms.",
    )
    .unwrap();
    library.replace_format(dune, "TXT", &replacement).unwrap();
    library.remove_book(emma, true).unwrap();
    let updated = library.fts_index(&context).unwrap();
    assert_eq!((updated.indexed, updated.removed), (1, 1));
    assert!(library.fts_search("spice", 10).unwrap().is_empty());
    assert_eq!(library.fts_search("sandworms", 10).unwrap()[0].book, dune);
    assert!(library.fts_search("handsome", 10).unwrap().is_empty());
}

#[test]
fn catalog_lists_books_with_custom_columns() {
    let (_temp, mut library) = fixture();
    library
        .create_custom_column("pages", "Pages", "int", false, &[])
        .unwrap();
    library.set_custom_value(1, "pages", "412").unwrap();
    library
        .update_book(
            3,
            &BookUpdate {
                title: Some("=Emma, \"again\"".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let csv = library.catalog("tag:classic", CatalogFormat::Csv).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("id,title,authors,") && lines[0].ends_with(",#pages"));
    assert!(lines[2].starts_with("1,Dune,Frank Herbert,Dune,1.0,\"sf, classic\",,5.0,"));
    assert!(lines[2].ends_with(",TXT,412"));
    assert!(lines[1].starts_with("3,\"'=Emma, \"\"again\"\"\",Jane Austen,,,"));

    let json = library.catalog("", CatalogFormat::Json).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 4);
    let dune = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == 1)
        .unwrap();
    assert_eq!(dune["#pages"], "412");
    assert_eq!(dune["tags"], serde_json::json!(["sf", "classic"]));
}
