//! The built-in reader.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::ops::Range;

use cr_core::formats::reading::{Block, BlockKind, Book, Style};
use cr_core::library::AnnotationKind;
use gpui_kit::component::input::Input;

use super::widgets::{empty_state, section_label};
use crate::actions::*;
use crate::app::{CalibreApp, ReaderPanel};

pub fn reader_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(book) = app.book.clone() else {
        return empty_state(
            IconName::BookOpen,
            "Nothing to read",
            "Choose a book with readable text and press Read.",
            cx,
        );
    };
    let total = book.sections.len();
    let section_index = app.section_index.min(total.saturating_sub(1));
    let section = &book.sections[section_index];
    let here = app.reader_locator();
    let progress = book.fraction(here) as f32;
    let font_size = app.settings.reader_font_size;
    if let Some(block) = app.reader_jump.take() {
        app.reader_scroll.scroll_to_top_of_item(block);
    }

    let panel_rows: Vec<AnyElement> = match app.reader_panel {
        ReaderPanel::Contents => {
            let current = book.toc_index(here);
            book.toc
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let target = entry.target;
                    panel_row(
                        ("toc", index),
                        Some(index) == current,
                        entry.level.saturating_sub(1),
                        entry.title.clone(),
                        None,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go_to(target, cx)))
                    .into_any_element()
                })
                .collect()
        }
        ReaderPanel::Notes => app
            .reader_annotations
            .iter()
            .enumerate()
            .map(|(index, annotation)| {
                let (target, id) = (annotation.start, annotation.id);
                let kind = match annotation.kind {
                    AnnotationKind::Bookmark => "Bookmark",
                    AnnotationKind::Highlight => "Highlight",
                };
                let detail = if annotation.chapter.is_empty() {
                    kind.to_string()
                } else {
                    format!("{kind} · {}", annotation.chapter)
                };
                panel_row(
                    ("note", index),
                    false,
                    0,
                    annotation.text.clone(),
                    Some(detail),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go_to(target, cx)))
                .child(
                    Button::new(("note-remove", index))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.remove_annotation(id, cx)
                        })),
                )
                .into_any_element()
            })
            .collect(),
        ReaderPanel::Search => app
            .reader_hits
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let detail = format!("{:.0}%", book.fraction(hit.start) * 100.0);
                panel_row(
                    ("hit", index),
                    index == app.reader_hit,
                    0,
                    hit.snippet.clone(),
                    Some(detail),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.show_hit(index, cx)))
                .into_any_element()
            })
            .collect(),
    };
    let panel_empty = match app.reader_panel {
        ReaderPanel::Contents => "",
        ReaderPanel::Notes => "No bookmarks or highlights yet.",
        ReaderPanel::Search => "Type to search this book.",
    };
    let tab = |id: &'static str, label: String, panel: ReaderPanel| {
        let button = Button::new(id).small().label(label);
        if app.reader_panel == panel {
            button.primary()
        } else {
            button.ghost()
        }
        .on_click(cx.listener(move |this, _, _, cx| {
            this.reader_panel = panel;
            cx.notify();
        }))
    };

    let tabs = h_flex()
        .px_3()
        .pt_3()
        .pb_2()
        .gap_1()
        .child(tab(
            "panel-contents",
            "Contents".into(),
            ReaderPanel::Contents,
        ))
        .child(tab(
            "panel-notes",
            format!("Notes · {}", app.reader_annotations.len()),
            ReaderPanel::Notes,
        ))
        .child(tab("panel-search", "Search".into(), ReaderPanel::Search));

    let blocks: Vec<AnyElement> = section
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let marked: Vec<Range<usize>> = app
                .reader_annotations
                .iter()
                .filter(|a| {
                    a.kind == AnnotationKind::Highlight
                        && a.start.section == section_index
                        && a.start.block == index
                })
                .map(|a| a.start.offset..a.end)
                .collect();
            let found: Vec<Range<usize>> = app
                .reader_hits
                .iter()
                .filter(|hit| hit.start.section == section_index && hit.start.block == index)
                .map(|hit| hit.start.offset..hit.end)
                .collect();
            let bookmarked = app.reader_annotations.iter().any(|a| {
                a.kind == AnnotationKind::Bookmark
                    && a.start.section == section_index
                    && a.start.block == index
            });
            let content = block_element(app, block, index, &marked, &found, font_size, cx);
            div()
                .id(("block", index))
                .w_full()
                .max_w(px(font_size * 40.))
                .pb(px(font_size * 0.9))
                .pl(px(f32::from(block.indent) * font_size * 1.4 + 8.))
                .border_l_2()
                .border_color(if app.reader_block == Some(index) {
                    theme.primary
                } else if bookmarked {
                    theme.warning
                } else {
                    theme.transparent
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select_reader_block(index, cx)))
                .child(content)
                .into_any_element()
        })
        .collect();

    let previous = section_index.checked_sub(1).map(|index| index + 1);
    let next = (section_index + 1 < total).then_some(section_index + 2);
    let footer = h_flex()
        .w_full()
        .max_w(px(font_size * 40.))
        .pt_8()
        .gap_3()
        .justify_between()
        .child(match previous {
            Some(number) => Button::new("footer-prev")
                .outline()
                .icon(IconName::ChevronLeft)
                .label(truncate_label(&section_name(&book, number - 1)))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(PreviousChapter), cx))
                .into_any_element(),
            None => div().into_any_element(),
        })
        .child(match next {
            Some(number) => Button::new("footer-next")
                .primary()
                .label(format!(
                    "Next: {}",
                    truncate_label(&section_name(&book, number - 1))
                ))
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
                            .w(px(280.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(theme.sidebar)
                            .border_r_1()
                            .border_color(theme.sidebar_border)
                            .child(tabs)
                            .when(app.reader_panel == ReaderPanel::Search, |this| {
                                this.child(
                                    v_flex()
                                        .px_3()
                                        .pb_2()
                                        .gap_1()
                                        .child(Input::new(&app.reader_search).small())
                                        .child(section_label(
                                            format!("{} match(es)", app.reader_hits.len()),
                                            cx,
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .id("chapter-scroll")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .px_2()
                                    .pb_3()
                                    .when(panel_rows.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .px_2()
                                                .text_sm()
                                                .text_color(theme.muted_foreground)
                                                .child(panel_empty),
                                        )
                                    })
                                    .child(v_flex().gap(px(1.)).children(panel_rows)),
                            ),
                    )
                })
                .child(
                    v_flex()
                        .id("reader-scroll")
                        .flex_1()
                        .h_full()
                        .items_center()
                        .px_10()
                        .py_12()
                        .overflow_y_scroll()
                        .track_scroll(&app.reader_scroll)
                        .text_size(px(font_size))
                        .line_height(relative(1.65))
                        // Blocks must stay direct children: the scroll
                        // position is saved as the index of the top one.
                        .children(blocks)
                        .child(footer),
                ),
        )
        .into_any_element()
}

fn section_name(book: &Book, index: usize) -> String {
    let title = &book.sections[index].title;
    if title.is_empty() {
        format!("Section {}", index + 1)
    } else {
        title.clone()
    }
}

fn panel_row(
    id: impl Into<ElementId>,
    active: bool,
    indent: u8,
    title: String,
    detail: Option<String>,
    cx: &mut Context<CalibreApp>,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    h_flex()
        .id(id)
        .w_full()
        .pr_2()
        .pl(px(8. + f32::from(indent.min(4)) * 14.))
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
            this.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().line_clamp(3).child(title))
                .when_some(detail, |this, detail| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                }),
        )
}

