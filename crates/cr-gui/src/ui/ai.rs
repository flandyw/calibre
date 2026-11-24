//! Building blocks shared by Ask AI, AI Library Studio and the AI preferences.

use std::path::PathBuf;
use std::sync::Arc;

use cr_core::ai::{AiClient, ResolvedProvider};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::cover_thumb;
use crate::app::CalibreApp;
use crate::online::StatusTone;

/// Where requests run, as a tinted pill. Hovering it shows the privacy note.
pub fn provider_badge(client: &AiClient, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let on_device = client.is_on_device();
    let color = if on_device { theme.success } else { theme.info };
    let detail = match client.resolved_provider() {
        ResolvedProvider::AppleIntelligence => client.provider_name().to_string(),
        _ => client.active_model().to_string(),
    };
    let note = client.privacy_note();
    h_flex()
        .id("ai-provider-badge")
        .flex_shrink_0()
        .max_w(px(300.))
        .gap_1p5()
        .px_2()
        .py(px(3.))
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.35))
        .bg(color.opacity(0.1))
        .text_xs()
        .child(
            Icon::new(if on_device {
                Lucide::Lock
            } else {
                Lucide::Cloud
            })
            .size_3()
            .text_color(color),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(if on_device { "On-device" } else { "Cloud" }),
        )
        .child(
            div()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(detail),
        )
        .tooltip(move |window, cx| Tooltip::new(note).build(window, cx))
        .into_any_element()
}

/// Model output as Markdown. Links are drawn as plain text and images never
/// load, so a reply cannot make the app contact a server.
pub fn markdown(id: impl Into<ElementId>, text: impl Into<SharedString>) -> TextView {
    TextView::markdown(id, text)
        .selectable(true)
        .link(|_, _| None)
        .image_source(|_| ImageSource::Custom(Arc::new(|_, _| None)))
        .text_sm()
}

/// The user's side of a conversation: a bubble on the right.
pub fn user_turn(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .w_full()
        .justify_end()
        .child(
            div()
                .max_w(px(540.))
                .px_3()
                .py_1p5()
                .rounded(px(12.))
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .text_color(theme.secondary_foreground)
                .child(text.into()),
        )
        .into_any_element()
}

/// The small round mark that opens every assistant turn.
pub fn assistant_mark(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded_full()
        .bg(theme.accent)
        .child(
            Icon::new(Lucide::Sparkles)
                .size_3()
                .text_color(theme.accent_foreground),
        )
}

/// The assistant's side: its mark, then `body` in the remaining width.
pub fn assistant_turn(id: impl Into<ElementId>, body: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .id(id)
        .w_full()
        .items_start()
        .gap_2()
        .child(assistant_mark(cx))
        .child(div().flex_1().min_w_0().pt(px(1.)).child(body))
        .into_any_element()
}

/// The assistant turn shown while a request is running and nothing has
/// arrived yet.
pub fn thinking(label: impl Into<SharedString>, cx: &App) -> AnyElement {
    assistant_turn(
        "ai-thinking",
        h_flex()
            .gap_2()
            .items_center()
            .child(Spinner::new().small().color(cx.theme().muted_foreground))
            .child(ShimmerText::new(label).id("ai-thinking-label").text_sm()),
        cx,
    )
}

/// A bordered block for a result that needs review or stands on its own.
pub fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.35))
}

/// A card heading: an icon, a title and anything pushed to the right.
pub fn card_title(icon: Lucide, title: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(Icon::new(icon).size_4().text_color(theme.muted_foreground))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
}

/// An outlined action with an icon, for the rows of one-click tasks.
pub fn action_chip(
    id: impl Into<ElementId>,
    icon: Lucide,
    label: impl Into<SharedString>,
) -> Button {
    Button::new(id).outline().small().icon(icon).label(label)
}

/// An icon-only ghost button with a tooltip.
pub fn icon_action(id: impl Into<ElementId>, icon: Lucide, tooltip: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .tooltip(tooltip)
        .accessibility_label(tooltip)
}

/// One line of feedback under a composer. Errors get a tinted box so they
/// are not mistaken for a hint.
pub fn status_line(tone: StatusTone, text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (icon, color) = match tone {
        StatusTone::Info => (Lucide::Info, theme.muted_foreground),
        StatusTone::Success => (Lucide::CircleCheck, theme.success),
        StatusTone::Error => (Lucide::TriangleAlert, theme.danger),
    };
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_start()
        .text_sm()
        .when(tone == StatusTone::Error, |row| {
            row.px_2()
                .py_1p5()
                .rounded(px(8.))
                .border_1()
                .border_color(color.opacity(0.4))
                .bg(color.opacity(0.1))
        })
        .child(
            Icon::new(icon)
                .size_4()
                .mt(px(2.))
                .flex_shrink_0()
                .text_color(color),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(if tone == StatusTone::Error {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(text.into()),
        )
        .into_any_element()
}

/// The centered placeholder of an empty conversation or result area.
pub fn empty_panel(
    icon: Lucide,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(40.))
                .rounded_full()
                .bg(theme.accent)
                .child(Icon::new(icon).size_5().text_color(theme.accent_foreground)),
        )
        .child(
            div()
                .max_w(px(520.))
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(460.))
                .text_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(subtitle.into()),
        )
}

/// How much of the catalog fit in the request, as a bar and a count.
pub fn catalog_meter(included: usize, total: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let share = if total == 0 {
        1.
    } else {
        (included as f32 / total as f32).clamp(0., 1.)
    };
    let color = if included >= total {
        theme.success
    } else {
        theme.warning
    };
    let label = if included >= total {
        format!("Whole catalog considered · {total} books")
    } else {
        format!("{included} of {total} books fit in the request")
    };
    h_flex()
        .gap_2()
        .items_center()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .w(px(72.))
                .h(px(4.))
                .rounded_full()
                .bg(theme.muted)
                .child(div().h_full().rounded_full().bg(color).w(relative(share))),
        )
        .child(label)
        .into_any_element()
}

/// The cover of a library book by id, or its placeholder.
pub fn book_cover(app: &CalibreApp, id: i64, title: &str, width: f32, cx: &App) -> AnyElement {
    let path: Option<PathBuf> = app
        .books
        .iter()
        .find(|book| book.id == id)
        .and_then(|book| app.cover_for(book));
    cover_thumb(path, title, width, width * 1.45, cx)
}
