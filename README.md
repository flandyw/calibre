# calibre-rust

A small, fast, native reimplementation of calibre's core ebook workflow: library
management, metadata, reading, editing and sending books to e-readers.

This is an early but usable core. The Rust implementation is the main project in
this repository; the original Python calibre source lives in the `legacy/` git
submodule, untouched, as a reference and for the parts `cr` does not cover yet.

```text
.
├── Cargo.toml       workspace: cr-core + cr-cli
├── crates/
│   ├── cr-core/     library engine, formats, metadata, OPF, devices
│   ├── cr-cli/      `cr` command line interface
│   └── cr-gui/      GPUI desktop app (standalone package)
└── legacy/          git submodule: upstream calibre (Python)
```

Clone the Python source along with this project using
`git clone --recurse-submodules <url>`, or fetch it in an existing clone with
`git submodule update --init`.

## Why it is small and quick

`cr` is one native binary. There is no Python runtime, no Qt, no external
converter process and no background service. The only data store is SQLite
(compiled in), and books are read directly from their EPUB/FB2/TXT/HTML files.

| Measurement | Result |
|---|---|
| Release binary (Linux x86-64, LTO + strip) | **4.3 MB** |
| `cr info` startup / peak RSS (`/usr/bin/time`) | **< 10 ms** / **5.5 MB** |

These are local measurements of `cr`, not a benchmark against calibre (which is
not installed in this environment). A proper large-library benchmark is on the
roadmap.

## What works

### Library management

- Create and open **calibre-compatible** libraries (`metadata.db`, schema
  version 28, the same triggers and application id).
- Add books, remove books (optionally deleting files), list, search, and show
  library statistics.
- Export books with a path template.
- Book directories use calibre's layout: `Author Sort/Title (id)/`.
- Each book gets `metadata.opf` and, when available, `cover.jpg`.
- Format files use calibre's `Title - Author.ext` naming.

A library created by `cr` can be read by calibre itself, and `cr` can open an
existing calibre library. The embedded schema is copied from calibre's own
`resources/metadata_sqlite.sql`; calibre's SQLite helper functions (`title_sort`,
`uuid4`, `author_to_author_sort`, ...) are registered on every connection so its
triggers work unchanged.

### Metadata

Import from:

- **EPUB** — OPF metadata, cover images, series, tags, identifiers, languages.
- **FB2** — title/author/series/genre/annotation, embedded cover.
- **TXT** — filename-derived metadata.
- **HTML/XHTML** — `<title>`, author/language/keywords meta tags.
- Other files can be stored and managed; metadata falls back to the filename.

Read/write calibre's `metadata.opf` (v2.0). Editable fields:

`title`, `authors`, `tags`, `series` + `series_index`, `publisher`,
`rating` (0–10 or stars), `comments` (HTML), `identifiers`, `languages`,
`pubdate`, and `cover` (normalised to JPEG).

Editing metadata renames the author/title directory and format files, then
rewrites `metadata.opf`, exactly as calibre expects.

### Reading

`cr read` converts a book to terminal text:

- EPUB in **spine order** (XHTML → text, chapter headings detected).
- FB2 sections.
- TXT with chapter-heading heuristics.
- HTML.

`--list` shows chapters and word counts, `--chapter N` reads one, `--all` reads
everything, and `--width` controls wrapping.

### Editing

`cr edit` unpacks an EPUB to a directory, lets you change its XHTML/resources,
then repacks it with a spec-correct stored `mimetype` entry. Repacking replaces
the library copy by default (`--no-replace` to keep it separate). This is the
beginning of the "Edit book" workflow; calibre's full editor UI is not there yet.

### Devices

- Detects Kindle (`system/` + `documents/`), Kobo (`.kobo`) and generic USB
  readers with a `documents/` directory under common mount points.
- Sends the preferred format (EPUB, AZW3, MOBI, PDF, HTML, TXT, FB2) or a chosen
  one, avoiding filename collisions.
- Updates the device's `metadata.calibre` catalog with title, authors, series,
  tags, rating, identifiers, dates, format and size.
- `--dry-run` previews without writing.

### Desktop UI

