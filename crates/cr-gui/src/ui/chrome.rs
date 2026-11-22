//! Window chrome: the unified title bar, status bar, welcome screen and overlays.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{section_label, tool_button};
use crate::actions::*;
use crate::app::{default_library_path, CalibreApp, Screen, Tone};
use crate::settings::{ThemePreference, ViewMode};

pub fn title_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let content = match app.screen {
        Screen::Library if app.library.is_none() => welcome_bar(cx),
        Screen::Library => library_bar(app, cx),
        Screen::Reader => reader_bar(app, cx),
        Screen::Epub => epub_bar(app, cx),
        Screen::Crop => crop_bar(),
    };
    TitleBar::new()
        .child(
            h_flex()
                .size_full()
                .pr_2()
                .gap_1()
                .items_center()
                .child(content),
        )
        .into_any_element()
}

fn welcome_bar(cx: &mut Context<CalibreApp>) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("calibre-rust"),
        )
        .into_any_element()
}

fn library_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let library_name = app.library_name();
    let recent = app.settings.recent_libraries.clone();
    let view = app.settings.view;
    let theme_pref = app.settings.theme;
    let device_count = app.devices.len();
    let device_names: Vec<String> = app
        .devices
        .iter()
        .map(|device| device.name.clone())
        .collect();
    let scanning = app.scanning_devices;

    let library_switcher = Button::new("library-switcher")
        .ghost()
        .small()
        .icon(Lucide::LibraryBig)
        .label(library_name)
        .tooltip("Switch library")
        .on_click(move |event, window, cx| {
            let mut menu = NativeMenu::new();
            for (index, path) in recent.iter().enumerate() {
                menu = menu.menu_with_check(
                    path.display().to_string(),
                    index == 0,
                    Box::new(OpenRecentLibrary(index)),
                );
            }
            menu.separator()
                .menu("Open Library…", Box::new(OpenLibrary))
                .menu("New Library…", Box::new(NewLibrary))
                .show(event.position(), window, cx);
        });

    let view_toggle = h_flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(theme.muted)
        .child(
            Button::new("view-list")
                .ghost()
                .xsmall()
                .icon(Lucide::List)
                .selected(view == ViewMode::List)
                .tooltip_with_action("List", &ShowAsList, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAsList), cx)),
        )
        .child(
            Button::new("view-grid")
                .ghost()
                .xsmall()
                .icon(Lucide::LayoutGrid)
                .selected(view == ViewMode::Grid)
                .tooltip_with_action("Covers", &ShowAsGrid, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAsGrid), cx)),
        );

    let devices = Button::new("devices-menu")
        .ghost()
        .small()
        .icon(Lucide::Usb)
        .loading(scanning)
        .when(device_count > 0, |button| {
            button.label(device_count.to_string())
        })
        .tooltip(if device_count == 0 {
            "No e-reader connected"
        } else {
            "Send to e-reader"
        })
        .on_click(move |event, window, cx| {
            let mut menu = NativeMenu::new();
            if device_names.is_empty() {
                menu = menu.menu_with_disabled(
                    "No e-readers connected",
                    true,
                    Box::new(RescanDevices),
                );
            }
            for (index, name) in device_names.iter().enumerate() {
                menu = menu.menu(format!("Send to {name}"), Box::new(SendToDevice(index)));
            }
            menu.separator()
                .menu("Rescan Devices", Box::new(RescanDevices))
                .show(event.position(), window, cx);
        });

    let appearance_icon = match theme_pref {
        ThemePreference::System => Lucide::SunMoon,
        ThemePreference::Light => Lucide::Sun,
        ThemePreference::Dark => Lucide::Moon,
    };
    let appearance = Button::new("appearance-menu")
        .ghost()
        .small()
        .icon(appearance_icon)
        .tooltip("Appearance")
        .on_click(move |event, window, cx| {
            NativeMenu::new()
                .menu_with_check(
                    "Match System",
                    theme_pref == ThemePreference::System,
                    Box::new(ThemeSystem),
                )
                .menu_with_check(
                    "Light",
                    theme_pref == ThemePreference::Light,
                    Box::new(ThemeLight),
                )
                .menu_with_check(
                    "Dark",
                    theme_pref == ThemePreference::Dark,
                    Box::new(ThemeDark),
                )
                .show(event.position(), window, cx);
        });

    h_flex()
        .w_full()
        .gap_1()
        .items_center()
        .child(tool_button(
            "toggle-sidebar",
            if app.settings.sidebar_visible {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            },
            "Toggle sidebar",
            ToggleSidebar,
        ))
        .child(library_switcher)
        .child(div().flex_1())
        .child(
            div().w(px(300.)).child(
                Input::new(&app.search)
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            ),
        )
        .child(div().flex_1())
        .child(tool_button(
            "add-books",
            IconName::Plus,
            "Add books",
            AddBooks,
        ))
        .child(view_toggle)
        .child(devices)
        .child(appearance)
        .child(tool_button(
            "toggle-inspector",
            if app.settings.inspector_visible {
                IconName::PanelRightClose
            } else {
                IconName::PanelRightOpen
            },
            "Toggle inspector",
            ToggleDetailsPane,
        ))
        .into_any_element()
}

