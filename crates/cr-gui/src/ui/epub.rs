//! The EPUB source editor.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{empty_state, section_label};
use crate::app::{format_size, CalibreApp};
use crate::tools::{EditorPanel, TocChange};

pub fn epub_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let current = app
        .epub_selected
        .and_then(|index| app.epub_files.get(index));

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

    let panel = app.tools.editor_panel;
    let tabs = h_flex().px_2().pt_3().pb_1().gap_1().children(
        [
            ("Files", EditorPanel::Files),
            ("Search", EditorPanel::Search),
            ("Contents", EditorPanel::Contents),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (label, which))| {
            let button = Button::new(("editor-panel", index)).small().label(label);
            if panel == which {
                button.primary()
            } else {
                button.ghost()
            }
            .on_click(cx.listener(move |this, _, _, cx| this.show_editor_panel(which, cx)))
        }),
    );
    let side = match panel {
        EditorPanel::Files => v_flex()
            .flex_1()
            .min_h_0()
            .child(div().px_4().pt_2().pb_2().child(section_label(
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
            )
            .into_any_element(),
        EditorPanel::Search => search_panel(app, cx),
        EditorPanel::Contents => contents_panel(app, cx),
    };

    h_flex()
        .size_full()
        .child(
            v_flex()
                .w(px(300.))
                .h_full()
                .flex_shrink_0()
                .bg(theme.sidebar)
                .border_r_1()
                .border_color(theme.sidebar_border)
                .child(tabs)
                .child(side),
        )
        .child(match current {
            Some(current) => v_flex()
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
                )
                .into_any_element(),
            None => empty_state(
                IconName::File,
                "No file selected",
                "Choose a file on the left.",
                cx,
            ),
        })
        .into_any_element()
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

/// Search and replace across every text file of the book.
fn search_panel(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let tools = &app.tools;
    let matches = tools.editor_matches.as_deref();
    let rows: Vec<AnyElement> =
        matches
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                v_flex()
                    .id(("editor-match", index))
                    .w_full()
                    .px_2()
                    .py(px(5.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_editor_match(index, window, cx)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · line {}", hit.file, hit.line)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(hit.text.trim().to_string()),
                    )
                    .into_any_element()
            })
            .collect();

    v_flex()
        .flex_1()
        .min_h_0()
        .child(
            v_flex()
                .px_3()
                .pt_2()
                .pb_2()
                .gap_2()
                .child(Input::new(&tools.editor_find).small().w_full())
                .child(Input::new(&tools.editor_replace).small().w_full())
                .child(
                    h_flex()
                        .gap_3()
                        .child(toggle(
                            "editor-regex",
                            "Regex",
                            tools.editor_regex,
                            cx,
                            |this, on| {
                                this.tools.editor_regex = on;
                                this.tools.editor_matches = None;
                            },
                        ))
                        .child(toggle(
                            "editor-case",
                            "Match case",
                            tools.editor_case,
                            cx,
                            |this, on| {
                                this.tools.editor_case = on;
                                this.tools.editor_matches = None;
                            },
                        )),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("editor-find-all")
                                .small()
                                .primary()
                                .label("Find All")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editor_find_all(window, cx)
                                })),
                        )
                        .child(
                            Button::new("editor-replace-all")
                                .small()
                                .outline()
                                .label("Replace All")
                                .tooltip("Replace every match in every file of the book")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editor_replace_all(window, cx)
                                })),
                        ),
                )
                .child(section_label(
                    match matches {
                        None => "Matches".to_string(),
                        Some(found) => format!("Matches · {}", found.len()),
                    },
                    cx,
                )),
        )
        .child(
            div()
                .id("editor-match-scroll")
                .flex_1()
                .overflow_y_scroll()
                .px_2()
                .pb_3()
                .when(matches.is_some_and(|found| found.is_empty()), |this| {
                    this.child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Nothing found."),
                    )
                })
                .child(v_flex().gap(px(1.)).children(rows)),
        )
        .into_any_element()
}

/// The book's table of contents, with tools to rebuild and rearrange it.
fn contents_panel(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let tools = &app.tools;
    let selected = tools.editor_toc_selected;
    let rows: Vec<AnyElement> =
        tools
            .editor_toc
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let active = selected == Some(index);
                div()
                    .id(("toc-entry", index))
                    .w_full()
                    .pl(px(
                        8. + 14. * f32::from(entry.level.saturating_sub(1).min(6))
                    ))
                    .pr_2()
                    .py(px(4.))
                    .rounded(px(6.))
                    .text_sm()
                    .truncate()
                    .cursor_pointer()
                    .when(active, |this| {
                        this.bg(theme.sidebar_accent)
                            .text_color(theme.sidebar_accent_foreground)
                    })
                    .when(!active, |this| {
                        this.hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.5)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_toc_entry(index, window, cx)
                    }))
                    .child(entry.title.clone())
                    .into_any_element()
            })
            .collect();
    let change = |id: &'static str,
                  label: &'static str,
                  tooltip: &'static str,
                  change: TocChange,
                  cx: &mut Context<CalibreApp>| {
        Button::new(id)
            .xsmall()
            .outline()
            .label(label)
            .tooltip(tooltip)
            .disabled(selected.is_none())
            .on_click(
                cx.listener(move |this, _, window, cx| this.edit_toc_entry(change, window, cx)),
            )
    };
    let build = |id: &'static str, label: &'static str, depth: u8, cx: &mut Context<CalibreApp>| {
        Button::new(id)
            .xsmall()
            .outline()
            .label(label)
            .tooltip("Replace the contents with the book's headings down to this level")
            .on_click(
                cx.listener(move |this, _, window, cx| this.generate_editor_toc(depth, window, cx)),
            )
    };

    v_flex()
        .flex_1()
        .min_h_0()
        .child(
            v_flex()
                .px_3()
                .pt_2()
                .pb_2()
                .gap_2()
                .child(section_label("Build from headings", cx))
                .child(
                    h_flex()
                        .gap_1()
                        .child(build("toc-build-1", "h1", 1, cx))
                        .child(build("toc-build-2", "h1–h2", 2, cx))
                        .child(build("toc-build-3", "h1–h3", 3, cx)),
                )
                .child(section_label("Selected entry", cx))
                .child(Input::new(&tools.editor_toc_title).small().w_full())
                .child(
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .child(change(
                            "toc-rename",
                            "Rename",
                            "Use the typed title",
                            TocChange::Rename,
                            cx,
                        ))
                        .child(change("toc-up", "Up", "Move up", TocChange::Up, cx))
                        .child(change("toc-down", "Down", "Move down", TocChange::Down, cx))
                        .child(change(
                            "toc-indent",
                            "Indent",
                            "Nest under the entry above",
                            TocChange::Indent,
                            cx,
                        ))
                        .child(change(
                            "toc-outdent",
                            "Outdent",
                            "Move out one level",
                            TocChange::Outdent,
                            cx,
                        ))
                        .child(change(
                            "toc-remove",
                            "Remove",
                            "Remove from the contents",
                            TocChange::Remove,
                            cx,
                        )),
                )
                .child(section_label(
                    format!("Contents · {}", tools.editor_toc.len()),
                    cx,
                )),
        )
        .child(
            div()
                .id("toc-scroll")
                .flex_1()
                .overflow_y_scroll()
                .px_2()
                .pb_3()
                .when(rows.is_empty(), |this| {
                    this.child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                "The book has no table of contents. Build one from its headings.",
                            ),
                    )
                })
                .child(v_flex().gap(px(1.)).children(rows)),
        )
        .into_any_element()
}