/// Flatten overlapping style spans, highlights and search matches into the
/// ordered, non-overlapping runs the text renderer needs.
pub(crate) fn text_runs(
    block: &Block,
    marked: &[Range<usize>],
    found: &[Range<usize>],
) -> Vec<(Range<usize>, Vec<Mark>)> {
    let length = block.text.len();
    let mut all: Vec<(Range<usize>, Mark)> = block
        .spans
        .iter()
        .map(|span| (span.start..span.end, Mark::Style(span.style.clone())))
        .collect();
    all.extend(marked.iter().map(|range| (range.clone(), Mark::Highlight)));
    all.extend(found.iter().map(|range| (range.clone(), Mark::Found)));
    all.retain_mut(|(range, _)| {
        range.end = range.end.min(length);
        range.start < range.end
            && block.text.is_char_boundary(range.start)
            && block.text.is_char_boundary(range.end)
    });
    let mut edges: Vec<usize> = all
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|edge| {
            let marks: Vec<Mark> = all
                .iter()
                .filter(|(range, _)| range.start <= edge[0] && edge[1] <= range.end)
                .map(|(_, mark)| mark.clone())
                .collect();
            (!marks.is_empty()).then(|| (edge[0]..edge[1], marks))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mark {
    Style(Style),
    Highlight,
    Found,
}

/// The part of a block's text in `part`, styled, with its links clickable.
fn rich_text(
    id: (usize, usize),
    text: &str,
    part: Range<usize>,
    runs: &[(Range<usize>, Vec<Mark>)],
    cx: &mut Context<CalibreApp>,
) -> InteractiveText {
    let theme = cx.theme().clone();
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for (range, marks) in runs {
        let (from, to) = (range.start.max(part.start), range.end.min(part.end));
        if from >= to {
            continue;
        }
        let range = from - part.start..to - part.start;
        let mut style = HighlightStyle::default();
        for mark in marks {
            match mark {
                Mark::Style(Style::Bold) => style.font_weight = Some(FontWeight::BOLD),
                Mark::Style(Style::Italic) => style.font_style = Some(FontStyle::Italic),
                Mark::Style(Style::Underline) => {
                    style.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        ..Default::default()
                    })
                }
                Mark::Style(Style::Strike) => {
                    style.strikethrough = Some(StrikethroughStyle {
                        thickness: px(1.),
                        ..Default::default()
                    })
                }
                Mark::Style(Style::Code) => style.background_color = Some(theme.muted),
                Mark::Style(Style::Superscript | Style::Subscript) => style.fade_out = Some(0.25),
                Mark::Style(Style::Link(href)) => {
                    style.color = Some(theme.link);
                    style.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        ..Default::default()
                    });
                    links.push((range.clone(), href.clone()));
                }
                Mark::Highlight => style.background_color = Some(theme.warning.opacity(0.35)),
                Mark::Found => style.background_color = Some(theme.primary.opacity(0.35)),
            }
        }
        highlights.push((range, style));
    }
    let app = cx.entity().downgrade();
    InteractiveText::new(
        ElementId::NamedInteger("text".into(), (id.0 * 64 + id.1) as u64),
        StyledText::new(text[part].to_string()).with_highlights(highlights),
    )
    .on_click(
        links.iter().map(|(range, _)| range.clone()).collect(),
        move |link, _, cx| {
            if let Some((_, href)) = links.get(link) {
                let href = href.clone();
                let _ = app.update(cx, |this, cx| this.follow_link(&href, cx));
            }
        },
    )
}

