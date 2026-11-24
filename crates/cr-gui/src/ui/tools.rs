//! The tools screen: bulk edit, conversion, search tools, devices, content
//! server, trash and maintenance. Behaviour lives in [`crate::tools`].

use std::time::SystemTime;

use cr_core::formats::OUTPUT_FORMATS;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{empty_state, field, section_label};
use crate::app::{format_size, CalibreApp};
use crate::tools::{named_search, Join, TermMatch, ToolPane, PROFILES, SEARCH_FIELDS, TEXT_FIELDS};

pub fn tools_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let nav: Vec<AnyElement> = ToolPane::ALL
        .into_iter()
        .enumerate()
        .map(|(index, pane)| {
            let active = app.tools.pane == pane;
            div()
                .id(("tool-pane", index))
                .w_full()
                .px_3()
                .py(px(6.))
                .rounded(px(6.))
                .text_sm()
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .when(!active, |this| {
                    this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                })
                .on_click(cx.listener(move |this, _, window, cx| this.show_tool(pane, window, cx)))
                .child(pane.title())
                .into_any_element()
        })
        .collect();

    let pane = match app.tools.pane {
        ToolPane::BulkEdit => bulk_pane(app, cx),
        ToolPane::Convert => convert_pane(app, cx),
        ToolPane::Search => search_pane(app, cx),
        ToolPane::FullText => super::library_tools::full_text_pane(app, cx),
        ToolPane::Devices => devices_pane(app, cx),
        ToolPane::Server => server_pane(app, cx),
        ToolPane::Trash => trash_pane(app, cx),
        ToolPane::Maintenance => maintenance_pane(app, cx),
        ToolPane::Transfer => super::library_tools::transfer_pane(app, cx),
        ToolPane::Catalog => super::library_tools::catalog_pane(app, cx),
        ToolPane::CustomColumns => super::custom_columns::pane(app, cx),
    };

    h_flex()
        .size_full()
        .child(
            v_flex()
                .w(px(super::SIDEBAR_WIDTH))
                .h_full()
                .flex_shrink_0()
                .p_2()
                .gap(px(1.))
                .bg(theme.sidebar)
                .border_r_1()
                .border_color(theme.sidebar_border)
                .child(div().px_2().pt_2().pb_2().child(section_label("Tools", cx)))
                .children(nav),
        )
        .child(
            div()
                .id("tool-scroll")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .child(v_flex().max_w(px(760.)).p_6().gap_4().child(pane)),
        )
        .into_any_element()
}

fn intro(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn input(state: &Entity<InputState>) -> Input {
    Input::new(state).small().w_full()
}

/// A button that is one of several choices; the chosen one is filled.
fn choice(id: impl Into<ElementId>, label: impl Into<SharedString>, chosen: bool) -> Button {
    let button = Button::new(id).small().label(label.into());
    if chosen {
        button.primary()
    } else {
        button.outline()
    }
}

fn toggle(
    id: &'static str,
    label: &'static str,
    checked: bool,
    cx: &mut Context<CalibreApp>,
    set: impl Fn(&mut CalibreApp, bool) + 'static,
) -> Checkbox {
    Checkbox::new(id)
        .label(label)
        .checked(checked)
        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
            set(this, *checked);
            cx.notify();
        }))
}

/// A list row with a main line, a muted second line and trailing controls.
fn row(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .w_full()
        .px_3()
        .py_2()
        .gap_3()
        .items_center()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().truncate().child(title.into()))
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(detail.into()),
                ),
        )
}

fn selection_note(app: &CalibreApp, verb: &str, cx: &App) -> Div {
    let count = app.selection_ids().len();
    note(
        match count {
            0 => format!("No books are selected. Go back and select the books to {verb}."),
            1 => format!("This will {verb} the 1 selected book."),
            count => format!("This will {verb} the {count} selected books."),
        },
        cx,
    )
}

// -------------------------------------------------------------------------
// Bulk edit
// -------------------------------------------------------------------------

