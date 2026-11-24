use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::tempdir;

fn cr() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cr"))
}

fn run(command: &mut Command) -> Output {
    let output = command.output().expect("failed to run cr");
    if !output.status.success() {
        panic!(
            "cr failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn make_book(dir: &Path) -> PathBuf {
    let path = dir.join("sample_book.txt");
    fs::write(
        &path,
        "Chapter 1\nHello from calibre-rust.\n\nChapter 2\nGoodbye.\n",
    )
    .unwrap();
    path
}

#[test]
fn online_commands_require_explicit_input_and_apply_target() {
    let temp = tempdir().unwrap();
    let library = temp.path().join("nonexistent-library");
    let output = cr().args(["ai", "ask", "1"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("enter a question"));

    let output = cr()
        .args(["fetch-metadata", "--apply", "tags"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--result"));

    let output = cr()
        .arg("-l")
        .arg(library)
        .arg("fetch-ebook-metadata")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Enter a title, author, or ISBN"));
}

#[test]
fn google_books_credentials_are_not_accepted_on_the_command_line() {
    let output = cr()
        .args(["fetch-metadata", "--google-books-api-key", "test-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
}

#[test]
fn ai_studio_requires_search_text_and_recap_chapter() {
    for (args, expected) in [
        (vec!["ai", "library", "search"], "<REQUEST>"),
        (vec!["ai", "book", "recap", "1"], "--through"),
    ] {
        let output = cr().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }
}

#[test]
fn cli_workflow() {
    let temp = tempdir().unwrap();
    let library = temp.path().join("library");
    let book = make_book(temp.path());

    run(cr().args(["--library", library.to_str().unwrap(), "init"]));

    let added = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "add",
        book.to_str().unwrap(),
    ]));
    assert!(text(&added).contains("#1"));

    let listed = run(cr().args(["--library", library.to_str().unwrap(), "--json", "list"]));
    let books: serde_json::Value = serde_json::from_str(&text(&listed)).unwrap();
    assert_eq!(books[0]["id"], 1);
    assert_eq!(books[0]["title"], "sample book");
    assert_eq!(books[0]["formats"][0], "TXT");

    let filtered = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "--json",
        "list",
        "tag:missing",
    ]));
    let empty: serde_json::Value = serde_json::from_str(&text(&filtered)).unwrap();
    assert!(empty.as_array().unwrap().is_empty());

    let set = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "set",
        "1",
        "--title",
        "A Better Title",
        "--author",
        "Jane Doe & John Smith",
        "--tag",
        "Fiction,Adventure",
        "--series",
        "Saga",
        "--series-index",
        "2",
        "--stars",
        "4",
        "--publisher",
        "Test Press",
        "--language",
        "en",
        "--identifier",
        "isbn=9780000000000",
        "--pubdate",
        "2020-05-01",
    ]));
    assert!(text(&set).contains("Updated #1"));

    let shown = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "--json",
        "show",
        "1",
    ]));
    let meta: serde_json::Value = serde_json::from_str(&text(&shown)).unwrap();
    assert_eq!(meta["title"], "A Better Title");
    assert_eq!(meta["authors"][0], "Jane Doe");
    assert_eq!(meta["authors"][1], "John Smith");
    assert_eq!(meta["tags"][1], "Adventure");
    assert_eq!(meta["series"], "Saga");
    assert_eq!(meta["series_index"], 2.0);
    assert_eq!(meta["rating"], 8);
    assert_eq!(meta["publisher"], "Test Press");
    assert_eq!(meta["identifiers"]["isbn"], "9780000000000");
    assert_eq!(meta["languages"][0], "en");
    assert_eq!(meta["formats"][0], "TXT");

    let chapters = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "read",
        "1",
        "--list",
    ]));
    assert!(text(&chapters).contains("Chapter 1"));

    let chapter = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "read",
        "1",
        "--chapter",
        "2",
    ]));
    assert!(text(&chapter).contains("Goodbye."));

    let formats = run(cr().args(["--library", library.to_str().unwrap(), "formats", "1"]));
    assert!(text(&formats).contains("TXT"));

    let export_dir = temp.path().join("export");
    let exported = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "export",
        "1",
        "--to",
        export_dir.to_str().unwrap(),
    ]));
    assert!(text(&exported).contains("A Better Title"));

    // Device detection and sending.
    let device_root = temp.path().join("kindle");
    fs::create_dir_all(device_root.join("system")).unwrap();
    fs::create_dir_all(device_root.join("documents")).unwrap();
    let devices = run(cr().args(["--json", "devices"]));
    // Discovery scans real mount points, so this only checks valid JSON output.
    let _: serde_json::Value = serde_json::from_str(&text(&devices)).unwrap();

    let sent = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "--json",
        "send",
        "1",
        "--device",
        device_root.to_str().unwrap(),
    ]));
    let report: serde_json::Value = serde_json::from_str(&text(&sent)).unwrap();
    assert_eq!(report["sent"].as_array().unwrap().len(), 1);
    let catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(device_root.join("metadata.calibre")).unwrap()).unwrap();
    assert_eq!(catalog[0]["title"], "A Better Title");
    assert!(device_root.join("documents").read_dir().unwrap().count() == 1);

    let info = run(cr().args(["--library", library.to_str().unwrap(), "--json", "info"]));
    let stats: serde_json::Value = serde_json::from_str(&text(&info)).unwrap();
    assert_eq!(stats["books"], 1);
    assert_eq!(stats["authors"], 2);

    let removed = run(cr().args([
        "--library",
        library.to_str().unwrap(),
        "rm",
        "1",
        "--delete-files",
    ]));
    assert!(text(&removed).contains("Removed #1"));
    let after = run(cr().args(["--library", library.to_str().unwrap(), "--json", "list"]));
    let after: serde_json::Value = serde_json::from_str(&text(&after)).unwrap();
    assert!(after.as_array().unwrap().is_empty());
}
