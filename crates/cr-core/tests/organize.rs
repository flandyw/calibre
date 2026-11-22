//! Search language golden fixture, saved searches, categories, custom columns.

use std::fs;

use cr_core::library::{BookUpdate, Category, Library, ListOptions};
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
    assert_eq!(values["mine"], ["9"]);
    assert_eq!(titles(&library, "#mine:>4"), ["Dune"]);

    library.set_custom_value(1, "saga", "").unwrap();
    assert!(!library.custom_values(1).unwrap().contains_key("saga"));
    library.delete_custom_column("medium").unwrap();
    assert_eq!(library.custom_columns().unwrap().len(), 2);
    // Removing a book with custom values still works.
    library.trash_book(1).unwrap();
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