fn bulk_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let tools = &app.tools;
    let busy = app.job.is_some();
    let field_choices: Vec<AnyElement> = TEXT_FIELDS
        .into_iter()
        .enumerate()
        .map(|(index, (label, text_field))| {
            let chosen = tools.bulk_field == Some(text_field);
            choice(("bulk-field", index), label, chosen)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tools.bulk_field = (!chosen).then_some(text_field);
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();

    v_flex()
        .gap_4()
        .child(intro(
            "Change the metadata of every selected book at once. Fields left empty are not changed.",
            cx,
        ))
        .child(section_label("Set", cx))
        .child(field("Authors", input(&tools.bulk_authors), cx))
        .child(
            h_flex()
                .gap_3()
                .items_end()
                .child(field("Publisher", input(&tools.bulk_publisher), cx))
                .child(toggle(
                    "bulk-clear-publisher",
                    "Clear",
                    tools.bulk_clear_publisher,
                    cx,
                    |this, on| this.tools.bulk_clear_publisher = on,
                )),
        )
        .child(
            h_flex()
                .gap_3()
                .items_end()
                .child(field("Series", input(&tools.bulk_series), cx))
                .child(
                    div()
                        .w(px(200.))
                        .flex_shrink_0()
                        .child(field("Numbering", input(&tools.bulk_series_start), cx)),
                )
                .child(toggle(
                    "bulk-clear-series",
                    "Clear",
                    tools.bulk_clear_series,
                    cx,
                    |this, on| this.tools.bulk_clear_series = on,
                )),
        )
        .child(
            h_flex()
                .gap_3()
                .items_end()
                .child(
                    div()
                        .w(px(120.))
                        .flex_shrink_0()
                        .child(field("Rating", input(&tools.bulk_rating), cx)),
                )
                .child(toggle(
                    "bulk-clear-rating",
                    "Clear",
                    tools.bulk_clear_rating,
                    cx,
                    |this, on| this.tools.bulk_clear_rating = on,
                ))
                .child(field("Languages", input(&tools.bulk_languages), cx)),
        )
        .child(section_label("Tags", cx))
        .child(field("Add", input(&tools.bulk_add_tags), cx))
        .child(
            h_flex()
                .gap_3()
                .items_end()
                .child(field("Remove", input(&tools.bulk_remove_tags), cx))
                .child(toggle(
                    "bulk-clear-tags",
                    "Remove all",
                    tools.bulk_clear_tags,
                    cx,
                    |this, on| this.tools.bulk_clear_tags = on,
                )),
        )
        .child(section_label("Search and replace", cx))
        .child(h_flex().gap_1().flex_wrap().children(field_choices))
        .child(
            h_flex()
                .gap_3()
                .child(field("Find", input(&tools.bulk_find), cx))
                .child(field("Replace with", input(&tools.bulk_replace), cx)),
        )
        .child(
            h_flex()
                .gap_4()
                .child(toggle(
                    "bulk-regex",
                    "Regular expression",
                    tools.bulk_regex,
                    cx,
                    |this, on| this.tools.bulk_regex = on,
                ))
                .child(toggle(
                    "bulk-ignore-case",
                    "Ignore case",
                    tools.bulk_ignore_case,
                    cx,
                    |this, on| this.tools.bulk_ignore_case = on,
                )),
        )
        .child(selection_note(app, "change", cx))
        .child(
            h_flex().child(
                Button::new("bulk-apply")
                    .primary()
                    .small()
                    .label("Apply to Selected Books")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.apply_bulk_edit(window, cx))),
            ),
        )
        .into_any_element()
}

// -------------------------------------------------------------------------
// Conversion
// -------------------------------------------------------------------------

fn convert_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let tools = &app.tools;
    let targets: Vec<AnyElement> = OUTPUT_FORMATS
        .into_iter()
        .enumerate()
        .map(|(index, format)| {
            choice(
                ("convert-target", index),
                format,
                tools.convert_target == format,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tools.convert_target = format;
                cx.notify();
            }))
            .into_any_element()
        })
        .collect();
    let profiles: Vec<AnyElement> = PROFILES
        .into_iter()
        .enumerate()
        .map(|(index, (label, profile))| {
            choice(
                ("convert-profile", index),
                label,
                tools.convert_profile == profile,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tools.convert_profile = profile;
                cx.notify();
            }))
            .into_any_element()
        })
        .collect();

    v_flex()
        .gap_4()
        .child(intro(
            "Convert the selected books to another format. The new file is added to each book; \
             the source is picked from the formats the book already has.",
            cx,
        ))
        .child(section_label("Convert to", cx))
        .child(h_flex().gap_1().flex_wrap().children(targets))
        .child(section_label("Device profile", cx))
        .child(h_flex().gap_1().flex_wrap().children(profiles))
        .child(note(
            "Images larger than the profile's screen are scaled down. Generic keeps them as they are.",
            cx,
        ))
        .child(section_label("Options", cx))
        .child(toggle(
            "convert-overwrite",
            "Replace a format the book already has (the old file is kept as ORIGINAL_<FORMAT>)",
            tools.convert_overwrite,
            cx,
            |this, on| this.tools.convert_overwrite = on,
        ))
        .child(toggle(
            "convert-no-split",
            "Keep each chapter in one file, however long",
            tools.convert_no_split,
            cx,
            |this, on| this.tools.convert_no_split = on,
        ))
        .child(selection_note(app, "convert", cx))
        .child(
            h_flex().child(
                Button::new("convert-run")
                    .primary()
                    .small()
                    .label(format!("Convert to {}", tools.convert_target))
                    .disabled(app.job.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.convert_selection(window, cx))),
            ),
        )
        .into_any_element()
}

