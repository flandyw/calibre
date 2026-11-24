//! Tools panes for full-text search, library transfers, and catalog export.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CalibreApp;
use crate::library_tools::CATALOG_FORMATS;
use crate::settings::ImportDuplicates;

pub fn catalog_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let state = &app.library_tools;
    let busy = app.job.is_some();
    let formats = CATALOG_FORMATS
        .iter()
        .enumerate()
        .map(|(index, (label, _))| {
            let button = Button::new(("catalog-format", index)).small().label(*label);
            let button = if index == state.catalog_format {
                button.primary()
            } else {
                button.outline()
            };
            button
                .disabled(busy)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.library_tools.catalog_format = index;
                    cx.notify();
                }))
        })
        .collect::<Vec<_>>();

    v_flex()
        .gap_3()
        .child(heading("Export Catalog"))
        .child(detail(
            "Save a list of books with their metadata and custom columns.",
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&state.catalog_query).small()),
                )
                .child(
                    Button::new("catalog-use-search")
                        .ghost()
                        .small()
                        .label("Use Library Search")
                        .disabled(app.query.trim().is_empty() || busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.use_library_query_for_catalog(window, cx)
                        })),
                ),
        )
        .child(h_flex().gap_2().flex_wrap().children(formats))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("export-catalog")
                        .primary()
                        .small()
                        .label("Save Catalog…")
                        .disabled(busy || app.library.is_none())
                        .on_click(
                            cx.listener(|app, _, window, cx| app.prompt_export_catalog(window, cx)),
                        ),
                )
                .when_some(state.catalog_output.clone(), |row, path| {
                    row.child(
                        Button::new("reveal-catalog")
                            .ghost()
                            .small()
                            .label(crate::actions::reveal_label())
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                }),
        )
        .into_any_element()
}

pub fn full_text_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let state = &app.library_tools;
    let indexed = app
        .library
        .as_ref()
        .is_some_and(|library| library.has_fts_index());
    let writable = app
        .library
        .as_ref()
        .is_some_and(|library| library.read_only_reason().is_none());
    let busy = app.job.is_some();
    let separator = cx.theme().border;
    let failures = state
        .index_report
        .as_ref()
        .into_iter()
        .flat_map(|report| &report.failed)
        .take(10)
        .map(|(id, error)| detail(format!("Book #{id} · {error}"), cx))
        .collect::<Vec<_>>();

    v_flex()
        .gap_3()
        .child(heading("Full-text Search"))
        .child(detail(
            if indexed {
                "Search the contents of indexed ebooks. Use quotes for a phrase, OR for alternatives, or * for a prefix."
            } else {
                "Build an index to search inside every readable book in this library."
            },
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("text-index")
                        .outline()
                        .small()
                        .label(if indexed { "Update Index" } else { "Build Index" })
                        .disabled(busy || !writable)
                        .on_click(cx.listener(|app, _, window, cx| app.index_book_text(window, cx))),
                )
                .when_some(state.index_report.as_ref(), |row, report| {
                    row.child(detail(
                        format!("{} indexed · {} current · {} removed · {} failed",
                            report.indexed, report.unchanged, report.removed, report.failed.len()),
                        cx,
                    ))
                }),
        )
        .children(failures)
        .when(indexed, |pane| {
            pane.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&state.text_query).small()))
                    .child(
                        Button::new("search-text")
                            .primary()
                            .small()
                            .label("Search")
                            .disabled(busy)
                            .on_click(cx.listener(|app, _, window, cx| app.search_book_text(window, cx))),
                    )
                    .child(
                        Button::new("clear-text")
                            .ghost()
                            .small()
                            .label("Clear")
                            .disabled(busy)
                            .on_click(cx.listener(|app, _, window, cx| app.clear_text_search(window, cx))),
                    ),
            )
        })
        .when_some(state.text_hits.as_ref(), |pane, results| {
            if results.is_empty() {
                return pane.child(detail("No matching chapters.", cx));
            }
            pane.children(results.iter().enumerate().map(|(index, hit)| {
                let (id, chapter) = (hit.book, hit.chapter);
                let selected_hit = hit.clone();
                let title = if hit.chapter_title.is_empty() {
                    format!("Chapter {chapter}")
                } else {
                    hit.chapter_title.clone()
                };
                v_flex()
                    .gap_1()
                    .py_2()
                    .border_t_1()
                    .border_color(separator)
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(hit.title.clone()))
                    .child(detail(title, cx))
                    .child(div().text_sm().child(hit.snippet.clone()))
                    .child(
                        h_flex().gap_2().pt_1()
                            .child(Button::new(("read-chapter", index)).outline().small()
                                .label("Read Book").disabled(busy)
                                .on_click(cx.listener(move |app, _, window, cx| app.read_library_book(id, None, Some(&selected_hit), window, cx))))
                            .child(Button::new(("show-hit", index)).ghost().small()
                                .label("Show Book in Library")
                                .on_click(cx.listener(move |app, _, window, cx| app.show_text_hit_in_library(id, window, cx)))),
                    )
            }))
        })
        .into_any_element()
}

