//! The EPUB source editor.

use gpui_kit::component::input::Textarea;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{empty_state, section_label};
use crate::app::{format_size, CalibreApp};

pub fn epub_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(current) = app
        .epub_selected
        .and_then(|index| app.epub_files.get(index))
    else {
        return empty_state(
            IconName::File,
            "No file selected",
            "Choose a file on the left.",
            cx,
        );
    };

    let rows: Vec<AnyElement> = app
        .epub_files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let active = app.epub_selected == Some(index);
            let (folder, name) = match file.relative.rsplit_once(['/', '\\']) {
                Some((folder, name)) => (folder.to_string(), name.to_string()),
                None => (String::new(), file.relative.clone()),
            };
            h_flex()
                .id(("epub-file", index))
                .w_full()
                .px_2()
                .py(px(5.))
                .gap_2()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                })
                .when(!active, |this| {
                    this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.load_epub_file(index, window, cx)),
                )
                .child(Icon::new(IconName::FileText).xsmall())
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(name))
                        .when(!folder.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(folder),
                            )
                        }),
                )
                .into_any_element()
        })
        .collect();

    h_flex()
        .size_full()
        .child(
            v_flex()
                .w(px(280.))
                .h_full()
                .flex_shrink_0()
                .bg(theme.sidebar)
                .border_r_1()
                .border_color(theme.sidebar_border)
                .child(div().px_4().pt_4().pb_2().child(section_label(
                    format!("Files · {}", app.epub_files.len()),
                    cx,
                )))
                .child(
                    div()
                        .id("epub-file-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .px_2()
                        .pb_3()
                        .child(v_flex().gap(px(1.)).children(rows)),
                ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .p_4()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(current.relative.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format_size(current.size)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(Textarea::new(&app.epub_editor).w_full().h_full()),
                ),
        )
        .into_any_element()
}
