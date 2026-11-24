//! Desktop preferences and their immediate application to the main window.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::*;
use gpui_kit::*;

use super::reveal::{revealed, FollowFocus as _, RevealOnFocus as _};
use crate::actions::ClearRecentLibraries;
use crate::app::CalibreApp;
use crate::settings::{
    config_dir, ExportCollision, ExportLayout, ImportDuplicates, OpenExternalBook, ReaderSpacing,
    ReaderWidth, Settings, StartupLibrary, ThemePreference, ViewMode, DEFAULT_READER_FONT_SIZE,
    MAX_READER_FONT_SIZE, MIN_READER_FONT_SIZE,
};

pub fn preferences_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let s = &app.settings;
    let muted = cx.theme().muted_foreground;
    let font = s.reader_font_size;
    let theme = choices(
        "appearance",
        s.theme,
        [
            ("System", ThemePreference::System),
            ("Light", ThemePreference::Light),
            ("Dark", ThemePreference::Dark),
        ],
        CalibreApp::set_theme,
        cx,
    );
    let layout = choices(
        "library-layout",
        s.view,
        [("List", ViewMode::List), ("Covers", ViewMode::Grid)],
        |app, view, _, cx| app.set_view(view, cx),
        cx,
    );
    let startup = choices(
        "startup-library",
        s.startup_library,
        [
            ("Last used", StartupLibrary::LastUsed),
            ("Calibre Library", StartupLibrary::Default),
            ("Welcome screen", StartupLibrary::Ask),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.startup_library = value, cx),
        cx,
    );
    let duplicates = choices(
        "duplicate-books",
        s.import_duplicates,
        [
            ("Merge formats", ImportDuplicates::Merge),
            ("Keep both", ImportDuplicates::KeepBoth),
            ("Skip", ImportDuplicates::Skip),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.import_duplicates = value, cx),
        cx,
    );
    let external = choices(
        "external-ebooks",
        s.open_external_book,
        [
            ("Import", OpenExternalBook::Import),
            ("Read", OpenExternalBook::Read),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.open_external_book = value, cx),
        cx,
    );
    let export_layout = choices(
        "export-layout",
        s.export_layout,
        [
            ("By book", ExportLayout::BookFolders),
            ("By author", ExportLayout::AuthorFolders),
            ("One folder", ExportLayout::Flat),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.export_layout = value, cx),
        cx,
    );
    let export_collision = choices(
        "export-collision",
        s.export_collision,
        [
            ("Rename", ExportCollision::Rename),
            ("Skip", ExportCollision::Skip),
            ("Replace", ExportCollision::Replace),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.export_collision = value, cx),
        cx,
    );
    let spacing = choices(
        "reader-spacing",
        s.reader_spacing,
        [
            ("Compact", ReaderSpacing::Compact),
            ("Normal", ReaderSpacing::Normal),
            ("Airy", ReaderSpacing::Airy),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.reader_spacing = value, cx),
        cx,
    );
    let width = choices(
        "reader-width",
        s.reader_width,
        [
            ("Narrow", ReaderWidth::Narrow),
            ("Standard", ReaderWidth::Standard),
            ("Wide", ReaderWidth::Wide),
        ],
        |app, value, _, cx| app.update_preferences(|s| s.reader_width = value, cx),
        cx,
    );
    let font_size = h_flex()
        .gap_2()
        .child(
            Button::new("font-decrease")
                .outline()
                .small()
                .label("−")
                .accessibility_label("Decrease text size")
                .disabled(font <= MIN_READER_FONT_SIZE)
                .on_click(cx.listener(|app, _, _, cx| app.change_font_size(-1., cx))),
        )
        .child(
            div()
                .w(px(54.))
                .text_center()
                .child(format!("{font:.0} px")),
        )
        .child(
            Button::new("font-increase")
                .outline()
                .small()
                .label("+")
                .accessibility_label("Increase text size")
                .disabled(font >= MAX_READER_FONT_SIZE)
                .on_click(cx.listener(|app, _, _, cx| app.change_font_size(1., cx))),
        )
        .child(
            Button::new("font-reset")
                .ghost()
                .small()
                .label("Reset")
                .disabled(font == DEFAULT_READER_FONT_SIZE)
                .on_click(cx.listener(|app, _, _, cx| app.change_font_size(0., cx))),
        );
    let config = config_dir();
    let config_path = config
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Unavailable".into());

    div()
        .id("preferences-scroll")
        .size_full()
        .overflow_y_scroll()
        .follow_focus(&app.focus_reveal.scroll("preferences-scroll"))
        .child(
            v_flex()
                .w_full()
                .max_w(px(820.))
                .mx_auto()
                .px_8()
                .py_6()
                .gap_6()
                .child(
                    section("Appearance", cx)
                        .child(row("Theme", theme))
                        .child(row("Book layout", layout))
                        .child(toggle(
                            "sidebar",
                            "Show library sidebar",
                            s.sidebar_visible,
                            |s, value| s.sidebar_visible = value,
                            cx,
                        ))
                        .child(toggle(
                            "details",
                            "Show book details",
                            s.inspector_visible,
                            |s, value| s.inspector_visible = value,
                            cx,
                        )),
                )
                .child(
                    section("Startup", cx)
                        .child(row("Open on launch", startup))
                        .child(toggle(
                            "restore-window",
                            "Restore window size and position",
                            s.restore_window,
                            |s, value| s.restore_window = value,
                            cx,
                        ))
                        .child(toggle(
                            "device-scan",
                            "Automatically look for e-readers",
                            s.auto_detect_devices,
                            |s, value| s.auto_detect_devices = value,
                            cx,
                        )),
                )
                .child(
                    section("Import and export", cx)
                        .child(row("When the system opens one ebook", external))
                        .child(row("Duplicate books", duplicates))
                        .child(row("Export folder layout", export_layout))
                        .child(row("Existing exported files", export_collision))
                        .child(toggle(
                            "export-cover",
                            "Save cover images with exported books",
                            s.export_cover,
                            |s, value| s.export_cover = value,
                            cx,
                        ))
                        .child(toggle(
                            "export-opf",
                            "Save OPF metadata with exported books",
                            s.export_metadata,
                            |s, value| s.export_metadata = value,
                            cx,
                        ))
                        .child(toggle(
                            "reveal-export",
                            "Show the export folder when finished",
                            s.reveal_export,
                            |s, value| s.reveal_export = value,
                            cx,
                        )),
                )
                .child(
                    section("Reader", cx)
                        .child(row("Text size", font_size))
                        .child(row("Line spacing", spacing))
                        .child(row("Text width", width))
                        .child(toggle(
                            "reader-contents",
                            "Open the reader with the sidebar",
                            s.reader_contents,
                            |s, value| s.reader_contents = value,
                            cx,
                        ))
                        .child(toggle(
                            "reader-resume",
                            "Resume from the last reading position",
                            s.reader_resume,
                            |s, value| s.reader_resume = value,
                            cx,
                        ))
                        .child(toggle(
                            "publisher-styles",
                            "Use publisher typography and spacing",
                            s.publisher_styles,
                            |s, value| s.publisher_styles = value,
                            cx,
                        )),
                )
                .child(section("AI Assistant", cx).child(super::online::preferences(app, cx)))
                .child(
                    section("Online metadata", cx)
                        .child(super::online::metadata_preferences(app, cx)),
                )
                .child(
                    section("Settings storage", cx)
                        .child(div().text_sm().text_color(muted).child(config_path))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("copy-config-path")
                                        .outline()
                                        .small()
                                        .label("Copy Folder Path")
                                        .disabled(config.is_none())
                                        .on_click(move |_, _, cx| {
                                            if let Some(path) = &config {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    path.display().to_string(),
                                                ));
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("clear-recent")
                                        .outline()
                                        .small()
                                        .label("Clear Recent Libraries")
                                        .disabled(s.recent_libraries.is_empty())
                                        .on_click(|_, window, cx| {
                                            window
                                                .dispatch_action(Box::new(ClearRecentLibraries), cx)
                                        }),
                                )
                                .child(
                                    Button::new("reset-preferences")
                                        .outline()
                                        .small()
                                        .label("Restore Defaults")
                                        .on_click(cx.listener(|app, _, window, cx| {
                                            app.reset_preferences(window, cx)
                                        })),
                                )
                                .reveal_on_focus(),
                        ),
                ),
        )
        .into_any_element()
}