fn block_element(
    app: &CalibreApp,
    block: &Block,
    index: usize,
    marked: &[Range<usize>],
    found: &[Range<usize>],
    font_size: f32,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    let theme = cx.theme().clone();
    match &block.kind {
        BlockKind::Rule => {
            return div()
                .w_full()
                .h(px(1.))
                .my_2()
                .bg(theme.border)
                .into_any_element()
        }
        BlockKind::Image(image) => {
            let picture = app.reader_images.get(*image).cloned().flatten();
            return v_flex()
                .w_full()
                .items_center()
                .gap_1()
                .when_some(picture, |this, picture| {
                    this.child(
                        img(picture)
                            .max_w_full()
                            .max_h(px(font_size * 36.))
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when(!block.text.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(block.text.clone()),
                    )
                })
                .into_any_element();
        }
        _ => {}
    }

    let runs = text_runs(block, marked, found);
    if block.kind == BlockKind::TableRow {
        let mut start = 0;
        let cells = block.text.split('\t').enumerate().map(|(cell, text)| {
            let range = start..start + text.len();
            start = range.end + 1;
            div()
                .flex_1()
                .min_w_0()
                .child(rich_text((index, cell), &block.text, range, &runs, cx))
        });
        return h_flex()
            .w_full()
            .gap_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .children(cells.collect::<Vec<_>>())
            .into_any_element();
    }
    let text = rich_text((index, 0), &block.text, 0..block.text.len(), &runs, cx);

    match &block.kind {
        BlockKind::Heading(level) => {
            let scale = match level {
                1 => 1.6,
                2 => 1.35,
                3 => 1.15,
                _ => 1.0,
            };
            div()
                .w_full()
                .pt(px(font_size * 0.4))
                .text_size(px(font_size * scale))
                .line_height(relative(1.25))
                .font_weight(FontWeight::BOLD)
                .child(text)
                .into_any_element()
        }
        BlockKind::ListItem { marker } => h_flex()
            .w_full()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(marker.clone()),
            )
            .child(div().flex_1().min_w_0().child(text))
            .into_any_element(),
        BlockKind::Preformatted => div()
            .w_full()
            .p_3()
            .rounded(px(6.))
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_size(px(font_size * 0.9))
            .child(text)
            .into_any_element(),
        _ => div()
            .w_full()
            .when(block.quoted, |this| {
                this.italic().text_color(theme.muted_foreground)
            })
            .child(text)
            .into_any_element(),
    }
}

fn truncate_label(title: &str) -> String {
    const MAX: usize = 36;
    if title.chars().count() > MAX {
        format!("{}…", title.chars().take(MAX).collect::<String>())
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use cr_core::formats::reading::{Block, BlockKind, Style};

    use super::{text_runs, Mark};

    #[test]
    fn overlapping_spans_become_ordered_runs() {
        let block = Block {
            kind: BlockKind::Paragraph,
            text: "Plain bold both end".into(),
            spans: vec![
                cr_core::formats::reading::Span {
                    start: 6,
                    end: 15,
                    style: Style::Bold,
                },
                cr_core::formats::reading::Span {
                    start: 11,
                    end: 15,
                    style: Style::Italic,
                },
            ],
            indent: 0,
            quoted: false,
        };
        let runs = text_runs(&block, &[0..8], &[100..200]);
        let ranges: Vec<Range<usize>> = runs.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..6, 6..8, 8..11, 11..15]);
        assert_eq!(runs[0].1, [Mark::Highlight]);
        assert_eq!(runs[1].1, [Mark::Style(Style::Bold), Mark::Highlight]);
        assert_eq!(
            runs[3].1,
            [Mark::Style(Style::Bold), Mark::Style(Style::Italic)]
        );
    }
}
