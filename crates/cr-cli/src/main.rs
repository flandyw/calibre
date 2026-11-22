//! `cr` — the calibre-rust command line interface.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use cr_core::device::{self, Device, SendOptions};
use cr_core::jobs::JobContext;
use cr_core::library::{
    filename_pattern, scan_sources, AddOutcome, BookSort, BookUpdate, BulkEdit, Collision,
    DuplicatePolicy, ExportOptions, ImportOptions, Library, ListOptions, SearchReplace, TextField,
};
use cr_core::metadata::{htmlize_comments, parse_user_date, Metadata};
use cr_core::text::{self, split_authors};
use cr_core::Chapter;
use serde_json::{json, Value};

const DEFAULT_TEMPLATE: &str = "{author_sort}/{title} ({id})/{title} - {author}";

#[derive(Debug, Parser)]
#[command(
    name = "cr",
    version,
    about = "calibre-rust: a small, fast calibre replacement",
    long_about = "Manage a calibre-compatible ebook library, read books in the terminal and send them to e-readers."
)]
struct Cli {
    /// Library directory (default: ~/Calibre Library)
    #[arg(short, long, global = true, env = "CR_LIBRARY")]
    library: Option<PathBuf>,

    /// Print machine-readable JSON where supported
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new calibre-compatible library
    Init {
        /// Directory to create (defaults to the selected library)
        path: Option<PathBuf>,
    },
    /// Add ebook files to the library
    Add {
        /// Files, or folders to scan for books
        files: Vec<PathBuf>,
        /// Scan folders recursively
        #[arg(short, long)]
        recursive: bool,
        /// When a book with the same title and authors exists: add, skip or merge
        #[arg(long, default_value = "add")]
        duplicates: String,
        /// Read metadata from file names with a regex using the named groups
        /// title, author, series and series_index
        #[arg(long)]
        filename_pattern: Option<String>,
        /// Create a record without a file, using this title
        #[arg(long, conflicts_with = "files")]
        empty: Option<String>,
    },
    /// Attach a file to a book as another format
    AddFormat {
        id: i64,
        file: PathBuf,
        /// Replace the format if the book already has it
        #[arg(long)]
        replace: bool,
    },
    /// Remove one format from a book (the file goes to the trash)
    RemoveFormat { id: i64, format: String },
    /// List, restore or permanently delete removed books
    Trash {
        /// list, restore or empty
        #[arg(default_value = "list")]
        action: String,
        /// Books to restore, or to delete permanently with `empty`
        ids: Vec<i64>,
        /// With `restore`: put this removed format back on the book
        #[arg(long)]
        format: Option<String>,
    },
    /// Apply one metadata change to many books
    Bulk(BulkArgs),
    /// Copy or move books into another library
    Copy {
        ids: Vec<i64>,
        /// The other library's directory
        #[arg(long)]
        to: PathBuf,
        /// Move instead of copy: the books go to this library's trash
        #[arg(long = "move")]
        remove_source: bool,
        /// When the book exists in the other library: add, skip or merge
        #[arg(long, default_value = "skip")]
        duplicates: String,
    },
    /// List or search books
    List {
        /// Search query, e.g. `author:tolkien tag:fantasy dune`
        query: Vec<String>,
        /// Sort field: title, author, date, pubdate, series, rating
        #[arg(long)]
        sort: Option<String>,
        /// Sort descending
        #[arg(long)]
        desc: bool,
        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show all metadata for one book
    Show { id: i64 },
    /// Edit book metadata
    Set(SetArgs),
    /// Remove books from the library
    #[command(name = "rm", alias = "remove")]
    Remove {
        ids: Vec<i64>,
        /// Delete the books and their files for good instead of moving them
        /// to the trash
        #[arg(long, alias = "delete-files")]
        permanent: bool,
    },
    /// List the formats stored for a book
    Formats { id: i64 },
    /// Read a book in the terminal
    Read {
        id: i64,
        /// Format to read (default: first stored format)
        #[arg(long)]
        format: Option<String>,
        /// Chapter number, starting at 1
        #[arg(long)]
        chapter: Option<usize>,
        /// List chapters without printing them
        #[arg(long)]
        list: bool,
        /// Print the whole book
        #[arg(long)]
        all: bool,
        /// Wrap width for text output
        #[arg(long, default_value_t = 80)]
        width: usize,
    },
    /// Convert EPUB to AZW3/MOBI or Kindle files to EPUB/TXT
    Convert {
        /// Input file or library book ID
        input: PathBuf,
        /// New output file (.azw3, .mobi, .epub or .txt)
        output: PathBuf,
        /// Source format to use for a library book
        #[arg(long)]
        from: Option<String>,
        /// Store the converted file on the same library book, replacing that format if present
        #[arg(long)]
        add: bool,
    },
    /// Save a book's cover image
    Cover {
        id: i64,
        /// Output file (default: cover-<id>.jpg)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Unpack or repack an EPUB for editing
    Edit {
        id: i64,
        /// Unpack the EPUB into this directory
        #[arg(long)]
        extract: Option<PathBuf>,
        /// Repack this directory into an EPUB
        #[arg(long)]
        pack: Option<PathBuf>,
        /// Output EPUB for --pack (default: book-<id>.epub)
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value = "EPUB")]
        format: String,
        /// Do not replace the library copy after repacking
        #[arg(long)]
        no_replace: bool,
    },
    /// Copy books out of the library
    Export {
        ids: Vec<i64>,
        /// Destination directory
        #[arg(short = 'o', long = "to")]
        destination: PathBuf,
        /// Path template ({author_sort}/{title} ({id})/{title} - {author})
        #[arg(long, default_value = DEFAULT_TEMPLATE)]
        template: String,
        /// Only export these formats (repeat or comma-separate)
        #[arg(long)]
        format: Vec<String>,
        /// Also write the cover as a .jpg
        #[arg(long)]
        cover: bool,
        /// Also write the metadata as an .opf
        #[arg(long)]
        opf: bool,
        /// When a file exists: rename, overwrite or skip
        #[arg(long, default_value = "rename")]
        collision: String,
    },
    /// List connected e-readers
    Devices,
    /// Send books to a connected e-reader
    Send {
        ids: Vec<i64>,
        /// Mount path or MTP identifier from `cr devices` (auto-detected when omitted)
        #[arg(long)]
        device: Option<PathBuf>,
        /// Format to send (default: preferred available format)
        #[arg(long)]
        format: Option<String>,
        /// Show what would be copied without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Show library statistics
    Info,
}

#[derive(Debug, Args)]
struct BulkArgs {
    /// Books to change (default: every book matching --query)
    ids: Vec<i64>,
    /// Select books with a search query instead of ids
    #[arg(long)]
    query: Option<String>,
    #[arg(long = "add-tag")]
    add_tags: Vec<String>,
    #[arg(long = "remove-tag")]
    remove_tags: Vec<String>,
    #[arg(long)]
    clear_tags: bool,
    /// Replace the authors (separate several with &)
    #[arg(long)]
    authors: Option<String>,
    /// Set the publisher (empty clears it)
    #[arg(long)]
    publisher: Option<String>,
    /// Set the series (empty clears it)
    #[arg(long)]
    series: Option<String>,
    /// Number the books in the given order, starting here
    #[arg(long)]
    series_start: Option<f64>,
    /// Set the rating in stars, 0-5 (0 clears it)
    #[arg(long)]
    stars: Option<u8>,
    /// Search and replace in a field: title, authors, publisher, series, tags, comments
    #[arg(long, requires = "search")]
    field: Option<String>,
    #[arg(long, requires = "field")]
    search: Option<String>,
    #[arg(long, default_value = "")]
    replace: String,
    /// Treat --search as a regular expression
    #[arg(long)]
    regex: bool,
    #[arg(long)]
    ignore_case: bool,
    /// Copy one field into another, e.g. `series:tags`
    #[arg(long)]
    copy: Option<String>,
    /// Show the changes without saving them
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// Book ids to edit
    ids: Vec<i64>,