fn reader_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let total = app.chapters.len();
    let position = format!("{} / {}", app.chapter_index + 1, total);
    h_flex()
        .w_full()
        .gap_1()
        .items_center()
        .child(
            Button::new("reader-back")
                .ghost()
                .small()
                .icon(IconName::ChevronLeft)
                .label("Library")
                .tooltip_with_action("Back to library", &Cancel, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(tool_button(
            "reader-chapters",
            if app.chapter_list_visible {
                IconName::PanelLeftClose
            } else {
                IconName::PanelLeftOpen
            },
            "Chapters",
            ToggleChapterList,
        ))
        .child(div().flex_1())
        .child(
            div()
                .max_w(px(420.))
                .truncate()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(app.reader_title.clone()),
        )
        .child(div().flex_1())
        .child(tool_button(
            "font-smaller",
            Lucide::AArrowDown,
            "Smaller text",
            DecreaseFontSize,
        ))
        .child(tool_button(
            "font-bigger",
            Lucide::AArrowUp,
            "Bigger text",
            IncreaseFontSize,
        ))
        .child(div().w(px(8.)))
        .child(
            tool_button(
                "chapter-prev",
                IconName::ChevronLeft,
                "Previous chapter",
                PreviousChapter,
            )
            .disabled(app.chapter_index == 0),
        )
        .child(div().text_xs().text_color(muted).child(position))
        .child(
            tool_button(
                "chapter-next",
                IconName::ChevronRight,
                "Next chapter",
                NextChapter,
            )
            .disabled(app.chapter_index + 1 >= total),
        )
        .into_any_element()
}

fn epub_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let title = app
        .metadata
        .as_ref()
        .map(|metadata| metadata.title.clone())
        .unwrap_or_default();
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            Button::new("epub-close")
                .ghost()
                .small()
                .icon(IconName::ChevronLeft)
                .label("Library")
                .tooltip_with_action("Close the editor", &Cancel, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Edit EPUB"),
        )
        .child(
            div()
                .max_w(px(320.))
                .truncate()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(title),
        )
        .child(div().flex_1())
        .when(app.epub_dirty, |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().size(px(7.)).rounded_full().bg(theme.warning))
                    .child("Edited"),
            )
        })
        .child(
            Button::new("epub-save")
                .small()
                .outline()
                .label("Save File")
                .tooltip_with_action("Save this file", &SaveChanges, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(SaveChanges), cx)),
        )
        .child(
            Button::new("epub-repack")
                .small()
                .primary()
                .label("Repack & Replace")
                .tooltip("Write all edits into the library's EPUB")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(RepackEpub), cx)),
        )
        .into_any_element()
}

fn crop_bar() -> AnyElement {
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            Button::new("crop-back")
                .ghost()
                .small()
                .icon(IconName::ChevronLeft)
                .label("Library")
                .tooltip_with_action("Cancel", &Cancel, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Crop Cover"),
        )
        .child(div().flex_1())
        .child(
            Button::new("crop-cancel")
                .small()
                .outline()
                .label("Cancel")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
        )
        .child(
            Button::new("crop-save")
                .small()
                .primary()
                .label("Save Cover")
                .tooltip_with_action("Save the cropped cover", &SaveChanges, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(SaveChanges), cx)),
        )
        .into_any_element()
}

