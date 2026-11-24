//! Background full-text indexing and transfers between calibre libraries.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use cr_core::formats::reading::{Book, Locator};
use cr_core::library::{AddOutcome, CatalogFormat, FtsHit, FtsReport, Library};
use gpui_kit::component::input::InputState;
use gpui_kit::*;

use crate::app::{
    default_library_path, display_name, set_input, text_input, CalibreApp, Shelf, Tone,
};
use crate::settings::ImportDuplicates;

const HIT_LIMIT: usize = 100;

pub const CATALOG_FORMATS: [(&str, &str); 6] = [
    ("CSV", "csv"),
    ("EPUB", "epub"),
    ("HTML", "html"),
    ("JSON", "json"),
    ("XML", "xml"),
    ("BibTeX", "bib"),
];

pub struct LibraryTools {
    pub text_query: Entity<InputState>,
    pub text_hits: Option<Vec<FtsHit>>,
    pub index_report: Option<FtsReport>,
    pub transfer_target: Option<PathBuf>,
    pub transfer_duplicates: ImportDuplicates,
    pub transfer_report: Option<TransferReport>,
    pub catalog_query: Entity<InputState>,
    pub catalog_format: usize,
    pub catalog_output: Option<PathBuf>,
}

pub struct TransferReport {
    pub moved: bool,
    pub written: usize,
    pub merged: usize,
    pub skipped: usize,
    pub failures: Vec<String>,
    pub cancelled: bool,
}

impl LibraryTools {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        Self {
            text_query: text_input("Words or an exact phrase", window, cx),
            text_hits: None,
            index_report: None,
            transfer_target: None,
            transfer_duplicates: ImportDuplicates::Skip,
            transfer_report: None,
            catalog_query: text_input("Leave blank to export the entire library", window, cx),
            catalog_format: 0,
            catalog_output: None,
        }
    }

    pub fn clear_results(&mut self) {
        self.text_hits = None;
        self.index_report = None;
        self.transfer_target = None;
        self.transfer_report = None;
        self.catalog_output = None;
    }
}