// -------------------------------------------------------------------------
// Search tools
// -------------------------------------------------------------------------

fn search_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let tools = &app.tools;
    let fields: Vec<AnyElement> = SEARCH_FIELDS
        .into_iter()
        .enumerate()
        .map(|(index, (label, _))| {
            choice(("term-field", index), label, tools.term_field == index)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tools.term_field = index;
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();
    let kinds: Vec<AnyElement> = TermMatch::ALL
        .into_iter()
        .enumerate()
        .map(|(index, (label, kind))| {
            choice(("term-match", index), label, tools.term_match == kind)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tools.term_match = kind;
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();
    let needs_value = !matches!(tools.term_match, TermMatch::Present | TermMatch::Missing);
    let join = |id: &'static str, label: &'static str, join: Join, cx: &mut Context<CalibreApp>| {
        Button::new(id).small().outline().label(label).on_click(
            cx.listener(move |this, _, window, cx| this.add_search_term(join, window, cx)),
        )
    };

    let saved_rows =
        |entries: &[(String, String)], virtual_library: bool, cx: &mut Context<CalibreApp>| {
            entries
                .iter()
                .enumerate()
                .map(|(index, (name, query))| {
                    let group = if virtual_library {
                        "virtual-library"
                    } else {
                        "saved-search"
                    };
                    // Shown through its name, so the search box says which one is open.
                    let run = named_search(virtual_library, name);
                    let delete = name.clone();
                    row((group, index), name.clone(), query.clone(), cx)
                        .child(
                            Button::new((SharedString::from(format!("{group}-show")), index))
                                .small()
                                .outline()
                                .label("Show")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.show_search(run.clone(), window, cx)
                                })),
                        )
                        .child(
                            Button::new((SharedString::from(format!("{group}-delete")), index))
                                .small()
                                .ghost()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete_search(delete.clone(), virtual_library, window, cx)
                                })),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        };
    let saved = saved_rows(&tools.saved_searches, false, cx);
    let libraries = saved_rows(&tools.virtual_libraries, true, cx);

    v_flex()
        .gap_4()
        .child(intro(
            "Build a search one term at a time, then save it as a saved search or a virtual library.",
            cx,
        ))
        .child(section_label("Current search", cx))
        .child(
            div()
                .w_full()
                .px_3()
                .py_2()
                .rounded(px(8.))
                .bg(theme.muted)
                .text_sm()
                .child(if app.query.trim().is_empty() {
                    "All books".to_string()
                } else {
                    app.query.clone()
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(note(
                    match app.books.len() {
                        1 => "1 book matches.".to_string(),
                        count => format!("{count} books match."),
                    },
                    cx,
                ))
                .child(div().flex_1())
                .child(
                    Button::new("search-clear")
                        .small()
                        .ghost()
                        .label("Clear")
                        .disabled(app.query.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_query(String::new(), window, cx)
                        })),
                )
                .child(
                    Button::new("search-show")
                        .small()
                        .primary()
                        .label("Show in Library")
                        .on_click(cx.listener(|this, _, window, cx| this.close_tools(window, cx))),
                ),
        )
        .child(section_label("Add a term", cx))
        .child(h_flex().gap_1().flex_wrap().children(fields))
        .child(h_flex().gap_1().flex_wrap().children(kinds))
        .when(needs_value, |this| {
            this.child(field("Value", input(&tools.term_value), cx))
        })
        .child(
            h_flex()
                .gap_2()
                .child(join("term-and", "Add (and)", Join::And, cx))
                .child(join("term-or", "Add (or)", Join::Or, cx))
                .child(join("term-not", "Add (not)", Join::Not, cx)),
        )
        .child(note(
            "Dates take 2024, 2024-06 or 2024-06-01; sizes take 500K or 2M; ratings are in stars. \
             You can also type a search in the library's search box: \
             author:asimov and (tag:sf or tag:\"space opera\") not format:pdf",
            cx,
        ))
        .child(section_label("Save the current search", cx))
        .child(
            h_flex()
                .gap_2()
                .items_end()
                .child(field("Name", input(&tools.search_name), cx))
                .child(
                    Button::new("save-search")
                        .small()
                        .outline()
                        .label("Save Search")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_search(false, window, cx)
                        })),
                )
                .child(
                    Button::new("save-virtual-library")
                        .small()
                        .outline()
                        .label("Save as Virtual Library")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_search(true, window, cx)
                        })),
                ),
        )
        .child(section_label(
            format!("Saved searches · {}", saved.len()),
            cx,
        ))
        .when(saved.is_empty(), |this| {
            this.child(note("None yet.", cx))
        })
        .child(v_flex().gap_1().children(saved))
        .child(section_label(
            format!("Virtual libraries · {}", libraries.len()),
            cx,
        ))
        .when(libraries.is_empty(), |this| {
            this.child(note("None yet.", cx))
        })
        .child(v_flex().gap_1().children(libraries))
        .into_any_element()
}

