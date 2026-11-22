//! The built-in reader.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{empty_state, section_label};
use crate::actions::*;
use crate::app::CalibreApp;

pub fn reader_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    if app.chapters.is_empty() {
        return empty_state(
            IconName::BookOpen,
            "Nothing to read",
            "Choose a book with readable text and press Read.",
            cx,
        );
    }

    let total = app.chapters.len();
    let current = &app.chapters[app.chapter_index.min(total - 1)];
    let progress = (app.chapter_index + 1) as f32 / total as f32;
    let font_size = app.settings.reader_font_size;

    let chapter_rows: Vec<AnyElement> = app
        .chapters
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            let active = index == app.chapter_index;
            h_flex()
                .id(("chapter", index))
                .w_full()
                .px_2()
                .py(px(6.))
                .gap_2()
                .items_start()
                .rounded(px(6.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .when(!active, |this| {
                    this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_chapter(index, cx)))
                .child(
                    div()
                        .w(px(22.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}", index + 1)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .line_clamp(2)
                        .child(chapter.title.clone()),
                )
                .into_any_element()
        })
        .collect();

    let paragraphs: Vec<AnyElement> = current
        .text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| {
            div()
                .w_full()
                .child(paragraph.to_string())
                .into_any_element()
        })
        .collect();

    let previous = app
        .chapter_index
        .checked_sub(1)
        .and_then(|index| app.chapters.get(index))
        .map(|chapter| chapter.title.clone());
    let next = app
        .chapters
        .get(app.chapter_index + 1)
        .map(|chapter| chapter.title.clone());

    let footer = h_flex()
        .w_full()
        .pt_8()
        .gap_3()
        .justify_between()
        .child(match previous {
            Some(title) => Button::new("footer-prev")
                .outline()
                .icon(IconName::ChevronLeft)
                .label(truncate_label(&title))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(PreviousChapter), cx))
                .into_any_element(),
            None => div().into_any_element(),
        })
        .child(match next {
            Some(title) => Button::new("footer-next")
                .primary()
                .label(format!("Next: {}", truncate_label(&title)))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NextChapter), cx))
                .into_any_element(),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("The end")
                .into_any_element(),
        });

    v_flex()
        .key_context(READER_CONTEXT)
        .track_focus(&app.reader_focus)
        .size_full()
        .child(
            div()
                .h(px(2.))
                .w_full()
                .bg(theme.border)
                .child(div().h_full().w(relative(progress)).bg(theme.primary)),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .when(app.chapter_list_visible, |this| {
                    this.child(
                        v_flex()
                            .w(px(260.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(theme.sidebar)
                            .border_r_1()
                            .border_color(theme.sidebar_border)
                            .child(
                                div()
                                    .px_4()
                                    .pt_4()
                                    .pb_2()
                                    .child(section_label(format!("Contents · {total}"), cx)),
                            )
                            .child(
                                div()
                                    .id("chapter-scroll")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .px_2()
                                    .pb_3()
                                    .child(v_flex().gap(px(1.)).children(chapter_rows)),
                            ),
                    )
                })
                .child(
                    div()
                        .id("reader-scroll")
                        .flex_1()
                        .h_full()
                        .overflow_y_scroll()
                        .track_scroll(&app.reader_scroll)
                        .child(
                            v_flex().w_full().items_center().px_10().py_12().child(
                                v_flex()
                                    .w_full()
                                    .max_w(px(font_size * 40.))
                                    .gap(px(font_size * 0.9))
                                    .text_size(px(font_size))
                                    .line_height(relative(1.65))
                                    .child(
                                        div().text_xs().text_color(theme.muted_foreground).child(
                                            format!("Chapter {} of {total}", app.chapter_index + 1),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(font_size * 1.6))
                                            .line_height(relative(1.25))
                                            .font_weight(FontWeight::BOLD)
                                            .child(current.title.clone()),
                                    )
                                    .children(paragraphs)
                                    .child(footer),
                            ),
                        ),
                ),
        )
        .into_any_element()
}

fn truncate_label(title: &str) -> String {
    const MAX: usize = 36;
    if title.chars().count() > MAX {
        format!("{}…", title.chars().take(MAX).collect::<String>())
    } else {
        title.to_string()
    }
}
