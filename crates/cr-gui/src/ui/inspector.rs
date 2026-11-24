//! The inspector: details of the selected book, or the metadata editor.

use cr_core::formats::editing::EDITABLE_FORMATS;
use cr_core::formats::html;
use cr_core::library::{PICTURE_FORMATS, READABLE_FORMATS};
use cr_core::text;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::library::book_context_menu;
use super::widgets::{chip, cover_thumb, detail_row, empty_state, field, section_label, stars};
use super::INSPECTOR_WIDTH;
use crate::actions::*;
use crate::app::{CalibreApp, Shelf};
use crate::files;

pub fn inspector(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let content = if app.metadata.is_none() {
        empty_state(
            IconName::BookOpen,
            "No book selected",
            "Select a book to see its details.",
            cx,
        )
    } else if app.editing {
        edit_form(app, cx)
    } else {
        details(app, cx)
    };
    div()
        .id("inspector")
        .w(px(INSPECTOR_WIDTH))
        .h_full()
        .flex_shrink_0()
        .overflow_y_scroll()
        .bg(theme.sidebar)
        .border_l_1()
        .border_color(theme.sidebar_border)
        .child(content)
        .into_any_element()
}

fn details(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(metadata) = &app.metadata else {
        return div().into_any_element();
    };

    let cover = div()
        .id("inspector-cover")
        .p(px(4.))
        .rounded(px(10.))
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .shadow_md()
        .cursor_pointer()
        .tooltip(|window, cx| {
            gpui_kit::component::tooltip::Tooltip::new("Drop an image here to replace the cover")
                .build(window, cx)
        })
        .drag_over::<ExternalPaths>(|style, _, _, cx| {
            style
                .border_color(cx.theme().primary)
                .bg(cx.theme().drop_target)
        })
        .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
            cx.stop_propagation();
            let image = paths
                .paths()
                .iter()
                .find(|path| files::has_extension(path, files::IMAGE_EXTENSIONS))
                .cloned();
            match image {
                Some(path) => this.set_cover_from_file(&path, window, cx),
                None => this.open_paths(paths.paths().to_vec(), window, cx),
            }
        }))
        .on_click(|event, window, cx| {
            if event.click_count() == 2 {
                window.dispatch_action(Box::new(ReadBook), cx);
            }
        })
        .child(cover_thumb(
            app.cover_path.clone(),
            &metadata.title,
            168.,
            252.,
            cx,
        ));

    let authors: Vec<AnyElement> = if metadata.authors.is_empty() {
        vec![div()
            .text_color(theme.muted_foreground)
            .child("Unknown author")
            .into_any_element()]
    } else {
        metadata
            .authors
            .iter()
            .enumerate()
            .map(|(index, author)| {
                let shelf = Shelf::Author(author.clone());
                div()
                    .id(("author-link", index))
                    .text_color(theme.link)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_shelf(shelf.clone(), window, cx)
                    }))
                    .child(author.clone())
                    .into_any_element()
            })
            .collect()
    };

    let has_files = !app.selected_formats.is_empty();
    let actions = h_flex()
        .gap_2()
        .child(
            Button::new("inspector-read")
                .primary()
                .small()
                .icon(IconName::BookOpen)
                .label("Read")
                .disabled(!has_files)
                .tooltip_with_action("Read in calibre-rust", &ReadBook, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ReadBook), cx)),
        )
        .child(
            Button::new("inspector-edit")
                .outline()
                .small()
                .icon(Lucide::SquarePen)
                .label("Edit")
                .tooltip_with_action("Edit metadata", &EditMetadata, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(EditMetadata), cx)),
        )
        .child(
            Button::new("inspector-open")
                .outline()
                .small()
                .icon(IconName::ExternalLink)
                .disabled(!has_files)
                .tooltip_with_action("Open in default app", &OpenInDefaultApp, None)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenInDefaultApp), cx)),
        )
        .child(
            Button::new("inspector-more")
                .ghost()
                .small()
                .icon(IconName::Ellipsis)
                .tooltip("More actions")
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    book_context_menu(this).show(event.position(), window, cx);
                })),
        );

    let mut header = v_flex().items_center().gap_3().child(cover).child(
        v_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .text_center()
                    .child(metadata.title.clone()),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_x_2()
                    .text_sm()
                    .children(authors),
            ),
    );
    if let Some(series) = metadata.series.clone() {
        let shelf = Shelf::Series(series.clone());
        header = header.child(
            div()
                .id("series-link")
                .text_sm()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|style| style.underline())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_shelf(shelf.clone(), window, cx)
                }))
                .child(format!(
                    "Book {} of {series}",
                    text::format_series_index(metadata.series_index)
                )),
        );
    }
    if let Some(rating) = metadata.rating.filter(|rating| *rating > 0) {
        header = header.child(stars(rating, 14., cx));
    }
    header = header.child(actions);

    // Details table.
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut add_row = |label: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            rows.push(detail_row(label, value, cx));
        }
    };
    add_row("Publisher", metadata.publisher.clone());
    add_row(
        "Published",
        metadata
            .pubdate
            .as_ref()
            .map(|date| date.format("%-d %B %Y").to_string()),
    );
    add_row(
        "Added",
        metadata
            .timestamp
            .as_ref()
            .map(|date| date.format("%-d %B %Y").to_string()),
    );
    add_row(
        "Languages",
        Some(metadata.languages.join(", ")).filter(|value| !value.is_empty()),
    );
    add_row(
        "Identifiers",
        Some(
            metadata
                .identifiers
                .iter()
                .map(|(kind, value)| format!("{kind}: {value}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    );

    let mut table = v_flex().gap_2().children(rows);
    let mut formats: Vec<AnyElement> = app
        .selected_formats
        .iter()
        .enumerate()
        .map(|(index, format)| {
            let format = format.clone();
            chip(("format-chip", index), format.clone(), cx)
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().secondary_hover))
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Format actions").build(window, cx)
                })
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    let action = |command| Box::new(BookFormatAction(command, format.clone()));
                    NativeMenu::new()
                        .menu_with_disabled(
                            "Read This Format",
                            !READABLE_FORMATS.contains(&format.as_str())
                                && !PICTURE_FORMATS.contains(&format.as_str()),
                            action(FormatCommand::Read),
                        )
                        .menu("Open in Default App", action(FormatCommand::Open))
                        .menu(reveal_label(), action(FormatCommand::Reveal))
                        .menu("Copy File Path", action(FormatCommand::CopyPath))
                        .separator()
                        .menu_with_disabled(
                            "Edit This Format…",
                            !EDITABLE_FORMATS.contains(&format.as_str()),
                            action(FormatCommand::Edit),
                        )
                        .menu_with_disabled(
                            "Remove This Format…",
                            !this.can_write() || this.job.is_some(),
                            action(FormatCommand::Remove),
                        )
                        .show(event.position(), window, cx);
                }))
                .into_any_element()
        })
        .collect();
    if let Some(source) = ["EPUB", "KEPUB"]
        .into_iter()
        .find(|source| app.selected_formats.iter().any(|format| format == source))
    {
        formats.push(
            Button::new("convert-to-azw3")
                .outline()
                .small()
                .label("Convert to AZW3")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.save_conversion(source, "azw3", window, cx)
                }))
                .into_any_element(),
        );
    }
    if let Some(source) = ["AZW3", "AZW", "MOBI", "PRC"]
        .into_iter()
        .find(|source| app.selected_formats.iter().any(|format| format == source))
    {
        formats.push(
            Button::new("convert-to-epub")
                .outline()
                .small()
                .label("Convert to EPUB")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.save_conversion(source, "epub", window, cx)
                }))
                .into_any_element(),
        );
    }
    table = table.child(detail_row(
        "Formats",
        if formats.is_empty() {
            div()
                .text_color(theme.muted_foreground)
                .child("None")
                .into_any_element()
        } else {
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(formats)
                .into_any_element()
        },
        cx,
    ));
    table = table.child(detail_row(
        "Location",
        div()
            .id("reveal-link")
            .text_color(theme.link)
            .cursor_pointer()
            .hover(|style| style.underline())
            .on_click(cx.listener(|this, _, _, cx| this.reveal_selected(cx)))
            .child(reveal_label()),
        cx,
    ));

    let tags: Vec<AnyElement> = metadata
        .tags
        .iter()
        .enumerate()
        .map(|(index, tag)| {
            let shelf = Shelf::Tag(tag.clone());
            chip(("tag-chip", index), tag.clone(), cx)
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().secondary_hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_shelf(shelf.clone(), window, cx)
                }))
                .into_any_element()
        })
        .collect();

    let description = metadata
        .comments
        .as_deref()
        .map(html::html_to_text)
        .unwrap_or_default();
    let paragraphs: Vec<AnyElement> = description
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| div().child(paragraph.to_string()).into_any_element())
        .collect();

    v_flex()
        .p_5()
        .gap_5()
        .child(header)
        .child(div().h(px(1.)).bg(theme.border))
        .child(table)
        .when(!tags.is_empty(), |this| {
            this.child(
                v_flex()
                    .gap_2()
                    .child(section_label("Tags", cx))
                    .child(h_flex().gap_1().flex_wrap().children(tags)),
            )
        })
        .when(!paragraphs.is_empty(), |this| {
            this.child(
                v_flex()
                    .gap_2()
                    .child(section_label("Description", cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .text_sm()
                            .line_height(relative(1.5))
                            .children(paragraphs),
                    ),
            )
        })
        .into_any_element()
}

