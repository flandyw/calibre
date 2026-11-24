//! Small building blocks shared by the views.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::*;
use gpui_kit::*;

use super::reveal::RevealOnFocus as _;

/// An icon-only toolbar button that dispatches `action` and shows its
/// keyboard shortcut in the tooltip.
pub fn tool_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    tooltip: &'static str,
    action: impl Action,
) -> Button {
    let tooltip_action = action.boxed_clone();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .tooltip_with_action(tooltip, tooltip_action.as_ref(), None)
        .accessibility_label(tooltip)
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

/// A cover image, or a tinted placeholder showing the title's initial.
pub fn cover_thumb(
    path: Option<PathBuf>,
    title: &str,
    width: f32,
    height: f32,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().clone();
    match path {
        Some(path) => img(path)
            .w(px(width))
            .h(px(height))
            .flex_shrink_0()
            .rounded(px((width / 16.).clamp(2., 6.)))
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => {
            let initial = title
                .chars()
                .find(|ch| ch.is_alphanumeric())
                .map(|ch| ch.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            let hue = title.bytes().fold(0u32, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(byte as u32)
            }) % 360;
            let tint = hsla(
                hue as f32 / 360.,
                0.32,
                if theme.is_dark() { 0.32 } else { 0.78 },
                1.,
            );
            div()
                .w(px(width))
                .h(px(height))
                .flex_shrink_0()
                .rounded(px((width / 16.).clamp(2., 6.)))
                .bg(tint)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.foreground.opacity(0.7))
                .text_size(px((width * 0.42).max(11.)))
                .font_weight(FontWeight::SEMIBOLD)
                .child(initial)
                .into_any_element()
        }
    }
}

/// Five stars for a calibre 0–10 rating.
pub fn stars(rating: u8, size: f32, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let filled = (rating as usize).div_ceil(2).min(5);
    h_flex()
        .gap(px(1.))
        .children((0..5).map(|index| {
            let icon = if index < filled {
                IconName::StarFill
            } else {
                IconName::Star
            };
            Icon::new(icon)
                .size(px(size))
                .text_color(if index < filled {
                    theme.warning
                } else {
                    theme.muted_foreground.opacity(0.5)
                })
        }))
        .into_any_element()
}

/// A small uppercase heading for a group of controls.
pub fn section_label(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into().to_uppercase())
}

/// A labelled form control.
pub fn field(label: &str, control: impl IntoElement, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            Label::new(label.to_string())
                .text_xs()
                .text_color(cx.theme().muted_foreground),
        )
        .child(control)
        .reveal_on_focus()
        .into_any_element()
}

/// A muted label beside a value, for the inspector's details table.
pub fn detail_row(label: &str, value: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .gap_3()
        .items_start()
        .child(
            div()
                .w(px(88.))
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().flex_1().min_w_0().text_sm().child(value))
        .reveal_on_focus()
        .into_any_element()
}

/// A rounded chip, optionally clickable.
pub fn chip(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme().clone();
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(px(6.))
        .bg(theme.secondary)
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.secondary_foreground)
        .child(text.into())
}

pub fn empty_state(icon: impl Into<Icon>, title: &str, subtitle: &str, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_8()
        .child(Icon::new(icon).size(px(40.)).text_color(muted.opacity(0.6)))
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_sm()
                .text_color(muted)
                .child(subtitle.to_string()),
        )
        .into_any_element()
}

/// What a book row renders while it is dragged.
pub struct DragPreview {
    pub title: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded(px(8.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_sm()
            .child(Icon::new(IconName::BookOpen).small())
            .child(self.title.clone())
    }
}

/// A dragged book. When it leaves the window it becomes the book's file, so
/// it can be dropped into Finder, Mail or another app.
#[derive(Clone)]
pub struct DraggedBook {
    pub id: i64,
    pub title: SharedString,
    pub formats: Vec<String>,
}
