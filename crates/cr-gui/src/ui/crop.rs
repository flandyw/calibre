//! The cover crop screen.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::widgets::{field, section_label};
use crate::app::{fit_dims, CalibreApp, Tone};

pub fn crop_body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let primary = theme.primary;

    let (original_width, original_height) = match app.crop_source.as_ref() {
        Some(source) => fit_dims(source.width() as f32, source.height() as f32, 340., 500.),
        None => (340., 500.),
    };
    let (crop_left, crop_top, crop_width, crop_height) =
        app.crop_percentages(cx).unwrap_or((0.0, 0.0, 100.0, 100.0));
    let bounds_cell = Rc::new(Cell::new((0.0f32, 0.0f32, 0.0f32, 0.0f32)));

    let handle = |top: bool, left: bool| {
        div()
            .absolute()
            .when(top, |this| this.top(-px(5.)))
            .when(!top, |this| this.bottom(-px(5.)))
            .when(left, |this| this.left(-px(5.)))
            .when(!left, |this| this.right(-px(5.)))
            .size(px(10.))
            .rounded_full()
            .bg(primary)
            .border_1()
            .border_color(gpui_kit::white())
    };

    let original = match &app.cover_path {
        Some(path) => {
            let bounds_for_mouse = bounds_cell.clone();
            let bounds_for_canvas = bounds_cell.clone();
            div()
                .relative()
                .w(px(original_width))
                .h(px(original_height))
                .rounded(px(6.))
                .overflow_hidden()
                .shadow_lg()
                .cursor(CursorStyle::Crosshair)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.start_crop_drag(event.position, bounds_for_mouse.get(), cx);
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    if event.dragging() {
                        this.drag_crop(event.position, window, cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_crop_drag(cx)),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_crop_drag(cx)),
                )
                .child(img(path.clone()).absolute().size_full())
                .child(
                    canvas(
                        move |bounds, _, _| {
                            bounds_for_canvas.set((
                                bounds.origin.x.as_f32(),
                                bounds.origin.y.as_f32(),
                                bounds.size.width.as_f32(),
                                bounds.size.height.as_f32(),
                            ))
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                // Dim everything outside the crop frame.
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w_full()
                        .h(relative(crop_top / 100.0))
                        .bg(gpui_kit::black().opacity(0.45)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .w_full()
                        .h(relative((100.0 - crop_top - crop_height).max(0.0) / 100.0))
                        .bg(gpui_kit::black().opacity(0.45)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(relative(crop_top / 100.0))
                        .w(relative(crop_left / 100.0))
                        .h(relative(crop_height / 100.0))
                        .bg(gpui_kit::black().opacity(0.45)),
                )
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .top(relative(crop_top / 100.0))
                        .w(relative((100.0 - crop_left - crop_width).max(0.0) / 100.0))
                        .h(relative(crop_height / 100.0))
                        .bg(gpui_kit::black().opacity(0.45)),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(crop_left / 100.0))
                        .top(relative(crop_top / 100.0))
                        .w(relative(crop_width / 100.0))
                        .h(relative(crop_height / 100.0))
                        .border_2()
                        .border_color(primary)
                        .cursor_move()
                        .child(handle(true, true))
                        .child(handle(true, false))
                        .child(handle(false, true))
                        .child(handle(false, false)),
                )
                .into_any_element()
        }
        None => div()
            .w(px(original_width))
            .h(px(original_height))
            .rounded(px(6.))
            .bg(theme.muted)
            .into_any_element(),
    };

    let (preview_width, preview_height) = app.crop_view_dims(cx);
    let preview = match &app.crop_preview_path {
        Some(path) => img(path.clone())
            .w(px(preview_width))
            .h(px(preview_height))
            .rounded(px(6.))
            .shadow_md()
            .into_any_element(),
        None => div()
            .w(px(preview_width))
            .h(px(preview_height))
            .rounded(px(6.))
            .bg(theme.muted)
            .into_any_element(),
    };
    let source_dims = app
        .crop_source
        .as_ref()
        .map(|source| format!("Original: {} × {} px", source.width(), source.height()))
        .unwrap_or_default();

    let preset = |id: &'static str, label: &'static str, aspect: Option<f32>| {
        Button::new(id).outline().small().label(label).on_click(
            cx.listener(move |this, _, window, cx| this.apply_crop_preset(aspect, window, cx)),
        )
    };
    let number = |label: &str, state| {
        div()
            .w(px(96.))
            .child(field(label, Input::new(state).small(), cx))
    };

    div()
        .id("crop-scroll")
        .size_full()
        .overflow_y_scroll()
        .child(
            h_flex()
                .w_full()
                .min_h_full()
                .p_8()
                .gap_10()
                .items_center()
                .justify_center()
                .child(
                    v_flex().gap_2().child(original).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Drag the frame or its corners. ".to_string() + &source_dims),
                    ),
                )
                .child(
                    v_flex()
                        .w(px(320.))
                        .gap_5()
                        .child(
                            v_flex()
                                .gap_2()
                                .child(section_label("Preview", cx))
                                .child(preview),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(section_label("Aspect ratio", cx))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .flex_wrap()
                                        .child(preset("crop-2-3", "2:3 Ebook", Some(2.0 / 3.0)))
                                        .child(preset("crop-3-4", "3:4", Some(0.75)))
                                        .child(preset("crop-1-1", "Square", Some(1.0)))
                                        .child(preset("crop-full", "Original", None)),
                                ),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(section_label("Frame (% of original)", cx))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .flex_wrap()
                                        .child(number("Left", &app.crop_left))
                                        .child(number("Top", &app.crop_top))
                                        .child(number("Width", &app.crop_width))
                                        .child(number("Height", &app.crop_height)),
                                )
                                .child(
                                    Button::new("crop-refresh")
                                        .ghost()
                                        .small()
                                        .icon(IconName::RefreshCw)
                                        .label("Update preview")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Err(err) = this.update_crop_preview(cx) {
                                                this.set_status(err, Tone::Error);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        ),
                ),
        )
        .into_any_element()
}
