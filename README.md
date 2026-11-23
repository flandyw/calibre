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
(compiled in), and books are read directly from their EPUB, Kindle, FB2, TXT or
HTML files.

| Measurement | Result |
|---|---|
| Release binary (Linux x86-64, LTO + strip) | **4.3 MB** |
| `cr info` startup / peak RSS (`/usr/bin/time`) | **< 10 ms** / **5.5 MB** |

These local measurements predate Kindle conversion support. A large-library
benchmark is on the roadmap.

## What works

### Library management

- Create and open **calibre-compatible** libraries (`metadata.db`, schema
  version 28, the same triggers and application id).
- Add books, list, search, and show library statistics.
- Import folders recursively, read metadata from file names with a pattern,
  create empty records, and choose what happens to duplicates: add, skip, or
  merge the file into the existing book as another format.
- Add, replace and remove individual formats of a book.
- Removed books and formats go to calibre's `.caltrash` folder and can be
  restored (with the same id) or deleted for good.
- Bulk metadata edits: add/remove/clear tags, set authors, publisher, series
  and rating, number a series, search and replace (plain or regex), copy one
  field into another, with a dry-run preview.
- Copy or move books between libraries with their formats, cover, metadata
  and extra `data/` files.
- Export books with a path template, selected formats, optional cover and
  OPF, and a rename/overwrite/skip rule for existing files.
- Book directories use calibre's layout: `First Author/Title (id)/`.
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
- **AZW3, AZW, MOBI and PRC** — embedded metadata and cover images when available.
- **FB2** — title/author/series/genre/annotation, embedded cover.
- **TXT** — filename-derived metadata.
- **HTML/XHTML** — `<title>`, author/language/keywords meta tags.
- **DOCX** — document properties (title, author, keywords, description).
- **RTF** — the `\info` group.
- **PDF** — the document information dictionary; the first full-page image,
  when there is one, becomes the cover.
- **CBZ/CBR** — `ComicInfo.xml` (series, number, writer, publisher, genre);
  the first page becomes the cover. A CBR must be a ZIP archive inside: RAR
  archives are stored but not read.
- **KEPUB** (`.kepub.epub`, `.kepub`) — as EPUB.
- Other files can be stored and managed; metadata falls back to the filename.

**Custom columns** of every calibre type except composite are read, written
and searchable, in calibre's own tables.

Read/write calibre's `metadata.opf` (v2.0). Editable fields:

`title`, `authors`, `tags`, `series` + `series_index`, `publisher`,
`rating` (0–10 or stars), `comments` (HTML), `identifiers`, `languages`,
`pubdate`, and `cover` (normalised to JPEG).

Editing metadata renames the author/title directory and format files, then
rewrites `metadata.opf`, exactly as calibre expects.

### Reading

`cr read` converts a book to terminal text:

- EPUB in **spine order** (XHTML → text, chapter headings detected).
- AZW3, AZW, MOBI and PRC, including KF8 chapters in reading order.
- FB2 sections.
- TXT with chapter-heading heuristics.
- HTML.

`--list` shows chapters and word counts, `--chapter N` reads one, `--all` reads
everything, and `--width` controls wrapping.

The GUI reader lays the book out from its own markup instead of extracted
text: headings, bold/italic/underline/code, lists, quotations, tables,
preformatted text, images and links (links inside the book jump, others open
in the browser), for every format that converts (EPUB, KEPUB, Kindle, FB2,
DOCX, HTML, RTF, TXT, PDF, and comics as one page image per section). The side
panel shows the book's real table of contents (EPUB 3 navigation document or
NCX, nested), search results, and bookmarks and highlights.

