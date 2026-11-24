//! The library screen: sidebar, book list or cover grid, and the inspector.

use std::ops::Range;

use cr_core::library::BookSort;
use cr_core::metadata::BookSummary;
use cr_core::text;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{cover_thumb, empty_state, section_label, DragPreview, DraggedBook};
use super::{inspector, GRID_CARD_WIDTH, GRID_GAP, GRID_PADDING, LIST_ROW_HEIGHT, SIDEBAR_WIDTH};
use crate::actions::*;
use crate::app::{CalibreApp, Shelf, ShelfSection};
use crate::settings::ViewMode;

pub fn library_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    h_flex()
        .size_full()
        .overflow_hidden()
        .when(app.settings.sidebar_visible, |this| {
            this.child(sidebar(app, cx))
        })
        .child(books_pane(app, cx))
        .when(app.settings.inspector_visible, |this| {
            this.child(inspector::inspector(app, cx))
        })
        .into_any_element()
}

// -------------------------------------------------------------------------
// Sidebar
// -------------------------------------------------------------------------

fn sidebar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let total = app.all_books.len();

    let mut content = v_flex().px_2().py_3().gap_4().child(
        v_flex()
            .gap(px(1.))
            .child(div().px_2().pb_1().child(section_label("Library", cx)))
            .child(shelf_row(
                app,
                "shelf-all",
                Lucide::LibraryBig,
                Shelf::All,
                Some(total),
                cx,
            ))
            .child(shelf_row(
                app,
                "shelf-recent",
                Lucide::Clock,
                Shelf::RecentlyAdded,
                None,
                cx,
            )),
    );

    for (section, title, icon) in [
        (ShelfSection::Authors, "Authors", Lucide::User),
        (ShelfSection::Series, "Series", Lucide::BookCopy),
        (ShelfSection::Tags, "Tags", Lucide::Tag),
        (ShelfSection::Formats, "Formats", Lucide::FileText),
    ] {
        let entries = app.categories.entries(section);
        if entries.is_empty() {
            continue;
        }
        let expanded = app.expanded_sections.contains(&section);
        let mut group = v_flex().gap(px(1.)).child(
            h_flex()
                .id(SharedString::from(format!("section-{title}")))
                .px_2()
                .py_1()
                .gap_1()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(theme.muted_foreground),
                )
                .child(section_label(title, cx))
                .child(div().flex_1())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(entries.len().to_string()),
                ),
        );
        if expanded {
            for (index, (name, count)) in entries.iter().enumerate() {
                let shelf = match section {
                    ShelfSection::Authors => Shelf::Author(name.clone()),
                    ShelfSection::Series => Shelf::Series(name.clone()),
                    ShelfSection::Tags => Shelf::Tag(name.clone()),
                    ShelfSection::Formats => Shelf::Format(name.clone()),
                };
                group = group.child(shelf_row(
                    app,
                    SharedString::from(format!("shelf-{title}-{index}")),
                    icon,
                    shelf,
                    Some(*count),
                    cx,
                ));
            }
        }
        content = content.child(group);
    }

    if !app.devices.is_empty() {
        let mut devices = v_flex()
            .gap(px(1.))
            .child(div().px_2().pb_1().child(section_label("Devices", cx)));
        for (index, device) in app.devices.iter().enumerate() {
            let busy = app.sending_to_device;
            devices = devices.child(
                h_flex()
                    .id(("device", index))
                    .px_2()
                    .py(px(5.))
                    .gap_2()
                    .items_center()
                    .rounded(px(6.))
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Send the selected book")
                            .action(&SendToFirstDevice, None)
                            .build(window, cx)
                    })
                    .on_click(move |_, window, cx| {
                        if !busy {
                            window.dispatch_action(Box::new(SendToDevice(index)), cx)
                        }
                    })
                    .child(
                        Icon::new(Lucide::TabletSmartphone)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().truncate().child(device.name.clone())),
            );
        }
        content = content.child(devices);
    }

    div()
        .id("sidebar")
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_shrink_0()
        .overflow_y_scroll()
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(content)
        .into_any_element()
}

fn shelf_row(
    app: &CalibreApp,
    id: impl Into<ElementId>,
    icon: Lucide,
    shelf: Shelf,
    count: Option<usize>,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let active = app.shelf == shelf;
    let title = shelf.title();
    h_flex()
        .id(id)
        .px_2()
        .py(px(5.))
        .gap_2()
        .items_center()
        .rounded(px(6.))
        .text_sm()
        .cursor_pointer()
        .when(active, |this| {
            this.bg(theme.sidebar_accent)
                .text_color(theme.sidebar_accent_foreground)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!active, |this| {
            this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
        })
        .on_click(
            cx.listener(move |this, _, window, cx| this.select_shelf(shelf.clone(), window, cx)),
        )
        .child(Icon::new(icon).small().text_color(if active {
            theme.sidebar_accent_foreground
        } else {
            theme.muted_foreground
        }))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .when_some(count, |this, count| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(count.to_string()),
            )
        })
        .into_any_element()
}