// -------------------------------------------------------------------------
// Devices
// -------------------------------------------------------------------------

fn devices_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let tools = &app.tools;
    let rescan = Button::new("devices-rescan")
        .small()
        .outline()
        .label("Rescan")
        .loading(app.scanning_devices)
        .on_click(cx.listener(|this, _, window, cx| {
            this.tools.device_loaded = false;
            this.rescan_devices(window, cx)
        }));
    if app.devices.is_empty() {
        return v_flex()
            .gap_4()
            .child(intro(
                "Connect a Kindle or Kobo, or mount a reader's storage, then rescan.",
                cx,
            ))
            .child(h_flex().child(rescan))
            .child(empty_state(
                IconName::Inbox,
                "No e-reader connected",
                "Books on a connected device are listed here.",
                cx,
            ))
            .into_any_element();
    }
    let index = tools.device_index.min(app.devices.len() - 1);
    let device_choices: Vec<AnyElement> = app
        .devices
        .iter()
        .enumerate()
        .map(|(position, device)| {
            choice(("device", position), device.name.clone(), position == index)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.load_device(position, window, cx)),
                )
                .into_any_element()
        })
        .collect();
    let marked = tools.device_marked.len();
    let selected = app.selection_ids().len();

    let books: Vec<AnyElement> = tools
        .device_books
        .iter()
        .enumerate()
        .map(|(position, book)| {
            let path = book.path.clone();
            let detail = format!(
                "{} · {} · {} · {}",
                book.format,
                format_size(book.bytes),
                book.state.as_str(),
                book.path
            );
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Checkbox::new(("device-book", position))
                        .accessibility_label(format!("Mark {} for removal", book.title))
                        .checked(tools.device_marked.contains(&book.path))
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            this.toggle_device_book(path.clone(), cx)
                        })),
                )
                .child(row(
                    ("device-row", position),
                    book.title.clone(),
                    detail,
                    cx,
                ))
                .into_any_element()
        })
        .collect();

    let reading: Vec<AnyElement> = tools
        .device_reading
        .iter()
        .enumerate()
        .map(|(position, entry)| {
            let mut detail = Vec::new();
            if let Some(author) = &entry.author {
                detail.push(author.clone());
            }
            if entry.finished {
                detail.push("finished".to_string());
            } else if let Some(percent) = entry.percent {
                detail.push(format!("{percent:.0}% read"));
            }
            if let Some(last_read) = &entry.last_read {
                detail.push(format!("last read {last_read}"));
            }
            match entry.annotations.len() {
                0 => {}
                1 => detail.push("1 highlight".to_string()),
                count => detail.push(format!("{count} highlights")),
            }
            row(
                ("device-reading", position),
                entry.title.clone(),
                detail.join(" · "),
                cx,
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .gap_4()
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(device_choices)
                .child(div().flex_1())
                .child(rescan),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("device-send")
                        .small()
                        .primary()
                        .label(match selected {
                            1 => "Send 1 Selected Book".to_string(),
                            count => format!("Send {count} Selected Books"),
                        })
                        .disabled(selected == 0 || app.sending_to_device)
                        .loading(app.sending_to_device)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.tools.device_loaded = false;
                            this.send_selected(index, window, cx)
                        })),
                )
                .child(
                    Button::new("device-refresh")
                        .small()
                        .outline()
                        .label("Refresh List")
                        .loading(tools.device_loading)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.load_device(index, window, cx)
                        })),
                )
                .child(div().flex_1())
                .child(
                    Button::new("device-remove")
                        .small()
                        .outline()
                        .label(match marked {
                            0 => "Delete from Device".to_string(),
                            count => format!("Delete {count} from Device"),
                        })
                        .disabled(marked == 0 || tools.device_loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_device_removal(window, cx)
                        })),
                ),
        )
        .child(section_label(
            format!("On the device · {}", tools.device_books.len()),
            cx,
        ))
        .when(tools.device_loading, |this| {
            this.child(note("Reading the device…", cx))
        })
        .when(!tools.device_loading && !tools.device_loaded, |this| {
            this.child(note(
                "Press Refresh List to read what is on the device.",
                cx,
            ))
        })
        .when(
            !tools.device_loading && tools.device_loaded && books.is_empty(),
            |this| this.child(note("No books found on the device.", cx)),
        )
        .child(v_flex().gap_1().children(books))
        .child(section_label(
            format!("Reading status · {}", reading.len()),
            cx,
        ))
        .when_some(tools.device_reading_note.clone(), |this, reason| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(reason),
            )
        })
        .when(
            tools.device_loaded && tools.device_reading_note.is_none() && reading.is_empty(),
            |this| this.child(note("The device has no reading progress recorded.", cx)),
        )
        .child(v_flex().gap_1().children(reading))
        .into_any_element()
}

