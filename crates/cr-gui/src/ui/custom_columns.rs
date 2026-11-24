//! Custom columns tool pane.

use cr_core::library::CUSTOM_DATATYPES;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::reveal::{revealed, RevealOnFocus as _};
use super::widgets::field;
use crate::app::{set_textarea, CalibreApp};

pub fn pane(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let state = &app.custom_columns;
    let locked = !app.can_write() || app.job.is_some();
    let muted = cx.theme().muted_foreground;
    let types = CUSTOM_DATATYPES
        .into_iter()
        .enumerate()
        .map(|(index, datatype)| {
            let label = match datatype {
                "text" => "Text",
                "comments" => "Long text",
                "series" => "Series",
                "enumeration" => "Fixed choices",
                "rating" => "Rating",
                "int" => "Whole number",
                "float" => "Decimal",
                "bool" => "Yes or no",
                "datetime" => "Date",
                _ => datatype,
            };
            choice(("custom-type", index), label, state.datatype == datatype)
                .disabled(locked)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.custom_columns.datatype = datatype;
                    cx.notify();
                }))
        });
    let columns =
        state
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let id = column.id;
                choice(
                    ("custom-column", index),
                    column.name.clone(),
                    state.selected == Some(id),
                )
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.select_custom_column(Some(id), window, cx)
                }))
            })
            .collect::<Vec<_>>();
    let mut body = v_flex()
        .gap_3()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Custom Columns"),
        )
        .child(if columns.is_empty() {
            div()
                .text_sm()
                .text_color(muted)
                .child("This library has no custom columns.")
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(columns)
                .reveal_on_focus()
                .into_any_element()
        });

    if let Some(column) = state.selected_column() {
        let value_locked = locked
            || app.selected != state.value_book
            || !CUSTOM_DATATYPES.contains(&column.datatype.as_str());
        let help = match column.datatype.as_str() {
            "text" if column.is_multiple => "Separate values with commas.",
            "series" => "Enter a series name followed by its index, such as Earthsea [2].",
            "rating" => "Enter 0 to 5 stars. Half stars are supported.",
            "bool" => "Enter yes or no.",
            "datetime" => "Enter a date such as 2026-11-24.",
            _ => "",
        };
        let values = column
            .choices
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.clone();
                Button::new(("custom-allowed-value", index))
                    .small()
                    .outline()
                    .label(value.clone())
                    .disabled(value_locked)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        set_textarea(&app.custom_columns.value, value.clone(), window, cx);
                    }))
            })
            .collect::<Vec<_>>();
        body = body
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(format!("#{} · {}", column.label, column.datatype)),
                    )
                    .child(
                        Button::new("delete-custom-column")
                            .small()
                            .outline()
                            .label("Delete Column…")
                            .disabled(locked)
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.prompt_delete_custom_column(window, cx)
                            })),
                    )
                    .reveal_on_focus(),
            )
            .when(app.selected.is_some(), |body| {
                body.child(field(
                    "Value for the selected book",
                    Textarea::new(&state.value).w_full(),
                    cx,
                ))
                .child(div().text_sm().child(app.selected_title()))
                .when(!help.is_empty(), |body| {
                    body.child(div().text_sm().text_color(muted).child(help))
                })
                .when(!values.is_empty(), |body| {
                    body.child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .children(values)
                            .reveal_on_focus(),
                    )
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("save-custom-value")
                                .small()
                                .primary()
                                .label("Save Value")
                                .disabled(value_locked)
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.save_custom_column_value(window, cx)
                                })),
                        )
                        .child(
                            Button::new("reload-custom-value")
                                .small()
                                .ghost()
                                .label("Reload Value")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.load_custom_columns(window, cx)
                                })),
                        )
                        .reveal_on_focus(),
                )
            });
    }

    body.child(
        v_flex()
            .gap_3()
            .mt_2()
            .pt_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Create Column"),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().child(field(
                        "Lookup name",
                        Input::new(&state.lookup).w_full(),
                        cx,
                    )))
                    .child(div().flex_1().child(field(
                        "Heading",
                        Input::new(&state.heading).w_full(),
                        cx,
                    ))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(types)
                    .reveal_on_focus(),
            )
            .when(state.datatype == "text", |section| {
                section.child(revealed(
                    Checkbox::new("custom-multiple")
                        .label("Allow multiple values")
                        .checked(state.multiple)
                        .on_click(cx.listener(|app, checked: &bool, _, cx| {
                            app.custom_columns.multiple = *checked;
                            cx.notify();
                        })),
                ))
            })
            .when(state.datatype == "enumeration", |section| {
                section.child(field(
                    "Allowed values, separated by commas",
                    Input::new(&state.choices).w_full(),
                    cx,
                ))
            })
            .child(revealed(
                Button::new("create-custom-column")
                    .primary()
                    .small()
                    .label("Create Column")
                    .disabled(locked)
                    .on_click(
                        cx.listener(|app, _, window, cx| app.create_custom_column(window, cx)),
                    ),
            )),
    )
    .into_any_element()
}

fn choice(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Button {
    let button = Button::new(id)
        .small()
        .label(label.into())
        .selected(selected);
    if selected {
        button.primary()
    } else {
        button.outline()
    }
}