pub fn status_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let (icon, color) = match app.status_tone {
        Tone::Info => (IconName::Info, theme.muted_foreground),
        Tone::Success => (IconName::CircleCheck, theme.success),
        Tone::Error => (IconName::TriangleAlert, theme.danger),
    };
    let counts = if app.library.is_some() {
        if app.books.len() == app.all_books.len() {
            format!("{} books", app.all_books.len())
        } else {
            format!("{} of {} books", app.books.len(), app.all_books.len())
        }
    } else {
        String::new()
    };
    let devices = match (app.sending_to_device, app.devices.len()) {
        (true, _) => "Sending…".to_string(),
        (false, 0) => String::new(),
        (false, 1) => app.devices[0].name.clone(),
        (false, count) => format!("{count} e-readers"),
    };

    h_flex()
        .h(px(26.))
        .flex_shrink_0()
        .px_3()
        .gap_2()
        .items_center()
        .bg(theme.status_bar)
        .border_t_1()
        .border_color(theme.status_bar_border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .when(!app.status.is_empty(), |this| {
            this.child(Icon::new(icon).xsmall().text_color(color))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(app.status.clone()),
        )
        .when(!devices.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(Lucide::Usb).xsmall())
                    .child(devices),
            )
        })
        .when(!counts.is_empty(), |this| this.child(counts))
        .into_any_element()
}

pub fn welcome(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let recent = app.settings.recent_libraries.clone();
    let default_path = default_library_path();

    let recent_rows: Vec<AnyElement> = recent
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let name = crate::app::display_name(path);
            let full = path.display().to_string();
            h_flex()
                .id(("recent-library", index))
                .w_full()
                .px_3()
                .py_2()
                .gap_3()
                .items_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().list_hover))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenRecentLibrary(index)), cx)
                })
                .child(Icon::new(IconName::FolderOpen).text_color(theme.muted_foreground))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(full),
                        ),
                )
                .into_any_element()
        })
        .collect();

    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            v_flex()
                .w(px(440.))
                .gap_6()
                .child(
                    v_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(64.))
                                .rounded(px(16.))
                                .bg(theme.primary)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(Lucide::LibraryBig)
                                        .size(px(34.))
                                        .text_color(theme.primary_foreground),
                                ),
                        )
                        .child(
                            div()
                                .pt_2()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .child("Welcome to calibre-rust"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(theme.muted_foreground)
                                .child("Open an existing calibre library or start a new one."),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_center()
                        .child(
                            Button::new("welcome-open")
                                .primary()
                                .icon(IconName::FolderOpen)
                                .label("Open Library…")
                                .tooltip_with_action("Open a library folder", &OpenLibrary, None)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(OpenLibrary), cx)
                                }),
                        )
                        .child(
                            Button::new("welcome-new")
                                .outline()
                                .icon(IconName::Plus)
                                .label("New Library…")
                                .tooltip_with_action("Create a library", &NewLibrary, None)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(NewLibrary), cx)
                                }),
                        ),
                )
                .when(!recent_rows.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .child(div().px_3().child(section_label("Recent", cx)))
                            .children(recent_rows),
                    )
                })
                .when(recent.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "calibre keeps its library in {} by default.",
                                default_path.display()
                            )),
                    )
                })
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(
                            "Tip: drop a library folder or ebook files anywhere in this window.",
                        ),
                ),
        )
        .into_any_element()
}

/// Invisible until files are dragged over the window.
pub fn drop_overlay(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let message = if app.library.is_some() {
        "Drop to add to your library"
    } else {
        "Drop a library folder to open it"
    };
    div()
        .id("drop-overlay")
        .absolute()
        .inset_0()
        .p_3()
        .opacity(0.)
        .drag_over::<ExternalPaths>(|style, _, _, _| style.opacity(1.))
        .child(
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded(px(12.))
                .border_2()
                .border_dashed()
                .border_color(theme.primary)
                .bg(theme.drop_target)
                .child(
                    Icon::new(Lucide::Download)
                        .size(px(36.))
                        .text_color(theme.primary),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.primary)
                        .child(message),
                ),
        )
        .into_any_element()
}