fn section(title: &'static str, cx: &App) -> Div {
    v_flex()
        .gap_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .pt_4()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
}

fn row(label: &'static str, control: impl IntoElement) -> Div {
    h_flex()
        .gap_4()
        .items_center()
        .justify_between()
        .text_sm()
        .child(label)
        .child(control)
        .reveal_on_focus()
}

fn choices<T: Copy + Eq + 'static, const N: usize>(
    id: &'static str,
    selected: T,
    options: [(&'static str, T); N],
    set: fn(&mut CalibreApp, T, &mut Window, &mut Context<CalibreApp>),
    cx: &mut Context<CalibreApp>,
) -> Div {
    h_flex().gap_1().children(
        options
            .into_iter()
            .enumerate()
            .map(|(index, (label, value))| {
                let button = Button::new((id, index))
                    .small()
                    .label(label)
                    .selected(value == selected);
                let button = if value == selected {
                    button.primary()
                } else {
                    button.outline()
                };
                button.on_click(cx.listener(move |app, _, window, cx| set(app, value, window, cx)))
            }),
    )
}

fn toggle(
    id: &'static str,
    label: &'static str,
    checked: bool,
    set: fn(&mut Settings, bool),
    cx: &mut Context<CalibreApp>,
) -> Div {
    revealed(
        Checkbox::new(id)
            .label(label)
            .checked(checked)
            .on_click(cx.listener(move |app, value: &bool, _, cx| {
                app.update_preferences(|settings| set(settings, *value), cx);
            })),
    )
}