    #[arg(long)]
    title: Option<String>,

    /// Author names, separated by & / and / ;
    #[arg(long, alias = "authors")]
    author: Option<String>,

    /// Replace the tag list (repeatable or comma-separated)
    #[arg(long = "tag")]
    tags: Vec<String>,

    /// Add tags without replacing existing ones
    #[arg(long = "add-tag")]
    add_tags: Vec<String>,

    /// Remove tags
    #[arg(long = "remove-tag")]
    remove_tags: Vec<String>,

    #[arg(long)]
    clear_tags: bool,

    #[arg(long)]
    series: Option<String>,

    #[arg(long)]
    series_index: Option<f64>,

    #[arg(long)]
    clear_series: bool,

    #[arg(long)]
    publisher: Option<String>,

    #[arg(long)]
    clear_publisher: bool,

    /// Rating on calibre's 0-10 scale (0 clears it)
    #[arg(long)]
    rating: Option<u8>,

    /// Rating in stars (1-5)
    #[arg(long)]
    stars: Option<u8>,

    #[arg(long)]
    comments: Option<String>,

    #[arg(long = "comments-file")]
    comments_file: Option<PathBuf>,

    /// Language code (repeatable)
    #[arg(long = "language")]
    languages: Vec<String>,

    #[arg(long)]
    clear_languages: bool,

    /// Identifier as type=value or type:value (repeatable)
    #[arg(long = "identifier")]
    identifiers: Vec<String>,

    #[arg(long)]
    clear_identifiers: bool,

    /// Publication date (YYYY-MM-DD)
    #[arg(long)]
    pubdate: Option<String>,

    /// Set the cover from an image file
    #[arg(long)]
    cover: Option<PathBuf>,