/// Lists every keyboard shortcut, read from the live keymap.
pub fn shortcuts_overlay(window: &mut Window, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();

    let row = |label: &'static str, action: &dyn Action, context: &str, window: &Window| {
        h_flex()
            .w_full()
            .py(px(3.))
            .gap_3()
            .justify_between()
            .child(div().text_sm().child(label))
            .child(
                match Kbd::binding_for_action(action, Some(context), window) {
                    Some(kbd) => kbd.into_any_element(),
                    None => div().text_xs().child("—").into_any_element(),
                },
            )
            .into_any_element()
    };
    let group = |title: &'static str, rows: Vec<AnyElement>, cx: &App| {
        v_flex()
            .gap_1()
            .child(div().pb_1().child(section_label(title, cx)))
            .children(rows)
    };

    let app_ctx = APP_CONTEXT;
    let list_ctx = &format!("{APP_CONTEXT} {LIST_CONTEXT}");
    let reader_ctx = &format!("{APP_CONTEXT} {READER_CONTEXT}");

    let library = group(
        "Library",
        vec![
            row("Add books", &AddBooks, app_ctx, window),
            row("Open library", &OpenLibrary, app_ctx, window),
            row("New library", &NewLibrary, app_ctx, window),
            row("Search", &FocusSearch, app_ctx, window),
            row("List view", &ShowAsList, app_ctx, window),
            row("Cover view", &ShowAsGrid, app_ctx, window),
            row("Toggle sidebar", &ToggleSidebar, app_ctx, window),
            row("Toggle inspector", &ToggleDetailsPane, app_ctx, window),
            row("Rescan devices", &RescanDevices, app_ctx, window),
        ],
        cx,
    );
    let books = group(
        "Selected book",
        vec![
            row("Read", &ReadBook, list_ctx, window),
            row("Edit metadata", &EditMetadata, app_ctx, window),
            row("Save changes", &SaveChanges, app_ctx, window),
            row("Open in default app", &OpenInDefaultApp, app_ctx, window),
            row(reveal_label(), &RevealInFileManager, app_ctx, window),
            row("Copy title and author", &CopyBookInfo, app_ctx, window),
            row("Export", &ExportSelected, app_ctx, window),
            row("Send to e-reader", &SendToFirstDevice, app_ctx, window),
            row("Remove", &RemoveBook, app_ctx, window),
        ],
        cx,
    );
    let navigation = group(
        "Navigation",
        vec![
            row("Previous book", &SelectPrevious, list_ctx, window),
            row("Next book", &SelectNext, list_ctx, window),
            row("First book", &SelectFirst, list_ctx, window),
            row("Last book", &SelectLast, list_ctx, window),
            row("Back / cancel", &Cancel, app_ctx, window),
            row("Show shortcuts", &ShowShortcuts, app_ctx, window),
        ],
        cx,
    );
    let reader = group(
        "Reader",
        vec![
            row("Previous chapter", &PreviousChapter, reader_ctx, window),
            row("Next chapter", &NextChapter, reader_ctx, window),
            row("Bigger text", &IncreaseFontSize, reader_ctx, window),
            row("Smaller text", &DecreaseFontSize, reader_ctx, window),
            row("Actual size", &ResetFontSize, reader_ctx, window),
            row("Toggle chapters", &ToggleChapterList, reader_ctx, window),
        ],
        cx,
    );

    div()
        .id("shortcuts-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.overlay)
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowShortcuts), cx))
        .child(
            v_flex()
                .id("shortcuts-card")
                .w(px(680.))
                .max_h(relative(0.85))
                .overflow_y_scroll()
                .p_6()
                .gap_5()
                .rounded(px(14.))
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                // Clicks inside the card should not close it.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .child("Keyboard Shortcuts"),
                        )
                        .child(tool_button(
                            "close-shortcuts",
                            IconName::Close,
                            "Close",
                            Cancel,
                        )),
                )
                .child(
                    h_flex()
                        .gap_8()
                        .items_start()
                        .child(v_flex().flex_1().gap_5().child(library).child(navigation))
                        .child(v_flex().flex_1().gap_5().child(books).child(reader)),
                ),
        )
        .into_any_element()
}