pub fn transfer_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let state = &app.library_tools;
    let count = app.selection_ids().len();
    let busy = app.job.is_some();
    let ready = count > 0 && state.transfer_target.is_some() && !busy;
    let source = app.library.as_ref().map(|library| library.root());
    let writable = app
        .library
        .as_ref()
        .is_some_and(|library| library.read_only_reason().is_none());
    let recent = app
        .settings
        .recent_libraries
        .iter()
        .filter(|path| Some(path.as_path()) != source)
        .take(5)
        .enumerate()
        .map(|(index, path)| {
            let target = path.clone();
            Button::new(("transfer-recent", index))
                .outline()
                .small()
                .label(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                )
                .tooltip(path.display().to_string())
                .disabled(busy)
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.set_transfer_target(target.clone(), window, cx)
                }))
        })
        .collect::<Vec<_>>();
    let duplicates = [
        ("Skip matches", ImportDuplicates::Skip),
        ("Keep both", ImportDuplicates::KeepBoth),
        ("Merge formats", ImportDuplicates::Merge),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (label, value))| {
        let button = Button::new(("transfer-duplicates", index))
            .small()
            .label(label)
            .selected(value == state.transfer_duplicates)
            .disabled(busy);
        let button = if value == state.transfer_duplicates {
            button.primary()
        } else {
            button.outline()
        };
        button.on_click(cx.listener(move |app, _, _, cx| {
            app.library_tools.transfer_duplicates = value;
            cx.notify();
        }))
    })
    .collect::<Vec<_>>();

    v_flex()
        .gap_3()
        .child(heading("Transfer Books"))
        .child(detail(
            if count == 0 { "Select books in the library to copy or move them.".into() }
            else { format!("{count} book(s) selected. Formats, covers, metadata and extra files are included.") },
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("choose-transfer-target").outline().small()
                        .label("Choose Destination…").disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| app.choose_transfer_target(window, cx))),
                )
                .when_some(state.transfer_target.as_ref(), |row, path| {
                    row.child(detail(path.display().to_string(), cx))
                }),
        )
        .when(!recent.is_empty(), |pane| {
            pane.child(detail("Recent libraries", cx))
                .child(h_flex().gap_2().flex_wrap().children(recent))
        })
        .child(detail("When the destination already has a matching book", cx))
        .child(h_flex().gap_2().children(duplicates))
        .when(state.transfer_duplicates == ImportDuplicates::Merge, |pane| {
            pane.child(detail("Merging is available when copying books.", cx))
        })
        .child(
            h_flex().gap_2()
                .child(Button::new("copy-transfer").primary().small().label("Copy Books")
                    .disabled(!ready)
                    .on_click(cx.listener(|app, _, window, cx| app.start_transfer(false, window, cx))))
                .child(Button::new("move-transfer").outline().small().label("Move Books…")
                    .disabled(!ready || !writable || state.transfer_duplicates == ImportDuplicates::Merge)
                    .on_click(cx.listener(|app, _, window, cx| app.start_transfer(true, window, cx)))),
        )
        .when_some(state.transfer_report.as_ref(), |pane, report| {
            let title = if report.cancelled { "Transfer stopped" } else { "Transfer complete" };
            pane.child(heading(title))
                .child(detail(format!("{} {} · {} merged · {} skipped · {} failed",
                    report.written, if report.moved { "moved" } else { "copied" },
                    report.merged, report.skipped, report.failures.len()), cx))
                .children(report.failures.iter().take(20).cloned().map(|failure| detail(failure, cx)))
                .when_some(state.transfer_target.clone(), |pane, path| {
                    pane.child(Button::new("open-transfer-target").ghost().small().label("Open Destination Library")
                        .disabled(busy)
                        .on_click(cx.listener(move |app, _, window, cx| { app.open_library(path.clone(), window, cx); })))
                })
        })
        .into_any_element()
}

fn heading(title: &'static str) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(title)
}

fn detail(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