impl CalibreApp {
    pub fn use_library_query_for_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        set_input(
            &self.library_tools.catalog_query,
            self.query.clone(),
            window,
            cx,
        );
    }

    pub fn prompt_export_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self
            .library
            .as_ref()
            .map(|library| library.root().to_path_buf())
        else {
            return;
        };
        let query = self
            .library_tools
            .catalog_query
            .read(cx)
            .value()
            .trim()
            .to_string();
        let (_, extension) = CATALOG_FORMATS[self.library_tools.catalog_format];
        let default = default_library_path();
        let home = default.parent().unwrap_or_else(|| Path::new("."));
        let receiver = cx.prompt_for_new_path(home, Some(&format!("Library Catalog.{extension}")));
        cx.spawn_in(window, async move |app, cx| {
            let Ok(Ok(Some(mut output))) = receiver.await else {
                return;
            };
            let replace = output.extension().is_some();
            if !replace {
                output.set_extension(extension);
            }
            let _ = app.update_in(cx, |app, window, cx| {
                if !app
                    .library
                    .as_ref()
                    .is_some_and(|library| library.root() == root)
                {
                    return;
                }
                if !output
                    .extension()
                    .and_then(|suffix| suffix.to_str())
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
                {
                    app.announce(
                        format!("Choose a .{extension} filename."),
                        Tone::Error,
                        window,
                        cx,
                    );
                    return;
                }
                app.run_job(
                    "Exporting library catalog",
                    window,
                    cx,
                    move |_| {
                        write_catalog(&Library::open(&root)?, &query, extension, &output, replace)?;
                        Ok(output)
                    },
                    |app, result, window, cx| match result {
                        Ok(output) => {
                            app.announce(
                                format!("Saved {}.", display_name(&output)),
                                Tone::Success,
                                window,
                                cx,
                            );
                            app.library_tools.catalog_output = Some(output);
                        }
                        Err(err) => app.announce(
                            format!("Could not export the catalog: {err}"),
                            Tone::Error,
                            window,
                            cx,
                        ),
                    },
                );
            });
        })
        .detach();
    }

    pub fn index_book_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        self.run_job(
            "Indexing book contents",
            window,
            cx,
            move |context| Library::open(&root)?.fts_index(context),
            |app, result, window, cx| match result {
                Ok(report) => {
                    let message = if report.cancelled {
                        "Indexing stopped. Run Update Index to continue."
                    } else {
                        "Full-text index updated."
                    };
                    app.library_tools.index_report = Some(report);
                    app.library_tools.text_hits = None;
                    app.set_status(message, Tone::Info);
                }
                Err(err) => {
                    app.announce(format!("Indexing failed: {err}"), Tone::Error, window, cx)
                }
            },
        );
    }

    pub fn search_book_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self
            .library
            .as_ref()
            .filter(|library| library.has_fts_index())
            .map(|library| library.root().to_path_buf())
        else {
            return;
        };
        let query = self
            .library_tools
            .text_query
            .read(cx)
            .value()
            .trim()
            .to_string();
        if query.is_empty() {
            return;
        }
        self.run_job(
            "Searching book contents",
            window,
            cx,
            move |_| Library::open(&root)?.fts_search(&query, HIT_LIMIT),
            |app, result, window, cx| match result {
                Ok(hits) => {
                    app.set_status(
                        if hits.len() == HIT_LIMIT {
                            format!("Showing the best {HIT_LIMIT} results.")
                        } else {
                            format!("{} chapter(s) matched.", hits.len())
                        },
                        Tone::Info,
                    );
                    app.library_tools.text_hits = Some(hits);
                }
                Err(err) => app.announce(format!("Search failed: {err}"), Tone::Error, window, cx),
            },
        );
    }

    pub fn show_text_hit_in_library(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tools(window, cx);
        self.select_shelf(Shelf::All, window, cx);
        self.set_query(format!("id:{id}"), window, cx);
        if let Some(index) = self.books.iter().position(|book| book.id == id) {
            self.click_index(index, false, false, window, cx);
        }
    }

    pub fn clear_text_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.library_tools.text_hits = None;
        set_input(&self.library_tools.text_query, String::new(), window, cx);
        cx.notify();
    }

    pub fn choose_transfer_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self
            .library
            .as_ref()
            .map(|library| library.root().to_path_buf())
        else {
            return;
        };
        let response = cx.prompt_for_paths(PathPromptOptions {
            directories: true,
            files: false,
            multiple: false,
            prompt: Some("Choose Destination Library".into()),
        });
        cx.spawn_in(window, async move |app, cx| {
            let Ok(Ok(Some(paths))) = response.await else {
                return;
            };
            let Some(target) = paths.into_iter().next() else {
                return;
            };
            let _ = app.update_in(cx, |app, window, cx| {
                if app
                    .library
                    .as_ref()
                    .is_some_and(|library| library.root() == source)
                {
                    app.set_transfer_target(target, window, cx);
                }
            });
        })
        .detach();
    }

    pub fn set_transfer_target(
        &mut self,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = &self.library else { return };
        let validation = (|| {
            let target = target.canonicalize()?;
            let source = library.root().canonicalize()?;
            if source == target {
                return Err(cr_core::Error::Other(
                    "Choose a different calibre library.".into(),
                ));
            }
            let destination = Library::open(&target)?;
            if let Some(reason) = destination.read_only_reason() {
                return Err(cr_core::Error::Other(reason.to_string()));
            }
            Ok(target)
        })();
        match validation {
            Ok(target) => {
                self.library_tools.transfer_target = Some(target);
                self.library_tools.transfer_report = None;
                cx.notify();
            }
            Err(err) => self.announce(
                format!("Cannot use this destination: {err}"),
                Tone::Error,
                window,
                cx,
            ),
        }
    }

    pub fn start_transfer(
        &mut self,
        move_books: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.library_tools.transfer_target.clone() else {
            return;
        };
        let ids = self.selection_ids();
        if ids.is_empty() {
            return;
        }
        let Some(source) = self
            .library
            .as_ref()
            .map(|library| library.root().to_path_buf())
        else {
            return;
        };
        let duplicates = self.library_tools.transfer_duplicates;
        if !move_books {
            self.transfer_selection(source, target, ids, duplicates, false, window, cx);
            return;
        }
        if self.writable_root(window, cx).is_none() || duplicates == ImportDuplicates::Merge {
            return;
        }
        let question = format!("Move {} selected book(s)?", ids.len());
        let response = window.prompt(
            PromptLevel::Warning,
            &question,
            Some("Successfully transferred books go to the source library’s Trash."),
            &["Move to Library", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |app, cx| {
            if let Ok(0) = response.await {
                let _ = app.update_in(cx, |app, window, cx| {
                    if app
                        .library
                        .as_ref()
                        .is_some_and(|library| library.root() == source)
                    {
                        app.transfer_selection(source, target, ids, duplicates, true, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn transfer_selection(
        &mut self,
        source: PathBuf,
        target: PathBuf,
        ids: Vec<i64>,
        duplicates: ImportDuplicates,
        move_books: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = if move_books {
            "Moving books"
        } else {
            "Copying books"
        };
        self.run_job(
            label,
            window,
            cx,
            move |context| {
                let mut source = Library::open(&source)?;
                let mut target = Library::open(&target)?;
                let outcomes = source.transfer_books(
                    &ids,
                    &mut target,
                    duplicates.into(),
                    move_books,
                    context,
                );
                let mut report = TransferReport {
                    moved: move_books,
                    written: 0,
                    merged: 0,
                    skipped: 0,
                    failures: Vec::new(),
                    cancelled: outcomes.len() < ids.len() && context.is_cancelled(),
                };
                for item in outcomes {
                    match item.result {
                        Ok(AddOutcome::Added(_)) => report.written += 1,
                        Ok(AddOutcome::Merged { .. }) => report.merged += 1,
                        Ok(AddOutcome::Skipped { .. }) => report.skipped += 1,
                        Err(err) => report.failures.push(format!("Book #{}: {err}", item.item)),
                    }
                }
                Ok(report)
            },
            |app, result, window, cx| match result {
                Ok(report) => {
                    let tone = if report.failures.is_empty() {
                        Tone::Info
                    } else {
                        Tone::Error
                    };
                    app.set_status(
                        format!(
                            "Transfer finished. {} added, {} merged, {} skipped, {} failed.",
                            report.written,
                            report.merged,
                            report.skipped,
                            report.failures.len()
                        ),
                        tone,
                    );
                    if report.moved {
                        app.refresh_after_book_change(window, cx);
                    }
                    app.library_tools.transfer_report = Some(report);
                }
                Err(err) => {
                    app.announce(format!("Transfer failed: {err}"), Tone::Error, window, cx)
                }
            },
        );
    }
}

/// The text index can omit image-only pages or split text differently from the
/// reader. Prefer a matching passage, then a uniquely named reading section.
pub fn locate_text_hit(book: &Book, hit: &FtsHit) -> Option<Locator> {
    let excerpt = hit.snippet.replace(['[', ']'], "");
    let passage = excerpt
        .split('…')
        .max_by_key(|part| part.len())
        .unwrap_or_default();
    let normalize = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let needle = normalize(passage);
    if !needle.is_empty() {
        let mut matches = book.sections.iter().enumerate().filter(|(_, section)| {
            let text = section
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            normalize(&text).contains(&needle)
        });
        if let Some((section, contents)) = matches.next() {
            if matches.next().is_none() {
                let block = contents
                    .blocks
                    .iter()
                    .position(|block| normalize(&block.text).contains(&needle))
                    .unwrap_or(0);
                return Some(Locator {
                    section,
                    block,
                    offset: 0,
                });
            }
        }
    }
    // An extraction chapter can span multiple reader sections, so the
    // surrounding words may cross a section boundary.
    if let Some((_, rest)) = hit.snippet.split_once('[') {
        if let Some((term, _)) = rest.split_once(']') {
            let mut matches = book.search(term, 2).into_iter();
            if let Some(first) = matches.next() {
                if matches.next().is_none() {
                    return Some(first.start);
                }
            }
        }
    }
    let title = normalize(&hit.chapter_title);
    if title.is_empty() {
        return None;
    }
    let mut matches = book
        .sections
        .iter()
        .enumerate()
        .filter(|(_, section)| normalize(&section.title) == title);
    let (section, _) = matches.next()?;
    matches.next().is_none().then_some(Locator {
        section,
        ..Default::default()
    })
}

fn write_catalog(
    library: &Library,
    query: &str,
    extension: &str,
    output: &Path,
    replace: bool,
) -> cr_core::Result<()> {
    crate::files::write_atomic(output, replace, |file| {
        if extension == "epub" {
            library.catalog_epub(query, file)?;
        } else {
            let format = CatalogFormat::parse(extension)
                .ok_or_else(|| cr_core::Error::UnsupportedFormat(extension.to_string()))?;
            file.write_all(library.catalog(query, format)?.as_bytes())?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use cr_core::formats::document::{Document, Section};

    use super::*;

    #[core::prelude::v1::test]
    fn text_hit_follows_the_passage_when_the_reader_has_extra_sections() {
        let book = Book::from_document(Document {
            sections: vec![
                Section {
                    title: "Cover".into(),
                    html: String::new(),
                },
                Section {
                    title: "Chapter 2".into(),
                    html: "<p>The train came at dawn.</p>".into(),
                },
                Section {
                    title: "Chapter 3".into(),
                    html: "<p>A lighthouse stood in the bay.</p>".into(),
                },
            ],
            ..Default::default()
        });
        let hit = FtsHit {
            book: 1,
            title: "A story".into(),
            chapter: 2,
            chapter_title: "Chapter 2".into(),
            snippet: "A [lighthouse] stood in the bay.".into(),
        };
        assert_eq!(locate_text_hit(&book, &hit).unwrap().section, 2);
        let spanning_sections = FtsHit {
            chapter: 1,
            chapter_title: "Chapter 2".into(),
            snippet: "The train came at dawn. A [lighthouse] stood in the bay.".into(),
            ..hit
        };
        assert_eq!(
            locate_text_hit(&book, &spanning_sections).unwrap().section,
            2
        );
    }
}