// -------------------------------------------------------------------------
// Book list / grid
// -------------------------------------------------------------------------

fn books_pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let body = if app.books.is_empty() {
        let (title, subtitle) = if app.all_books.is_empty() {
            (
                "Your library is empty",
                if cfg!(target_os = "macos") {
                    "Add books with ⌘O, or drop ebook files into this window."
                } else {
                    "Add books with Ctrl+O, or drop ebook files into this window."
                },
            )
        } else if !app.query.is_empty() {
            (
                "No matches",
                "Try another search, or press Esc to clear it.",
            )
        } else {
            ("Nothing here", "No books match this shelf.")
        };
        empty_state(IconName::BookOpen, title, subtitle, cx)
    } else {
        match app.settings.view {
            ViewMode::List => book_list(app, cx),
            ViewMode::Grid => book_grid(app, cx),
        }
    };

    v_flex()
        .key_context(LIST_CONTEXT)
        .track_focus(&app.list_focus)
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(theme.background)
        .child(list_header(app, cx))
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

fn sort_label(sort: BookSort) -> &'static str {
    match sort {
        BookSort::Title => "Title",
        BookSort::Author => "Author",
        BookSort::Date => "Date Added",
        BookSort::Pubdate => "Published",
        BookSort::Series => "Series",
        BookSort::Rating => "Rating",
    }
}

fn list_header(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let sort = app.settings.sort();
    let descending = app.settings.descending;
    let recent = app.shelf == Shelf::RecentlyAdded;
    let count = match app.books.len() {
        1 => "1 book".to_string(),
        count => format!("{count} books"),
    };

    h_flex()
        .h(px(44.))
        .flex_shrink_0()
        .px_4()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(app.shelf.title()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(count),
        )
        .child(div().flex_1())
        .when(!recent, |this| {
            this.child(
                Button::new("sort-menu")
                    .ghost()
                    .xsmall()
                    .icon(if descending {
                        IconName::SortDescending
                    } else {
                        IconName::SortAscending
                    })
                    .label(sort_label(sort))
                    .tooltip("Sort books")
                    .on_click(move |event, window, cx| {
                        let item =
                            |name: &'static str, which: BookSort, action: Box<dyn Action>| {
                                (name, which == sort, action)
                            };
                        let mut menu = NativeMenu::new();
                        for (name, checked, action) in [
                            item("Title", BookSort::Title, Box::new(SortByTitle)),
                            item("Author", BookSort::Author, Box::new(SortByAuthor)),
                            item("Date Added", BookSort::Date, Box::new(SortByDateAdded)),
                            item("Published", BookSort::Pubdate, Box::new(SortByPublished)),
                            item("Series", BookSort::Series, Box::new(SortBySeries)),
                            item("Rating", BookSort::Rating, Box::new(SortByRating)),
                        ] {
                            menu = menu.menu_with_check(name, checked, action);
                        }
                        menu.separator()
                            .menu_with_check(
                                "Descending",
                                descending,
                                Box::new(ToggleSortDirection),
                            )
                            .show(event.position(), window, cx);
                    }),
            )
        })
        .into_any_element()
}