    #[arg(long)]
    clear_cover: bool,
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("cr: {err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let library_path = cli.library.unwrap_or_else(default_library_path);
    match cli.command {
        Command::Init { path } => {
            let path = path.unwrap_or(library_path);
            let library = Library::create(&path)?;
            if cli.json {
                println!(
                    "{}",
                    json!({"path": path, "library_id": library.library_id()?})
                );
            } else {
                println!("Created library at {}", path.display());
            }
        }
        Command::Add {
            files,
            recursive,
            duplicates,
            filename_pattern: pattern,
            empty,
        } => {
            let mut library = Library::open(&library_path)?;
            if let Some(title) = empty {
                let added = library.add_empty(Metadata::new(title))?;
                println!("#{}\t{}", added.id, added.title);
                return Ok(());
            }
            if files.is_empty() {
                bail!("no files given");
            }
            let options = ImportOptions {
                duplicates: parse_duplicates(&duplicates)?,
                filename_pattern: pattern.as_deref().map(filename_pattern).transpose()?,
            };
            let sources = scan_sources(&files, recursive);
            if sources.is_empty() {
                bail!("no book files found");
            }
            let mut failures = 0usize;
            for item in library.import_books(&sources, &options, &JobContext::detached()) {
                let file = item.item.display().to_string();
                match item.result {
                    Ok(AddOutcome::Added(added)) => {
                        if cli.json {
                            println!(
                                "{}",
                                json!({
                                    "id": added.id,
                                    "title": added.title,
                                    "authors": added.authors,
                                    "formats": added.formats,
                                    "path": added.path,
                                })
                            );
                        } else {
                            println!("#{}\t{}", added.id, added.title);
                        }
                    }
                    Ok(AddOutcome::Merged { id, format, added }) => {
                        if cli.json {
                            println!("{}", json!({"id": id, "merged": format, "added": added}));
                        } else if added {
                            println!("#{id}\t+{format}\t{file}");
                        } else {
                            println!("#{id}\talready has {format}\t{file}");
                        }
                    }
                    Ok(AddOutcome::Skipped { existing }) => {
                        if cli.json {
                            println!("{}", json!({"skipped": file, "existing": existing}));
                        } else {
                            println!("skipped\t{file}\t(same as #{})", existing[0]);
                        }
                    }
                    Err(err) => {
                        failures += 1;
                        eprintln!("{file}: {err}");
                    }
                }
            }
            if failures > 0 {
                bail!("{failures} file(s) could not be added");
            }
        }
        Command::AddFormat { id, file, replace } => {
            let mut library = Library::open(&library_path)?;
            if library.add_format(id, &file, replace)? {
                println!("#{id} now has {}", library.formats(id)?.join(", "));
            } else {
                bail!("book {id} already has that format; pass --replace to overwrite it");
            }
        }
        Command::RemoveFormat { id, format } => {
            let mut library = Library::open(&library_path)?;
            library.remove_format(id, &format)?;
            println!("Removed {} from #{id}", format.to_ascii_uppercase());
        }
        Command::Trash {
            action,
            ids,
            format,
        } => {
            let mut library = Library::open(&library_path)?;
            match action.as_str() {
                "list" => {
                    let entries = library.list_trash()?;
                    if cli.json {
                        print_json(&json!(entries
                            .iter()
                            .map(|entry| json!({
                                "id": entry.id,
                                "book": entry.is_book,
                                "title": entry.title,
                                "authors": entry.authors,
                                "formats": entry.formats,
                            }))
                            .collect::<Vec<_>>()))?;
                        return Ok(());
                    }
                    if entries.is_empty() {
                        println!("The trash is empty.");
                    }
                    for entry in entries {
                        let kind = if entry.is_book { "book" } else { "format" };
                        println!(
                            "#{}\t{kind}\t{}\t{}",
                            entry.id,
                            entry.formats.join(","),
                            entry.title
                        );
                    }
                }
                "restore" => {
                    if ids.is_empty() {
                        bail!("no book ids given");
                    }
                    for id in ids {
                        if let Some(format) = &format {
                            library.restore_format(id, format)?;
                            println!("Restored {} of #{id}", format.to_ascii_uppercase());
                            continue;
                        }
                        let restored = library.restore_book(id)?;
                        println!("Restored #{}\t{}", restored.id, restored.title);
                    }
                }
                "empty" => {
                    let mut removed = 0;
                    if ids.is_empty() {
                        removed = library.purge_trash(None)?;
                    }
                    for id in ids {
                        removed += library.purge_trash(Some(id))?;
                    }
                    println!("Deleted {removed} item(s) from the trash.");
                }
                other => bail!("unknown trash action `{other}` (use list, restore or empty)"),
            }
        }
        Command::Bulk(args) => bulk_edit(&library_path, args)?,
        Command::Copy {
            ids,
            to,
            remove_source,
            duplicates,
        } => {
            if ids.is_empty() {
                bail!("no book ids given");
            }
            let mut library = Library::open(&library_path)?;
            let mut target = Library::open(&to)?;
            let policy = parse_duplicates(&duplicates)?;
            let mut failures = 0usize;
            let results = library.transfer_books(
                &ids,
                &mut target,
                policy,
                remove_source,
                &JobContext::detached(),
            );
            for item in results {
                match item.result {
                    Ok(AddOutcome::Added(added)) => println!("#{} -> #{}", item.item, added.id),
                    Ok(AddOutcome::Merged { id, .. }) => {
                        println!("#{} merged into #{id}", item.item)
                    }
                    Ok(AddOutcome::Skipped { existing }) => {
                        println!("#{} skipped (same as #{})", item.item, existing[0])
                    }
                    Err(err) => {
                        failures += 1;
                        eprintln!("#{}: {err}", item.item);
                    }
                }
            }
            if failures > 0 {
                bail!("{failures} book(s) could not be copied");
            }
        }
        Command::List {
            query,
            sort,
            desc,
            limit,
        } => {
            let library = Library::open(&library_path)?;
            let sort = match sort {
                Some(value) => Some(
                    BookSort::parse(&value)
                        .ok_or_else(|| anyhow!("unknown sort field: {value}"))?,
                ),
                None => None,
            };
            let options = ListOptions {
                query: if query.is_empty() {
                    None
                } else {
                    Some(query.join(" "))
                },
                sort,
                descending: desc,
                limit,
            };
            let books = library.list_books(&options)?;
            if cli.json {
                let value = Value::Array(books.iter().map(|book| book.to_json()).collect());
                print_json(&value)?;
            } else if books.is_empty() {
                println!("No books found.");
            } else {
                print_book_table(&books);
            }
        }
        Command::Show { id } => {
            let library = Library::open(&library_path)?;
            let meta = library.get_metadata(id)?;
            let formats = library.formats(id)?;
            let path = library.book_path(id)?;
            if cli.json {
                let mut value = meta.to_json();
                value["id"] = json!(id);
                value["formats"] = json!(formats);
                value["path"] = json!(path);
                print_json(&value)?;
            } else {
                print_metadata(id, &meta, &formats, &path);
            }
        }
        Command::Set(args) => {
            let mut library = Library::open(&library_path)?;
            edit_metadata(&mut library, args, cli.json)?;
        }
        Command::Remove { ids, permanent } => {
            if ids.is_empty() {
                bail!("no book ids given");
            }
            let mut library = Library::open(&library_path)?;
            for id in ids {
                if permanent {
                    library.remove_book(id, true)?;
                    println!("Removed #{id} permanently");
                } else {
                    library.trash_book(id)?;
                    println!(
                        "Removed #{id} (in the trash; `cr trash restore {id}` brings it back)"
                    );
                }
            }
        }
        Command::Formats { id } => {
            let library = Library::open(&library_path)?;
            let formats = library.formats(id)?;
            if formats.is_empty() {
                println!("No formats stored for book {id}.");
            }
            for format in formats {
                if let Some(path) = library.format_path(id, &format)? {
                    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    println!("{:<7} {:>10}  {}", format, human_size(size), path.display());
                }
            }
        }
        Command::Read {
            id,
            format,
            chapter,
            list,
            all,
            width,
        } => read_book(&library_path, id, format, chapter, list, all, width)?,
        Command::Convert {
            input,
            output,
            from,
            add,
        } => {
            convert_book(&library_path, input, output, from, add, cli.json)?;
        }
        Command::Cover { id, output } => {
            let library = Library::open(&library_path)?;
            let data = library
                .cover(id)?
                .ok_or_else(|| anyhow!("book {id} has no cover"))?;
            let output = output.unwrap_or_else(|| PathBuf::from(format!("cover-{id}.jpg")));
            fs::write(&output, data)?;
            println!("{}", output.display());
        }
        Command::Edit {
            id,
            extract,
            pack,
            output,
            format,
            no_replace,
        } => edit_book(
            &library_path,
            id,
            extract,
            pack,
            output,
            &format,
            no_replace,
        )?,
        Command::Export {
            ids,
            destination,
            template,
            format,
            cover,
            opf,
            collision,
        } => {
            if ids.is_empty() {
                bail!("no book ids given");
            }
            let library = Library::open(&library_path)?;
            let options = ExportOptions {
                template,
                formats: split_list(&format),
                cover,
                opf,
                collision: Collision::parse(&collision)
                    .with_context(|| format!("unknown --collision `{collision}`"))?,
            };
            let mut written = Vec::new();
            for id in ids {
                written.extend(library.export_book(id, &destination, &options)?);
            }
            if written.is_empty() {
                println!("Nothing to export.");
            }
            for path in written {
                println!("{}", path.display());
            }
        }
        Command::Devices => {
            let devices = device::discover()?;
            if cli.json {
                print_json(&json!(devices
                    .iter()
                    .map(|device| json!({
                        "name": device.name,
                        "kind": device.kind.as_str(),
                        "root": device.root,
                    }))
                    .collect::<Vec<_>>()))?;
            } else if devices.is_empty() {
                println!("No e-readers detected.");
            } else {
                for device in devices {
                    println!(
                        "{:<20} {:<8} {}",
                        device.name,
                        device.kind.as_str(),
                        device.root.display()
                    );
                }
            }
        }
        Command::Send {
            ids,
            device: device_path,
            format,
            dry_run,
        } => {
            if ids.is_empty() {
                bail!("no book ids given");
            }
            let library = Library::open(&library_path)?;
            let device = resolve_device(device_path)?;
            let report = device::send(&library, &device, &ids, &SendOptions { format, dry_run })?;
            if cli.json {
                print_json(&json!({
                    "device": device.name,
                    "catalog": report.catalog,
                    "dry_run": report.dry_run,
                    "sent": report.sent.iter().map(|sent| json!({
                        "id": sent.id,
                        "title": sent.title,
                        "format": sent.format,
                        "path": sent.path,
                        "bytes": sent.bytes,
                    })).collect::<Vec<_>>(),
                    "skipped": report.skipped.iter().map(|(id, reason)| json!({
                        "id": id,
                        "reason": reason,
                    })).collect::<Vec<_>>(),
                }))?;
            } else {
                for sent in &report.sent {
                    println!(
                        "{}{} -> {}",
                        if dry_run { "[dry-run] " } else { "" },
                        sent.title,
                        sent.path.display()
                    );
                }
                for (id, reason) in &report.skipped {
                    eprintln!("skipped #{id}: {reason}");
                }
                if let Some(catalog) = &report.catalog {
                    println!(
                        "Catalog: {}{}",
                        catalog.display(),
                        if dry_run { " (not written)" } else { "" }
                    );
                }
            }
        }
        Command::Info => {
            let library = Library::open(&library_path)?;
            let stats = library.stats()?;
            if cli.json {
                print_json(&json!({
                    "books": stats.books,
                    "authors": stats.authors,
                    "series": stats.series,
                    "tags": stats.tags,
                    "publishers": stats.publishers,
                    "total_bytes": stats.total_bytes,
                    "formats": stats.formats,
                }))?;
            } else {
                println!("Library:    {}", library.root().display());
                println!("Books:      {}", stats.books);
                println!("Authors:    {}", stats.authors);
                println!("Series:     {}", stats.series);
                println!("Tags:       {}", stats.tags);
                println!("Publishers: {}", stats.publishers);
                println!("Size:       {}", human_size(stats.total_bytes));
                if !stats.formats.is_empty() {
                    let formats = stats
                        .formats
                        .iter()
                        .map(|(format, count)| format!("{format}: {count}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!("Formats:    {formats}");
                }
            }
        }
    }
    Ok(())
}

fn parse_duplicates(value: &str) -> Result<DuplicatePolicy> {
    DuplicatePolicy::parse(value)
        .with_context(|| format!("unknown --duplicates `{value}` (use add, skip or merge)"))
}

fn bulk_edit(library_path: &Path, args: BulkArgs) -> Result<()> {
    let mut library = Library::open(library_path)?;
    let ids: Vec<i64> = match (&args.query, args.ids.is_empty()) {
        (Some(query), true) => library
            .list_books(&ListOptions {
                query: Some(query.clone()),
                ..Default::default()
            })?
            .iter()
            .map(|book| book.id)
            .collect(),
        (None, false) => args.ids.clone(),
        (Some(_), false) => bail!("give book ids or --query, not both"),
        (None, true) => bail!("no book ids or --query given"),
    };
    let field =
        |name: &str| TextField::parse(name).with_context(|| format!("unknown field `{name}`"));
    let optional = |value: String| Some(value.trim().to_string()).filter(|v| !v.is_empty());
    let edit = BulkEdit {
        authors: args.authors.as_deref().map(split_authors),
        publisher: args.publisher.map(optional),
        series: args.series.map(optional),
        series_start: args.series_start,
        rating: args
            .stars
            .map(|stars| (stars > 0).then_some(stars.min(5) * 2)),
        languages: None,
        clear_tags: args.clear_tags,
        remove_tags: split_list(&args.remove_tags),
        add_tags: split_list(&args.add_tags),
        copy: match &args.copy {
            Some(pair) => {
                let (from, to) = pair
                    .split_once(':')
                    .context("--copy takes `from:to`, e.g. series:tags")?;
                Some((field(from)?, field(to)?))
            }
            None => None,
        },
        replace: match (&args.field, args.search) {
            (Some(name), Some(pattern)) => Some(SearchReplace {
                field: field(name)?,
                pattern,
                replacement: args.replace,
                regex: args.regex,
                ignore_case: args.ignore_case,
            }),
            _ => None,
        },
    };
    if args.dry_run {
        for (position, &id) in ids.iter().enumerate() {
            let patch = library.bulk_patch(id, position, &edit)?;
            if patch != BookUpdate::default() {
                println!("#{id}\t{}", describe_patch(&patch));
            }
        }
        return Ok(());
    }
    let mut changed = 0usize;
    let mut failures = 0usize;
    for item in library.bulk_update(&ids, &edit, &JobContext::detached()) {
        match item.result {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(err) => {
                failures += 1;
                eprintln!("#{}: {err}", item.item);
            }
        }
    }
    println!("Changed {changed} of {} book(s).", ids.len());
    if failures > 0 {
        bail!("{failures} book(s) could not be changed");
    }
    Ok(())
}

fn describe_patch(patch: &BookUpdate) -> String {
    let mut parts = Vec::new();
    if let Some(title) = &patch.title {
        parts.push(format!("title={title}"));
    }
    if let Some(authors) = &patch.authors {
        parts.push(format!("authors={}", authors.join(" & ")));
    }
    if let Some(publisher) = &patch.publisher {
        parts.push(format!(
            "publisher={}",
            publisher.clone().unwrap_or_default()
        ));
    }
    if let Some(series) = &patch.series {
        parts.push(format!("series={}", series.clone().unwrap_or_default()));
    }
    if let Some(index) = patch.series_index {
        parts.push(format!("series_index={index}"));
    }
    if let Some(tags) = &patch.tags {
        parts.push(format!("tags={}", tags.join(", ")));
    }
    if let Some(rating) = &patch.rating {
        parts.push(format!("stars={}", rating.unwrap_or(0) / 2));
    }
    if patch.comments.is_some() {
        parts.push("comments".to_string());
    }
    parts.join("; ")
}

fn edit_metadata(library: &mut Library, args: SetArgs, json: bool) -> Result<()> {
    if args.ids.is_empty() {
        bail!("no book ids given");
    }
    if args.rating.is_some() && args.stars.is_some() {
        bail!("use either --rating (0-10) or --stars (1-5), not both");
    }
    if args.comments.is_some() && args.comments_file.is_some() {
        bail!("use either --comments or --comments-file, not both");
    }

    let mut update = BookUpdate::default();
    if let Some(title) = args.title {
        update.title = Some(title);
    }
    if let Some(author) = args.author {
        let authors = split_authors(&author);
        if authors.is_empty() {
            bail!("no author names parsed");
        }
        update.authors = Some(authors);
    }
    if !args.tags.is_empty() {
        update.tags = Some(split_list(&args.tags));
    }
    if args.clear_tags {
        update.tags = Some(Vec::new());
    }
    if let Some(publisher) = args.publisher {
        update.publisher = Some(Some(publisher));
    }
    if args.clear_publisher {
        update.publisher = Some(None);
    }
    if let Some(series) = args.series {
        update.series = Some(Some(series));
    }
    if args.clear_series {
        update.series = Some(None);
    }
    if let Some(index) = args.series_index {
        update.series_index = Some(index);
    }
    if let Some(rating) = args.rating {
        update.rating = Some(if rating == 0 {
            None
        } else {
            Some(rating.min(10))
        });
    }
    if let Some(stars) = args.stars {
        if !(1..=5).contains(&stars) {
            bail!("--stars must be between 1 and 5");
        }
        update.rating = Some(Some(stars * 2));
    }
    if let Some(comments) = args.comments {
        update.comments = Some(Some(htmlize_comments(&comments)));
    }
    if let Some(file) = args.comments_file {
        let text =
            fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        update.comments = Some(Some(htmlize_comments(&text)));
    }
    if !args.languages.is_empty() {
        update.languages = Some(split_list(&args.languages));
    }
    if args.clear_languages {
        update.languages = Some(Vec::new());
    }
    if !args.identifiers.is_empty() {
        update.identifiers = Some(parse_identifiers(&args.identifiers)?);
    }
    if args.clear_identifiers {
        update.identifiers = Some(BTreeMap::new());
    }
    if let Some(pubdate) = args.pubdate {
        let date = parse_user_date(&pubdate)
            .ok_or_else(|| anyhow!("invalid date: {pubdate} (use YYYY-MM-DD)"))?;
        update.pubdate = Some(Some(date));
    }
    if let Some(cover) = args.cover {
        let data = fs::read(&cover).with_context(|| format!("reading {}", cover.display()))?;
        update.cover = Some(Some(data));
    }
    if args.clear_cover {
        update.cover = Some(None);
    }

    let add_tags = split_list(&args.add_tags);
    let remove_tags = split_list(&args.remove_tags);
    if update == BookUpdate::default() && add_tags.is_empty() && remove_tags.is_empty() {
        bail!("nothing to change (see --help)");
    }

    for id in args.ids {
        let mut book_update = update.clone();
        if !add_tags.is_empty() || !remove_tags.is_empty() {
            let mut tags = match &book_update.tags {
                Some(tags) => tags.clone(),
                None => library.get_metadata(id)?.tags,
            };
            for tag in &add_tags {
                if !tags
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(tag))
                {
                    tags.push(tag.clone());
                }
            }
            for tag in &remove_tags {
                tags.retain(|existing| !existing.eq_ignore_ascii_case(tag));
            }
            book_update.tags = Some(tags);
        }
        library.update_book(id, &book_update)?;
        let meta = library.get_metadata(id)?;
        if json {
            print_json(&meta.to_json())?;
        } else {
            println!("Updated #{}: {}", id, meta.title);
        }
    }
    Ok(())
}

fn convert_book(
    library_path: &Path,
    input: PathBuf,
    output: PathBuf,
    from: Option<String>,
    add: bool,
    json: bool,
) -> Result<()> {
    let id = input
        .to_str()
        .filter(|_| !input.exists())
        .and_then(|input| input.parse::<i64>().ok());
    let target = text::format_from_path(&output)
        .ok_or_else(|| anyhow!("give the output a .azw3, .mobi, .epub or .txt extension"))?;
    if let Some(id) = id {
        let mut library = Library::open(library_path)?;
        let kindle_formats = ["AZW3", "AZW", "MOBI", "PRC"];
        let from = from.as_deref();
        let candidates = if from.is_some() {
            from.as_slice()
        } else {
            match target.as_str() {
                "AZW3" | "MOBI" => &["EPUB"][..],
                "EPUB" | "TXT" => &kindle_formats[..],
                _ => bail!("unsupported conversion target {target}"),
            }
        };
        let mut source = None;
        for format in candidates {
            if let Some(path) = library.format_path(id, format)? {
                source = Some(path);
                break;
            }
        }
        let source = source
            .ok_or_else(|| anyhow!("book {id} has no input file in {}", candidates.join(", ")))?;
        cr_core::formats::convert(&source, &output)?;
        if add {
            library.replace_format(id, &target, &output)?;
        }
    } else {
        if add || from.is_some() {
            bail!("--add and --from require a library book ID");
        }
        cr_core::formats::convert(&input, &output)?;
    }
    if json {
        print_json(&json!({"path": output, "format": target, "book_id": id, "added": add}))?;
    } else {
        println!("{}", output.display());
    }
    Ok(())
}

fn read_book(
    library_path: &PathBuf,
    id: i64,
    format: Option<String>,
    chapter: Option<usize>,
    list: bool,
    all: bool,
    width: usize,
) -> Result<()> {
    let library = Library::open(library_path)?;
    let book = library.read_book(id, format.as_deref())?;
    if book.chapters.is_empty() {
        bail!("no readable text found for book {id}");
    }
    if list {
        for (index, chapter) in book.chapters.iter().enumerate() {
            println!(
                "{:>3}. {} ({} words)",
                index + 1,
                chapter.title,
                chapter.text.split_whitespace().count()
            );
        }
        return Ok(());
    }
    if all {
        for (index, chapter) in book.chapters.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_chapter(chapter, width);
        }
        return Ok(());
    }
    let index = chapter.unwrap_or(1);
    if index == 0 || index > book.chapters.len() {
        bail!(
            "chapter {index} does not exist (book has {})",
            book.chapters.len()
        );
    }
    print_chapter(&book.chapters[index - 1], width);
    Ok(())
}

fn edit_book(
    library_path: &PathBuf,
    id: i64,
    extract: Option<PathBuf>,
    pack: Option<PathBuf>,
    output: Option<PathBuf>,
    format: &str,
    no_replace: bool,
) -> Result<()> {
    if extract.is_none() && pack.is_none() {
        bail!("specify --extract DIR and/or --pack DIR");
    }
    if !format.eq_ignore_ascii_case("EPUB") {
        bail!("--format currently only supports EPUB");
    }
    let mut library = Library::open(library_path)?;

    if let Some(directory) = extract {
        let count = library.unpack_book(id, "EPUB", &directory)?;
        println!("Extracted {count} files to {}", directory.display());
    }
    if let Some(directory) = pack {
        let output = output.unwrap_or_else(|| PathBuf::from(format!("book-{id}.epub")));
        let count = cr_core::formats::epub::repack(&directory, &output)?;
        println!("Packed {count} files into {}", output.display());
        if !no_replace {
            library.replace_format(id, "EPUB", &output)?;
            println!("Replaced EPUB in the library");
        }
    }
    Ok(())
}

fn resolve_device(path: Option<PathBuf>) -> Result<Device> {
    match path {
        Some(path) if path.to_str().is_some_and(|id| id.starts_with("mtp:")) && !path.is_dir() => {
            device::discover()?
                .into_iter()
                .find(|device| device.is_mtp() && device.root == path)
                .ok_or_else(|| {
                    anyhow!(
                        "no Kindle connected at {}. Run cr devices to rescan.",
                        path.display()
                    )
                })
        }
        Some(path) => Device::detect_at(&path).ok_or_else(|| {
            anyhow!(
                "no e-reader detected at {} (expected documents/ or system/)",
                path.display()
            )
        }),
        None => {
            let devices = device::discover()?;
            match devices.len() {
                1 => Ok(devices.into_iter().next().expect("length checked")),
                0 => bail!("no e-reader detected. Connect one in USB file-transfer mode or pass --device PATH"),
                _ => {
                    let list = devices
                        .iter()
                        .map(|device| format!("{} ({})", device.name, device.root.display()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    bail!("multiple e-readers detected: {list}. Pass --device with a mount path or MTP identifier.")
                }
            }
        }
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_chapter(chapter: &Chapter, width: usize) {
    if !chapter.title.trim().is_empty() {
        println!("{}\n", chapter.title.trim());
    }
    println!("{}", wrap_text(&chapter.text, width));
}

fn wrap_text(text: &str, width: usize) -> String {
    let width = width.max(20);
    let mut lines = Vec::new();
    for paragraph in text.replace('\r', "\n").split('\n') {
        if paragraph.trim().is_empty() {
            if lines.last().is_some_and(|line: &String| !line.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn print_book_table(books: &[cr_core::BookSummary]) {
    println!(
        "{:>5}  {:<34}  {:<24}  {:<22}  {:<8}  {}",
        "ID", "Title", "Author", "Series", "Fmts", "Added"
    );
    for book in books {
        let authors = book.authors.join(" & ");
        let series = match &book.series {
            Some(series) => format!(
                "{} #{}",
                series,
                text::format_series_index(book.series_index)
            ),
            None => String::new(),
        };
        let added = book
            .timestamp
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        println!(
            "{:>5}  {:<34}  {:<24}  {:<22}  {:<8}  {}",
            book.id,
            truncate(&book.title, 34),
            truncate(&authors, 24),
            truncate(&series, 22),
            truncate(&book.formats.join(","), 8),
            added
        );
    }
}

fn print_metadata(id: i64, meta: &Metadata, formats: &[String], path: &str) {
    println!("ID:           {id}");
    println!("Title:        {}", meta.title);
    println!("Authors:      {}", meta.authors.join(" & "));
    println!("Author sort:  {}", meta.computed_author_sort());
    println!("Title sort:   {}", meta.computed_title_sort());
    if let Some(publisher) = &meta.publisher {
        println!("Publisher:    {publisher}");
    }
    if let Some(series) = &meta.series {
        println!(
            "Series:       {} #{}",
            series,
            text::format_series_index(meta.series_index)
        );
    }
    if !meta.tags.is_empty() {
        println!("Tags:         {}", meta.tags.join(", "));
    }
    println!("Rating:       {}", rating_display(meta.rating));
    if !meta.languages.is_empty() {
        println!("Languages:    {}", meta.languages.join(", "));
    }
    if !meta.identifiers.is_empty() {
        let identifiers = meta
            .identifiers
            .iter()
            .map(|(kind, value)| format!("{kind}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Identifiers:  {identifiers}");
    }
    if let Some(pubdate) = &meta.pubdate {
        println!("Published:    {}", pubdate.format("%Y-%m-%d"));
    }
    if let Some(timestamp) = &meta.timestamp {
        println!("Added:        {}", timestamp.format("%Y-%m-%d %H:%M"));
    }
    println!("Formats:      {}", formats.join(", "));
    println!(
        "Cover:        {}",
        if meta.cover.is_some() { "yes" } else { "no" }
    );
    println!("Path:         {path}");
    if let Some(comments) = &meta.comments {
        println!("Comments:     {}", comments.replace('\n', " "));
    }
}

fn rating_display(rating: Option<u8>) -> String {
    match rating {
        Some(value) => {
            let stars = value as f64 / 2.0;
            let filled = (stars.round() as usize).min(5);
            format!(
                "{:.1}/5  {}{}",
                stars,
                "*".repeat(filled),
                "-".repeat(5 - filled)
            )
        }
        None => "-".to_string(),
    }
}

fn split_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_identifiers(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut identifiers = BTreeMap::new();
    for value in values {
        let (kind, data) = value
            .split_once('=')
            .or_else(|| value.split_once(':'))
            .ok_or_else(|| anyhow!("identifier must be type=value, got: {value}"))?;
        let kind = kind.trim().to_ascii_lowercase();
        let data = data.trim();
        if kind.is_empty() || data.is_empty() {
            bail!("identifier must be type=value, got: {value}");
        }
        identifiers.insert(kind, data.to_string());
    }
    Ok(identifiers)
}

fn default_library_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        return PathBuf::from(home).join("Calibre Library");
    }
    PathBuf::from("Calibre Library")
}

fn truncate(input: &str, width: usize) -> String {
    if input.chars().count() <= width {
        return input.to_string();
    }
    let mut out: String = input.chars().take(width.saturating_sub(2)).collect();
    out.push_str("..");
    out
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_text() {
        // Width is clamped to a readable minimum of 20 columns.
        let wrapped = wrap_text("one two three four five", 10);
        assert_eq!(wrapped, "one two three four\nfive");
    }

    #[test]
    fn parses_identifiers() {
        let ids = parse_identifiers(&["isbn=123".into(), "asin:B0".into()]).unwrap();
        assert_eq!(ids.get("isbn").map(String::as_str), Some("123"));
        assert_eq!(ids.get("asin").map(String::as_str), Some("B0"));
        assert!(parse_identifiers(&["invalid".into()]).is_err());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
    }

    #[test]
    fn truncates_long_strings() {
        assert_eq!(truncate("abcdefghij", 6), "abcd..");
        assert_eq!(truncate("abc", 6), "abc");
    }
}
