//! Reproducible library benchmark.
//!
//!     cargo run --release -p cr-core --example bench -- 10000
//!
//! Builds a deterministic N-book library in a temporary directory (or reuses
//! the one named by `CR_BENCH_DIR`) and times ingestion, opening, listing,
//! searching and editing. Prints one Markdown table row per measurement.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cr_core::jobs::JobContext;
use cr_core::library::{BookSort, BookUpdate, Library, ListOptions};

const AUTHORS: [&str; 8] = [
    "Ursula K. Le Guin",
    "Frank Herbert",
    "Octavia E. Butler",
    "Stanisław Lem",
    "Iain M. Banks",
    "N. K. Jemisin",
    "Arkady Strugatsky",
    "Ted Chiang",
];
const TAGS: [&str; 6] = ["sf", "fantasy", "classic", "essay", "short", "history"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args()
        .nth(1)
        .map(|n| n.parse())
        .transpose()?
        .unwrap_or(1000);
    let temporary = tempfile::tempdir()?;
    let base = std::env::var_os("CR_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_path_buf());
    let root = base.join(format!("library-{count}"));

    println!("| Measurement ({count} books) | Result |");
    println!("|---|---|");

    if !root.join("metadata.db").is_file() {
        let sources = write_sources(&base.join("sources"), count)?;
        let mut library = Library::create(&root)?;
        let started = Instant::now();
        let results = library.add_books(&sources, &JobContext::detached());
        let elapsed = started.elapsed();
        let failed = results.iter().filter(|r| r.result.is_err()).count();
        assert_eq!(failed, 0, "{failed} books failed to import");
        row(
            "Ingest",
            format!(
                "{} ({:.0} books/s)",
                show(elapsed),
                count as f64 / elapsed.as_secs_f64()
            ),
        );
        let started = Instant::now();
        for (index, id) in library.all_book_ids()?.into_iter().enumerate() {
            let patch = BookUpdate {
                tags: Some(vec![
                    TAGS[index % TAGS.len()].to_string(),
                    TAGS[(index / 7) % TAGS.len()].to_string(),
                ]),
                series: Some((index % 5 == 0).then(|| format!("Series {}", index % 97))),
                rating: Some(Some((index % 10 + 1) as u8)),
                ..Default::default()
            };
            library.update_book(id, &patch)?;
        }
        row(
            "Edit metadata, per book",
            show(started.elapsed() / count as u32),
        );
    }

    row(
        "Open library (cold)",
        show(time(1, || Library::open(&root).map(drop))?),
    );
    let library = Library::open(&root)?;
    let list = |query: Option<&str>, sort: BookSort| {
        let options = ListOptions {
            query: query.map(str::to_string),
            sort: Some(sort),
            ..Default::default()
        };
        let library = &library;
        move || library.list_books(&options).map(drop)
    };
    row(
        "Open library (warm)",
        show(time(20, || Library::open(&root).map(drop))?),
    );
    row(
        "List all, by title",
        show(time(5, list(None, BookSort::Title))?),
    );
    row(
        "List all, by author",
        show(time(5, list(None, BookSort::Author))?),
    );
    row(
        "Search `herbert`",
        show(time(5, list(Some("herbert"), BookSort::Title))?),
    );
    row(
        "Search `tag:sf author:lem -series:7`",
        show(time(
            5,
            list(Some("tag:sf author:lem -series:7"), BookSort::Title),
        )?),
    );
    row(
        "Library statistics",
        show(time(5, || library.stats().map(drop))?),
    );
    if let Some(peak) = peak_rss_kb() {
        row("Peak RSS", format!("{:.1} MB", peak as f64 / 1024.0));
    }
    Ok(())
}

fn write_sources(directory: &Path, count: usize) -> std::io::Result<Vec<PathBuf>> {
    fs::create_dir_all(directory)?;
    (0..count)
        .map(|index| {
            let author = AUTHORS[index % AUTHORS.len()];
            let path = directory.join(format!("book-{index:06}.html"));
            fs::write(
                &path,
                format!(
                    "<html><head><title>Volume {index} of the Archive</title>\
                     <meta name=\"author\" content=\"{author}\"></head>\
                     <body><h1>Chapter 1</h1><p>Text of book {index}.</p></body></html>"
                ),
            )?;
            Ok(path)
        })
        .collect()
}

/// Mean wall time of `runs` calls.
fn time(runs: u32, mut body: impl FnMut() -> cr_core::Result<()>) -> cr_core::Result<Duration> {
    let started = Instant::now();
    for _ in 0..runs {
        body()?;
    }
    Ok(started.elapsed() / runs)
}

fn show(duration: Duration) -> String {
    let micros = duration.as_secs_f64() * 1e6;
    if micros < 1000.0 {
        format!("{micros:.0} µs")
    } else if micros < 1e6 {
        format!("{:.1} ms", micros / 1e3)
    } else {
        format!("{:.2} s", micros / 1e6)
    }
}

fn row(name: &str, value: String) {
    println!("| {name} | {value} |");
}

fn peak_rss_kb() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