fn book_list(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    uniform_list(
        "book-list",
        app.books.len(),
        cx.processor(|this, range: Range<usize>, window, cx| {
            range
                .filter_map(|index| {
                    let book = this.books.get(index)?.clone();
                    Some(list_row(this, index, &book, window, cx))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&app.list_scroll)
    .size_full()
    .py_1()
    .into_any_element()
}

fn list_row(
    app: &CalibreApp,
    index: usize,
    book: &BookSummary,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    let theme = cx.theme().clone();
    let selected = app.is_marked(book.id);
    let focused = app.list_focus.is_focused(window);
    let authors = if book.authors.is_empty() {
        "Unknown author".to_string()
    } else {
        book.authors.join(" & ")
    };
    let series = book
        .series
        .as_ref()
        .map(|series| format!("{series} #{}", text::format_series_index(book.series_index)));
    let selected_bg = if focused {
        theme.list_active
    } else {
        theme.muted
    };

    let row = h_flex()
        .id(("book-row", book.id as u64))
        .w_full()
        .h_full()
        .px_2()
        .gap_3()
        .items_center()
        .rounded(px(8.))
        .when(selected, |this| this.bg(selected_bg))
        .when(!selected, |this| {
            this.hover(|style| style.bg(cx.theme().list_hover))
        })
        .child(cover_thumb(app.cover_for(book), &book.title, 30., 44., cx))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(book.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(authors),
                ),
        )
        .when_some(series, |this, series| {
            this.child(
                div()
                    .max_w(px(180.))
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(series),
            )
        })
        .child(
            h_flex()
                .gap_1()
                .children(book.formats.iter().take(3).map(|format| {
                    div()
                        .px(px(5.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(px(10.))
                        .text_color(theme.muted_foreground)
                        .child(format.clone())
                })),
        );

    // Rows span the full list width; the outer padding keeps the rounded
    // highlight off the pane edges.
    div()
        .w_full()
        .h(px(LIST_ROW_HEIGHT))
        .px_2()
        .child(book_interactions(row, index, book, cx))
        .into_any_element()
}

fn book_grid(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let cards: Vec<AnyElement> = app
        .books
        .iter()
        .enumerate()
        .map(|(index, book)| {
            let selected = app.is_marked(book.id);
            let authors = if book.authors.is_empty() {
                "Unknown author".to_string()
            } else {
                book.authors.join(" & ")
            };
            let card = v_flex()
                .id(("book-card", book.id as u64))
                .w(px(GRID_CARD_WIDTH))
                .gap_1p5()
                .child(
                    div()
                        .p(px(3.))
                        .rounded(px(8.))
                        .border_2()
                        .border_color(if selected {
                            theme.primary
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .child(cover_thumb(
                            app.cover_for(book),
                            &book.title,
                            GRID_CARD_WIDTH - 10.,
                            (GRID_CARD_WIDTH - 10.) * 1.5,
                            cx,
                        )),
                )
                .child(
                    div()
                        .px_1()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .line_clamp(2)
                        .child(book.title.clone()),
                )
                .child(
                    div()
                        .px_1()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(authors),
                );
            book_interactions(card, index, book, cx).into_any_element()
        })
        .collect();

    let width_cell = app.grid_width.clone();
    div()
        .relative()
        .size_full()
        .child(
            canvas(
                move |bounds, _, _| width_cell.set(bounds.size.width.as_f32()),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id("book-grid")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&app.grid_scroll)
                .p(px(GRID_PADDING))
                .flex()
                .flex_wrap()
                .content_start()
                .gap(px(GRID_GAP))
                .children(cards),
        )
        .into_any_element()
}

/// Click, double-click, context menu and drag-out behaviour shared by list
/// rows and grid cards.
fn book_interactions(
    element: Stateful<Div>,
    index: usize,
    book: &BookSummary,
    cx: &mut Context<CalibreApp>,
) -> Stateful<Div> {
    let dragged = DraggedBook {
        id: book.id,
        title: book.title.clone().into(),
        formats: book.formats.clone(),
    };
    let view = cx.entity().downgrade();
    element
        .cursor_pointer()
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            window.focus(&this.list_focus, cx);
            let modifiers = event.modifiers();
            let toggle = modifiers.control || modifiers.platform;
            this.click_index(index, toggle, modifiers.shift, window, cx);
            if event.click_count() == 2 {
                this.start_reading(window, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.list_focus, cx);
                let inside = this.books.get(index).is_some_and(|b| this.is_marked(b.id));
                if !inside {
                    this.click_index(index, false, false, window, cx);
                }
                book_context_menu(this).show(event.position, window, cx);
            }),
        )
        .on_drag(dragged, |dragged, _, _, cx| {
            let title = dragged.title.clone();
            cx.new(|_| DragPreview { title })
        })
        .external_drag_payload(move |dragged: &DraggedBook, _, cx| {
            let view = view.upgrade()?;
            let path = view
                .read(cx)
                .preferred_format_path(dragged.id, &dragged.formats)?;
            Some(ExternalDragPayload::Files(FileDragPaths::new([(
                path, false,
            )])))
        })
}

/// The right-click menu for the selected book, drawn by the OS.
pub fn book_context_menu(app: &CalibreApp) -> NativeMenu {
    let has_epub = crate::app::editable_format(&app.selected_formats).is_some();
    let has_files = !app.selected_formats.is_empty();
    let mut menu = NativeMenu::new()
        .menu_with_disabled("Read", !has_files, Box::new(ReadBook))
        .menu_with_disabled(
            "Open in Default App",
            !has_files,
            Box::new(OpenInDefaultApp),
        )
        .menu(reveal_label(), Box::new(RevealInFileManager))
        .separator()
        .menu("Edit Metadata…", Box::new(EditMetadata))
        .menu_with_disabled("Ask AI…", app.job.is_some(), Box::new(AskAi))
        .menu_with_disabled(
            "Download Metadata…",
            app.job.is_some(),
            Box::new(DownloadMetadata),
        )
        .menu_with_disabled(
            "Attach Formats…",
            !app.can_write() || app.job.is_some(),
            Box::new(AttachFormats),
        )
        .menu("Change Cover…", Box::new(ChangeCover))
        .menu_with_disabled("Crop Cover…", app.cover_path.is_none(), Box::new(CropCover))
        .menu_with_disabled("Edit Book…", !has_epub, Box::new(EditEpub))
        .separator()
        .menu("Copy Title and Author", Box::new(CopyBookInfo))
        .menu("Export…", Box::new(ExportSelected));
    if !app.devices.is_empty() {
        let mut send = NativeMenu::new();
        for (index, device) in app.devices.iter().enumerate() {
            send = send.menu_with_disabled(
                device.name.clone(),
                app.sending_to_device,
                Box::new(SendToDevice(index)),
            );
        }
        menu = menu.submenu("Send to", send);
    }
    menu.separator().menu("Remove…", Box::new(RemoveBook))
}