// -------------------------------------------------------------------------
// Content server
// -------------------------------------------------------------------------

fn server_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let tools = &app.tools;
    let base = v_flex().gap_4().child(intro(
        "Serve this library to web browsers and reading apps on your network: browse, search, \
         download, read in the browser, and an OPDS catalog at /opds. The server only reads the library.",
        cx,
    ));
    if let Some(server) = &tools.server {
        let url = server.url.clone();
        let opds = format!("{}/opds", server.url);
        return base
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(8.)).rounded_full().bg(theme.success))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("The server is running"),
                            ),
                    )
                    .child(div().text_sm().child(server.url.clone()))
                    .child(note(format!("OPDS catalog: {opds}"), cx))
                    .when_some(server.warning.clone(), |this, warning| {
                        this.child(div().text_xs().text_color(theme.warning).child(warning))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("server-open")
                            .small()
                            .primary()
                            .label("Open in Browser")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("server-stop")
                            .small()
                            .outline()
                            .label("Stop Server")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.stop_server() {
                                    this.announce(
                                        "The server is stopped.",
                                        crate::app::Tone::Info,
                                        window,
                                        cx,
                                    );
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(note(
                "The server stops when you close the window or switch library.",
                cx,
            ))
            .into_any_element();
    }
    base.child(section_label("Address", cx))
        .child(
            h_flex()
                .gap_3()
                .child(field("Listen on", input(&tools.server_host), cx))
                .child(div().w(px(140.)).flex_shrink_0().child(field(
                    "Port",
                    input(&tools.server_port),
                    cx,
                ))),
        )
        .child(note(
            "127.0.0.1 serves this computer only. Use 0.0.0.0 to serve other machines, and set a \
             password when you do. There is no TLS: put a TLS reverse proxy in front for anything \
             beyond your own network.",
            cx,
        ))
        .child(section_label("Login (optional)", cx))
        .child(
            h_flex()
                .gap_3()
                .child(field("User", input(&tools.server_user), cx))
                .child(field("Password", input(&tools.server_password), cx)),
        )
        .child(
            h_flex().child(
                Button::new("server-start")
                    .small()
                    .primary()
                    .label("Start Server")
                    .on_click(cx.listener(|this, _, window, cx| this.start_server(window, cx))),
            ),
        )
        .into_any_element()
}

// -------------------------------------------------------------------------
// Trash
// -------------------------------------------------------------------------

/// How long ago something happened, in the largest fitting unit.
pub fn age(then: SystemTime, now: SystemTime) -> String {
    let seconds = now
        .duration_since(then)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (count, unit) = match seconds {
        0..60 => return "just now".to_string(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

fn trash_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let entries = &app.tools.trash;
    let now = SystemTime::now();
    let rows: Vec<AnyElement> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut detail = Vec::new();
            if !entry.authors.is_empty() {
                detail.push(entry.authors.join(" & "));
            }
            detail.push(if entry.is_book {
                "whole book".to_string()
            } else {
                "removed format".to_string()
            });
            if !entry.formats.is_empty() {
                detail.push(entry.formats.join(", "));
            }
            if let Some(deleted) = entry.deleted {
                detail.push(format!("removed {}", age(deleted, now)));
            }
            row(
                ("trash", index),
                entry.title.clone(),
                detail.join(" · "),
                cx,
            )
            .child(
                Button::new(("trash-restore", index))
                    .small()
                    .outline()
                    .label("Restore")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restore_trashed(index, window, cx)
                    })),
            )
            .child(
                Button::new(("trash-delete", index))
                    .small()
                    .ghost()
                    .label("Delete")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_purge(Some(index), window, cx)
                    })),
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .gap_4()
        .child(intro(
            "Removed books and formats are kept here until you delete them. calibre uses the same trash.",
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(section_label(format!("In the trash · {}", entries.len()), cx))
                .child(div().flex_1())
                .child(
                    Button::new("trash-empty")
                        .small()
                        .outline()
                        .label("Empty Trash")
                        .disabled(entries.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_purge(None, window, cx)
                        })),
                ),
        )
        .when(entries.is_empty(), |this| {
            this.child(note("The trash is empty.", cx))
        })
        .child(v_flex().gap_1().children(rows))
        .into_any_element()
}