fn edit_form(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let input = |label: &str, state: &Entity<InputState>, cx: &App| {
        field(label, Input::new(state).small().w_full(), cx)
    };

    v_flex()
        .p_5()
        .gap_4()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_base()
                        .font_weight(FontWeight::BOLD)
                        .child("Edit Metadata"),
                )
                .child(
                    Button::new("edit-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .tooltip_with_action("Discard changes", &Cancel, None)
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                )
                .child(
                    Button::new("edit-save")
                        .small()
                        .primary()
                        .label("Save")
                        .tooltip_with_action("Save metadata", &SaveChanges, None)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SaveChanges), cx)
                        }),
                ),
        )
        .child(input("Title", &app.title_input, cx))
        .child(input("Authors", &app.authors_input, cx))
        .child(
            h_flex()
                .gap_3()
                .child(div().flex_1().child(input("Series", &app.series_input, cx)))
                .child(
                    div()
                        .w(px(72.))
                        .child(input("Number", &app.series_index_input, cx)),
                ),
        )
        .child(input("Publisher", &app.publisher_input, cx))
        .child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .child(input("Published", &app.pubdate_input, cx)),
                )
                .child(
                    div()
                        .w(px(96.))
                        .child(input("Rating", &app.rating_input, cx)),
                ),
        )
        .child(input("Tags", &app.tags_input, cx))
        .child(input("Languages", &app.languages_input, cx))
        .child(input("Identifiers", &app.identifiers_input, cx))
        .child(field(
            "Description",
            Textarea::new(&app.comments_input).w_full().h(px(180.)),
            cx,
        ))
        .into_any_element()
}