`crates/cr-gui` is an early GPUI front end built with
[gpui-kit](https://github.com/longbridge/gpui-kit). It has:

- open/create a library, with a native folder picker;
- a searchable book list and cover/metadata detail pane;
- native file picking for adding books and choosing covers;
- metadata editing (title, authors, series, tags, rating, identifiers,
  languages, publication date and multi-line comments) with validation;
- cover replacement and interactive cover cropping: drag the crop box and
  corner handles, or use the 2:3, 1:1 and 3:4 presets;
- export the selected book, or the current filtered library, to a folder
  picker using the same path template as `cr export`;
- a basic EPUB source editor: unpack, edit XHTML/CSS/OPF files, save changes
  and repack into the library, with a save/discard prompt when closing;
- a chapter-by-chapter reader for EPUB, FB2, TXT and HTML;
- device discovery and one-click send;
- light/dark theme switching.

The GUI is a separate package (excluded from the root workspace) because it
pulls in GPUI and native windowing libraries. `cargo test --workspace` still
only builds and tests the engine and CLI.

## Build and install

Requires a recent stable Rust toolchain (1.80+). SQLite is bundled.

```bash
cargo test --workspace
cargo build --release

# either use it in place
./target/release/cr --version

# or install the `cr` binary
cargo install --path crates/cr-cli
```

### Desktop dependencies

On Debian/Ubuntu, GPUI needs X11/Wayland and font development packages:

```bash
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libfontconfig1-dev libfreetype-dev libxcb1-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libssl-dev

cd crates/cr-gui
cargo run
```

The window opens with `~/Calibre Library` if it exists. Open another library
with the path field, `Browse...`, or create one with `New`.

## Quick start

```bash
# Create a library (default is ~/Calibre Library)
cr init ~/Calibre\ Library

# Import books
cr add ~/Downloads/*.epub

# Browse and search
cr list
cr list "author:ursula tag:fantasy"
cr list --sort series --limit 20

# Inspect and edit metadata
cr show 12
cr set 12 --author "Ursula K. Le Guin" --tag "Fantasy,Classic" \
          --series "Earthsea" --series-index 2 --stars 5

# Read
cr read 12 --list
cr read 12 --chapter 3 --width 88

# Edit an EPUB
cr edit 12 --extract ./book-12
# ... edit ./book-12/OEBPS/... ...
cr edit 12 --pack ./book-12 --output ./book-12.epub

# Export and send
cr export 12 --to ~/exports
cr devices
cr send 12 --device /media/$USER/Kindle

# JSON for scripts
cr --json list
cr --json show 12
```

`-l/--library` (or `CR_LIBRARY`) selects the library for every command.
`--json` is accepted globally.

### Search syntax

| Query | Meaning |
|---|---|
| `dune` | title/author/tag contains "dune" |
| `title:hobbit` | title contains |
| `author:tolkien` | any author contains |
| `tag:fantasy` | any tag contains |
| `series:"earthsea"` | series contains |
| `publisher:ace` | publisher contains |
| `format:EPUB` | has that format (exact) |
| `language:en` | language code |
| `identifier:isbn:978...` | identifier type/value |
| `rating:8` | calibre rating 0–10 |
| `id:12` | database id |
| `!tag:romance` | negation |
| `sort:-series` | order by series descending |

Multiple terms are combined with AND.

## Architecture

```text
cr-cli  (clap)
  └── cr-core
        ├── library    SQLite schema v28, CRUD, search, covers, OPF sync
        ├── metadata   Metadata, BookSummary, dates, comments, sorting
        ├── text       title_sort / author_sort / ASCII filenames / paths
        ├── opf        calibre metadata.opf reader + writer
        ├── xml        tiny namespace-insensitive tree parser
        ├── formats
        │   ├── epub   OPF + cover + spine text + unpack/repack
        │   ├── fb2    FictionBook metadata, cover, sections
        │   ├── html   HTML → text, headings, meta tags
        │   └── txt    encoding detection, chapter heuristics
        └── device     mount detection, copy + metadata.calibre
```

The engine API is usable independently of the CLI:

```rust
use cr_core::{Library, ListOptions};

let mut library = Library::open_or_create("Calibre Library".as_ref())?;
let book = library.add_book("book.epub".as_ref())?;
println!("{}", book.title);
for summary in library.list_books(&ListOptions::default())? {
    println!("{}: {}", summary.id, summary.title);
}
```

## Tests

`cargo test --workspace` runs **43 tests**:

- sorting, filenames and path conventions;
- XML/OPF parsing and round-tripping;
- EPUB, FB2, TXT and HTML metadata/text;
- a full library lifecycle: create a calibre v28 DB, import an EPUB with a PNG
  cover, search, edit metadata (directory + OPF rename), unpack/edit/repack the
  EPUB, export, send to a fake Kindle and remove;
- reopen a library in a new connection;
- CLI end-to-end: init, add, list, search, set, show, read, formats, export,
  devices, send, info and rm.

## Not there yet

The engine covers the core; the surrounding calibre product is much larger.
Currently missing:

- **GUI polish** — the GPUI front end has a basic text-based EPUB editor, but
  no visual editor, bulk metadata actions or cover-generation tools yet.
- **Format conversion** (EPUB ↔ AZW3/MOBI, PDF, DOCX, ...).
- PDF/MOBI/AZW3 metadata and text extraction (they can be stored and sent, but
  import falls back to filename metadata).
- calibre's plugin system, content server/OPDS, annotations, full-text search,
  custom columns, virtual libraries, saved searches and the complete search
  language.
- Full ebook editor (only EPUB unpack/repack today), cover cropping and
  thumbnail generation, DRM.
- Opening libraries older than schema 28 without an upgrade step.

## Roadmap

1. Polish the GPUI app: visual EPUB editing, bulk actions and a richer crop
   surface with drag handles.
2. Native EPUB ↔ AZW3/MOBI conversion and PDF/DOCX text extraction.
3. Calibre custom columns and saved searches.
4. OPDS/content server and annotation sync.
5. Benchmarks against calibre on a 10k-book library, with a compatibility test
   suite that opens real calibre libraries.

## License

GPL-3.0-or-later, matching calibre. The SQLite schema is derived from calibre's
`resources/metadata_sqlite.sql`.