// -------------------------------------------------------------------------
// Maintenance
// -------------------------------------------------------------------------

fn maintenance_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let busy = app.job.is_some();
    let problems = app.tools.problems.as_deref();
    let repairable = problems.map_or(0, |problems| {
        problems
            .iter()
            .filter(|problem| problem.repairable())
            .count()
    });
    let rows: Vec<AnyElement> = problems
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, problem)| {
            row(
                ("problem", index),
                problem.describe(),
                if problem.repairable() {
                    "Can be repaired"
                } else {
                    "Needs attention by hand"
                },
                cx,
            )
            .into_any_element()
        })
        .collect();

    v_flex()
        .gap_4()
        .child(intro(
            "Check that the database and the files on disk agree, and repair what can be repaired safely.",
            cx,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("library-check")
                        .small()
                        .primary()
                        .label("Check Library")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.check_library(window, cx))),
                )
                .child(
                    Button::new("library-repair")
                        .small()
                        .outline()
                        .label(match repairable {
                            0 => "Repair".to_string(),
                            count => format!("Repair {count} Problem(s)"),
                        })
                        .disabled(busy || repairable == 0)
                        .on_click(cx.listener(|this, _, window, cx| this.repair_library(window, cx))),
                ),
        )
        .map(|this| match problems {
            None => this.child(note("The library has not been checked yet.", cx)),
            Some([]) => this.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(div().size(px(8.)).rounded_full().bg(theme.success))
                    .child("No problems found."),
            ),
            Some(found) => this
                .child(section_label(format!("Problems · {}", found.len()), cx))
                .child(v_flex().gap_1().children(rows)),
        })
        .child(section_label("Rebuild the database", cx))
        .child(note(
            "If metadata.db is damaged, rebuild it from the metadata.opf file kept in each book's \
             folder. The current database is kept as a backup. Custom columns, saved searches and \
             virtual libraries are not restored.",
            cx,
        ))
        .child(
            h_flex().child(
                Button::new("library-rebuild")
                    .small()
                    .outline()
                    .label("Rebuild Database…")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_rebuild(window, cx))),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[core::prelude::v1::test]
    fn ages_use_the_largest_fitting_unit() {
        let now = SystemTime::now();
        let ago = |seconds| age(now - Duration::from_secs(seconds), now);
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(59 * 60), "59 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(3 * 86_400 + 5), "3 days ago");
        // A clock that went backwards is not an error.
        assert_eq!(age(now + Duration::from_secs(60), now), "just now");
    }
}