The book's stylesheet is followed for what a reflowing reader can honour:
alignment, text size, bold/italic/underline/strike, super- and subscript,
paragraph spacing, left margins, hidden elements (`display: none`) and
embedded TrueType/OpenType fonts (including fonts obfuscated with the EPUB or
Adobe method, which are keyed to the book's identifier and are not DRM).
Selectors by element, class, ID and ancestry are understood; colours,
floats, first-line indents and paged mode are not applied. `cr serve` gives
the browser reader the same stylesheet and fonts.

A PDF is read as reflowed text with its images and outline, not as fixed
pages: columns, tables and scanned (image-only) pages do not come through
well, and encrypted files are refused.

- **Position**: the place is saved when you move or close the book and
  restored on reopening; a book last read in calibre's viewer resumes at
  calibre's recorded progress.
- **Search**: `cr read ID --search TEXT` and `--toc` on the command line;
  Find in the GUI.
- **Bookmarks and highlights**: `cr annotations add --book ID --find TEXT
  [--bookmark] [--note ...]`, `list`, `search`, `note`, `rm`, and
  `export --format markdown|json`. A highlight covers text within one
  paragraph; the GUI highlights the clicked paragraph.
- **Storage**: reading state lives in `cr-reading.db` beside `metadata.db`.
  Book files and calibre's tables are never written. Highlights are anchored
  by their text, so they follow it when the book file is edited.
- **calibre's annotations**: `cr annotations list --book ID` also shows
  bookmarks and highlights made in calibre's viewer, and
  `cr annotations import-calibre --book ID` copies its highlights in.
  Nothing is written back to calibre.

### Editing EPUB, KEPUB and AZW3 books

`cr epub ACTION TARGET` works on a library book ID, an `.epub`, `.kepub.epub`
or `.azw3` file, or an unpacked directory (`cr edit ID --extract DIR`). A
library book is edited in the first of EPUB, KEPUB and AZW3 it has. An AZW3
book is opened through its EPUB form and rebuilt as AZW3 when saved:

- `check` reports missing or unlisted files, bad media types, markup that is
  not well-formed (with the line), broken links, images and anchors, spine
  and cover problems and a missing table of contents. `fix` repairs the ones
  that need no judgement. `check` exits non-zero when there are errors.
- `search PATTERN` and `replace PATTERN REPLACEMENT` across markup, styles
  and the package document, with `--regex`, `--case` and `--file`.
- `toc` shows the table of contents; `toc --generate --depth N` rebuilds the
  navigation document and/or NCX from the headings.
- `files`, `add SOURCE PATH`, `rm PATH`, `mv FROM TO` (every reference to the
  file is updated) and `spine PATH POSITION` manage files and reading order.
- `compress` re-encodes and scales down JPEG and PNG images.
- `diff` lists what changed (two EPUB files, or a library book against its
  kept original); `restore ID` puts the original back.

Saving to a library book checks it first and refuses a book with errors
(`--force` overrides), writes the file atomically, does nothing when the
content is unchanged, and keeps the first replaced file as
`ORIGINAL_<FORMAT>` (`ORIGINAL_EPUB`, `ORIGINAL_KEPUB`, `ORIGINAL_AZW3`).
The GUI editor's Check Book and Check & Save to Library buttons use the same
checks and save path. Not included: live preview, spell check, HTML tidy,
font subsetting and EPUBCheck.

### Conversion

`cr convert INPUT OUTPUT` selects the target by the output extension.

| From | To |
|---|---|
| EPUB, KEPUB, AZW3, AZW, MOBI, PRC, FB2, DOCX, HTML, RTF, TXT, PDF, CBZ, CBR | EPUB, AZW3, MOBI, HTMLZ, TXT |

EPUB to AZW3/MOBI uses [Kindling](https://github.com/ciscoriordan/kindling) and
Kindle to EPUB uses bundled [libmobi](https://github.com/bfabiszewski/libmobi);
both keep the book's own files, including styles and fonts. Every other pair
goes through one document model (sections of XHTML, images, a stylesheet and
metadata): the source is read into it and the target is written from it. That
path keeps headings, paragraphs, inline formatting, lists, tables, images,
links, footnote anchors, the source's CSS and the fonts it embeds; it drops
scripts, forms and media. A PDF is read as reflowed text (see Reading); a
comic becomes one page image per section. The EPUB it writes has a generated table of contents and a
cover page. Kindle reading and conversion require DRM-free files.

- `--no-split` keeps the source as one section. By default a new section
  starts at `h1`/`h2` headings (HTML), `Heading 1` (DOCX), `<section>` (FB2)
  and "Chapter ..." lines (TXT).
- `--profile kindle|kobo|tablet` scales images down to that screen.

`INPUT` can be a filename or a library book ID. With a book ID, use
`--from FORMAT` to choose the source format and `--add` to store the result on
the same book. The output path must be new. `--add` replaces the stored copy of
the target format if the book already has one.

`cr convert --to FORMAT ID... [--search QUERY]` converts library books in one
run and stores the result on each book, with the library's metadata and cover.
A book that already has the format is skipped; `--overwrite` replaces it and
keeps the old file as `ORIGINAL_<FORMAT>`, as calibre does. One book failing
does not stop the others.

Not converted yet: RAR-compressed comics (repack as CBZ), ODT, LIT and other
legacy formats; Kindle to Kindle directly (go through EPUB). There is no PDF,
KEPUB or comic output.

### Editing

`cr edit` unpacks a book to a directory, lets you change its XHTML/resources,
then repacks it with a spec-correct stored `mimetype` entry. `--format` picks
EPUB (the default), KEPUB or AZW3. Repacking replaces the library copy by
default (`--no-replace` to keep it separate). This is the beginning of the
"Edit book" workflow; calibre's full editor UI is not there yet.

### Devices

- Detects Kindle (`system/` + `documents/`), Kobo (`.kobo`) and generic USB
  readers with a `documents/` directory under common mount points.
- Detects MTP Kindles, including recent Paperwhite, Scribe and Colorsoft models,
  over USB on macOS and Linux. Enable USB file transfer on the Kindle, then
  click **Devices** in the desktop app or run `cr devices`.
- Sends books to the device without overwriting existing files. A Kindle gets
  AZW3, AZW, MOBI, PRC, PDF or TXT; other readers prefer EPUB. A book with no
  format the device reads is converted for the send (EPUB to AZW3 for a
  Kindle), read back to check it, and only then copied; the library is not
  changed. `--no-convert` skips such books instead.
- Mounted devices: free space is checked before anything is written, each file
  is copied under a temporary name and renamed once its size is verified, and
  `--template "{author}/{title}"` sets folders and file names (`{series}`,
  `{series_index}` and `{id}` also work).
- `cr device books` lists what is on the device, matched to the library, as
  in sync, changed (the library file differs), other format (a converted copy)
  or not in library. `cr device rm PATH...` deletes device copies, with a
  Kindle's `.sdr` sidecar folder.
- `cr device status` reads progress and highlights the device records: Kobo's
  database (read from a copy, never opened in place) or a mounted Kindle's
  `My Clippings.txt`. A Kindle over MTP does not expose these.
- Updates mounted devices' `metadata.calibre` catalog with title, authors, series,
  tags, rating, identifiers, dates, format and size.
- `--dry-run` previews without writing.

### Content server

- `cr serve` serves the library read-only over HTTP (default
  `127.0.0.1:8080`, change with `--bind`): browse, search with the same
  search syntax, sort, book details, covers and downloads.
- `/opds` is an OPDS catalog (navigation, paged acquisition feeds and
  OpenSearch) for reading apps such as KOReader.
- **Read** opens EPUB, Kindle, FB2, DOCX, HTML, RTF and TXT books in the
  browser as one page; the browser remembers the position per book. Scripts
  in a book are blocked by the page's content security policy.
- `--user NAME` with the password in `CR_SERVER_PASSWORD` requires HTTP Basic
  authentication. The server speaks plain HTTP: to reach it from another
  machine, put it behind a reverse proxy that adds TLS.
- It uses only the standard library's sockets, one thread per connection, up
  to 32 at a time.

### Catalogs and maintenance

- `cr catalog books.csv|.json|.xml|.bib|.epub [SEARCH]` writes a book list as
  CSV, JSON, XML, BibTeX or an EPUB grouped by author.
- `cr check` compares every book's record with its folder: missing folders,
  format files and covers, wrong sizes, book files and covers the database
  does not list, missing `metadata.opf`, and folders that belong to no book.
  `--repair` fixes all but missing and unknown folders; it never deletes a
  book file.
- `cr restore-db` rebuilds a lost or damaged `metadata.db` from the
  `metadata.opf` in each book folder, keeping ids and uuids. The old database
  is kept as `metadata_pre_restore.db`. Custom columns, saved searches and
  virtual libraries are not in those files and are not restored.

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
- multi-selection (Ctrl/Cmd-click, Shift-click, select all) with batch
  remove, export and send;
- adding a file whose title and authors match an existing book attaches it
  to that book as another format;
- export the selected books, or the current filtered library, to a folder
  picker using the same path template as `cr export`;
- a basic EPUB source editor: unpack, edit XHTML/CSS/OPF files, save changes
  and repack into the library, with a save/discard prompt when closing;
- a chapter-by-chapter reader for EPUB, Kindle books, FB2, TXT and HTML;
- EPUB-to-AZW3 and Kindle-to-EPUB conversion from the selected book's Formats
  row, with a save dialog;
- device discovery and one-click send;
- light/dark theme switching.

The GUI is a separate package (excluded from the root workspace) because it
pulls in GPUI and native windowing libraries. `cargo test --workspace` still
only builds and tests the engine and CLI.

## Build and install

Requires a recent stable Rust toolchain (1.90+) and a C compiler. SQLite and
libmobi are bundled.

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

The window reopens the last library you used, or `~/Calibre Library` if it
exists. Use File → Open Library (⇧⌘O) or New Library (⇧⌘N) for others, or
drop a library folder onto the window. Press ⌘/ for every keyboard shortcut.

### macOS app bundle

```bash
crates/cr-gui/packaging/macos/bundle.sh          # release build
crates/cr-gui/packaging/macos/bundle.sh --open   # build, then launch
```

This writes `crates/cr-gui/target/release/bundle/calibre-rust.app`, with the
app icon and file associations ("Open With" for ebooks, and for folders so a
calibre library can be opened from Finder). It is signed ad hoc, which is
enough to run it on the Mac that built it; copy it to `/Applications` to keep
it. Sharing it with other Macs needs a Developer ID signature and
notarization. The icon is drawn by `packaging/macos/make-icon.swift`; pass
`--redraw-icon` to regenerate `icon-1024.png` after editing it.

## Quick start

```bash
# Create a library (default is ~/Calibre Library)
cr init ~/Calibre\ Library

# Import books
cr add ~/Downloads/*.epub
cr add -r ~/Books --duplicates merge     # EPUB + AZW3 of one book -> one record
cr add ~/scans --filename-pattern '(?P<author>.+) - (?P<title>.+)'
cr add-format 12 book.azw3
cr remove-format 12 azw3

# Browse and search
cr list
cr list "author:ursula tag:fantasy"
cr list --sort series --limit 20

# Inspect and edit metadata
cr show 12
cr set 12 --author "Ursula K. Le Guin" --tag "Fantasy,Classic" \
          --series "Earthsea" --series-index 2 --stars 5

# Change many books at once
cr bulk 3 4 5 --series "Dune" --series-start 1 --add-tag sf
cr bulk --query "tag:scifi" --remove-tag scifi --add-tag sf --dry-run
cr bulk 7 8 --field title --search '^The ' --replace '' --regex

# Remove, restore, and move between libraries
cr rm 12                 # to the trash; --permanent deletes
cr trash
cr trash restore 12
cr copy 3 4 --to ~/Other\ Library --move

# Read
cr read 12 --list
cr read 12 --chapter 3 --width 88
cr read 12 --toc
cr read 12 --search "white whale"
cr annotations add --book 12 --find "Call me Ishmael" --note "opening"
cr annotations export --book 12 --format markdown

# Check and edit an EPUB in the library
cr epub check 12
cr epub replace 12 "colour" "color"
cr epub toc 12 --generate --depth 2
cr epub diff 12
cr epub restore 12

# Convert and optionally store the result on an existing library book
cr convert book.epub book.azw3
cr convert book.azw3 book.txt
cr convert 12 book-for-kindle.azw3 --add
cr convert notes.docx notes.epub --profile kindle
cr convert --to azw3 --search "tag:sf format:epub"

# Edit an EPUB
cr edit 12 --extract ./book-12
# ... edit ./book-12/OEBPS/... ...
cr edit 12 --pack ./book-12 --output ./book-12.epub

# Export and send
cr export 12 --to ~/exports --format epub --cover --opf --collision skip
cr serve --bind 127.0.0.1:8080
CR_SERVER_PASSWORD=secret cr serve --bind 0.0.0.0:8080 --user ann
cr catalog books.bib
cr check --repair
cr devices
cr send 12 --device /media/$USER/Kindle
cr device books
cr device rm "documents/Old Book - Author.azw3"
cr device status
# With one connected device, the mount path or MTP identifier is optional.
cr send 12

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
| `rating:>=4` | at least four stars (`rating:false`: unrated) |
| `id:12` | database id |
| `tag:=sf` | exact match instead of "contains" |
| `title:~^the` | regular expression |
| `series:true`, `cover:false` | field is set / empty |
| `pubdate:>=2020-06`, `date:today`, `date:30daysago` | date comparisons |
| `size:>2M` | largest format bigger than 2 MB |
| `#pages:>300`, `#genre:=noir` | custom columns |
| `search:name`, `vl:name` | a saved search / virtual library |
| `!tag:romance`, `not tag:romance` | negation |
| `tag:a or (tag:b and not tag:c)` | OR and grouping |
| `sort:-series` | order by series descending |

Terms are combined with AND unless `or` is given. This follows calibre's
search language; ratings are in stars, as in calibre.

### Organising

```sh
cr searches save unread-sf "tag:sf and #read:false"   # then: cr list search:unread-sf
cr searches --virtual save Comics "format:cbz"        # then: cr list --vl Comics
cr categories tags                                    # every tag with its book count
cr categories tags --rename scifi sf                  # rename, or merge into an existing tag
cr columns add pages --type int                       # custom column
cr set-custom 12 pages 412
cr fts index && cr fts search "spice must flow"       # search inside books
cr catalog library.csv "tag:sf"                       # CSV or JSON catalog
```

Saved searches, virtual libraries and custom columns are stored where calibre
stores them, so both programs share them. The full-text index is a separate
file (`cr-full-text.db`) that can be deleted at any time; `cr fts index` only
re-reads books whose files changed.

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
        │   ├── mobi   Kindle metadata, cover, and chapter reconstruction
        │   └── txt    encoding detection, chapter heuristics
        └── device     mount and Kindle MTP detection, book transfer
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

`cargo test --workspace` covers:

- sorting, filenames and path conventions;
- XML/OPF parsing and round-tripping;
- EPUB, Kindle, FB2, TXT and HTML metadata/text, plus Kindle conversion
  round trips;
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
  no visual editor, bulk metadata dialog, trash browser or cover-generation
  tools yet (bulk edits, trash and library transfers are CLI-only).
- The desktop app does not yet show custom columns, the category browser,
  saved searches or full-text results (its search box does accept the whole
  search language); these are CLI-only.
- Format conversion outside the EPUB and Kindle paths above.
- Page-faithful PDF viewing, PDF metadata stored in compressed object streams
  or XMP, and RAR/7z comic archives (CBR/CB7).
- Downloading metadata and covers from online sources.
- Notes on authors/series/tags, composite (template) custom columns, and
  custom values in `metadata.opf` (calibre rewrites them there itself).
- calibre's plugin system, content server/OPDS and annotations.
- Live preview and spell check in the editor, cover cropping and
  thumbnail generation, DRM.
- Opening libraries older than schema 28 without an upgrade step.

## Roadmap

1. Polish the GPUI app: visual EPUB editing, bulk actions and a richer crop
   surface with drag handles.
2. Page-faithful PDF viewing.
3. Calibre custom columns and saved searches.
4. OPDS/content server and annotation sync.
5. Benchmarks against calibre on a 10k-book library, with a compatibility test
   suite that opens real calibre libraries.

## License

GPL-3.0-or-later, matching calibre. The SQLite schema is derived from calibre's
`resources/metadata_sqlite.sql`.
