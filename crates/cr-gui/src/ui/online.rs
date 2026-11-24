use std::sync::Arc;

use cr_core::ai::Role;
use cr_core::formats::html::html_to_text;
use cr_core::online_metadata::{Candidate, Field, Source};
use cr_core::Metadata;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CalibreApp;

pub fn preferences(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let busy = app.job.is_some();
    let model = app.online.model.read(cx).value().to_string();
    let filter = model.to_ascii_lowercase();
    let model_rows = app
        .online
        .models
        .iter()
        .enumerate()
        .filter(|(_, model)| {
            model.id.to_ascii_lowercase().contains(&filter)
                || model.name.to_ascii_lowercase().contains(&filter)
                || filter == app.settings.openrouter_model.to_ascii_lowercase()
        })
        .map(|(index, model)| {
            let id = model.id.clone();
            Button::new(("openrouter-model", index))
                .ghost()
                .small()
                .label(format!("{} · {}", model.name, id))
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.save_openrouter_model(id.clone(), window, cx);
                }))
        })
        .collect::<Vec<_>>();
    v_flex()
        .gap_3()
        .child(div().text_sm().text_color(muted).child(
            "Use your own OpenRouter account. A key typed here lasts until you close the app; set OPENROUTER_API_KEY to use it across launches.",
        ))
        .child(field("API key", Input::new(&app.online.key).w_full()
            .content_type(InputContentType::Password)))
        .child(field("Model ID or catalog filter", Input::new(&app.online.model).w_full()))
        .child(div().text_sm().text_color(muted).child(
            format!("Active model: {}", app.settings.openrouter_model),
        ))
        .child(h_flex().flex_wrap().gap_2()
            .child(Button::new("save-openrouter-model").outline().small().label("Use Model ID")
                .on_click(cx.listener(|app, _, window, cx| {
                    let value = app.online.model.read(cx).value().to_string();
                    app.save_openrouter_model(value, window, cx);
                })))
            .child(Button::new("test-openrouter").outline().small().label("Test Connection")
                .disabled(busy).on_click(cx.listener(|app, _, window, cx| app.test_openrouter(window, cx))))
            .child(Button::new("list-openrouter-models").outline().small().label("Browse Models")
                .disabled(busy).on_click(cx.listener(|app, _, window, cx| app.load_openrouter_models(window, cx)))))
        .when_some(app.online.connection.clone(), |this, status| {
            this.child(div().text_sm().text_color(muted).child(status))
        })
        .when(!model_rows.is_empty(), |tree| tree.child(
            div().id("openrouter-models").max_h(px(176.)).overflow_y_scroll()
                .child(v_flex().items_start().children(model_rows)),
        ))
        .into_any_element()
}

pub fn metadata_dialog(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let Some(session) = &app.online.metadata else {
        return div().into_any_element();
    };
    let busy = app.job.is_some();
    let muted = cx.theme().muted_foreground;
    let label = format!(
        "Book {} of {} · {}",
        session.reviewed, session.total, session.current.title,
    );
    let search_actions = h_flex().gap_2().flex_wrap().children(
        [
            ("search-open-library", Source::OpenLibrary),
            ("search-google-books", Source::GoogleBooks),
        ]
        .into_iter()
        .map(|(id, source)| {
            Button::new(id)
                .outline()
                .small()
                .disabled(busy)
                .label(format!("Search {source}"))
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.search_online_metadata(source, window, cx);
                }))
        }),
    );
    let results = session
        .results
        .iter()
        .enumerate()
        .map(|(index, row)| {
            h_flex()
                .gap_2()
                .py_1()
                .w_full()
                .child(cover(row.thumbnail.clone(), 32., 46., cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .child(row.candidate.title.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(row.candidate.authors.join(", ")),
                        ),
                )
                .child(
                    Button::new(("online-result", index))
                        .ghost()
                        .small()
                        .disabled(busy)
                        .label("Compare")
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.preview_online_candidate(index, window, cx)
                        })),
                )
        })
        .collect::<Vec<_>>();
    let comparison = session.preview.as_ref().map(|preview| {
        let rows = Field::ALL
            .into_iter()
            .map(|field| {
                let available = if field == Field::Cover {
                    preview.cover.is_some()
                } else {
                    candidate_text(&preview.candidate, field).is_some()
                };
                let selector = Checkbox::new(format!("online-field-{}", field.label()))
                    .label(field.label())
                    .checked(preview.fields.contains(&field))
                    .disabled(!available || busy || !app.can_write())
                    .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                        if let Some(preview) = app
                            .online
                            .metadata
                            .as_mut()
                            .and_then(|state| state.preview.as_mut())
                        {
                            set_checked(&mut preview.fields, field, *checked);
                            cx.notify();
                        }
                    }));
                if field == Field::Cover {
                    comparison_row(
                        selector,
                        cover(session.current_cover.clone(), 54., 76., cx),
                        cover(preview.cover.clone(), 54., 76., cx),
                        cx,
                    )
                } else {
                    comparison_row(
                        selector,
                        cell(current_text(&session.current, field), cx),
                        cell(candidate_text(&preview.candidate, field), cx),
                        cx,
                    )
                }
            })
            .collect::<Vec<_>>();
        v_flex()
            .gap_1()
            .w_full()
            .child(comparison_header(cx))
            .children(rows)
    });
    let apply_disabled = busy
        || !app.can_write()
        || session
            .preview
            .as_ref()
            .is_none_or(|preview| preview.fields.is_empty());
    let skip_label = if session.remaining.is_empty() {
        "Close"
    } else {
        "Skip Book"
    };
    v_flex()
        .w_full()
        .gap_3()
        .child(div().text_sm().text_color(muted).child(label))
        .child(
            h_flex()
                .gap_2()
                .child(query_field("Title", &app.online.title, busy))
                .child(query_field("Author", &app.online.author, busy))
                .child(query_field("ISBN (takes priority)", &app.online.isbn, busy)),
        )
        .child(search_actions)
        .child(
            div()
                .text_sm()
                .text_color(muted)
                .child(if session.status.is_empty() {
                    "Search by title and author, or enter an ISBN for an edition lookup."
                        .to_string()
                } else {
                    session.status.clone()
                }),
        )
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .when(!results.is_empty(), |this| {
                    this.child(
                        div()
                            .id("online-search-results")
                            .w(px(252.))
                            .flex_shrink_0()
                            .max_h(px(322.))
                            .overflow_y_scroll()
                            .pr_2()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(v_flex().children(results)),
                    )
                })
                .when_some(comparison, |this, comparison| {
                    this.child(
                        div()
                            .id("online-metadata-comparison")
                            .flex_1()
                            .min_w_0()
                            .max_h(px(322.))
                            .overflow_y_scroll()
                            .child(comparison),
                    )
                }),
        )
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .when(!session.remaining.is_empty(), |this| {
                    this.child(
                        Button::new("stop-metadata")
                            .ghost()
                            .small()
                            .label("Done")
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.online.metadata = None;
                                window.close_dialog(cx);
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new("advance-metadata")
                        .outline()
                        .small()
                        .label(skip_label)
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.advance_online_metadata(window, cx)
                        })),
                )
                .child(
                    Button::new("apply-online-metadata")
                        .primary()
                        .small()
                        .label("Apply Selected")
                        .disabled(apply_disabled)
                        .on_click(
                            cx.listener(|app, _, window, cx| app.apply_online_metadata(window, cx)),
                        ),
                ),
        )
        .into_any_element()
}

pub fn ai_dialog(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let Some(session) = &app.online.ai else {
        return div().into_any_element();
    };
    let busy = app.job.is_some();
    let muted = cx.theme().muted_foreground;
    let header = format!(
        "{} · {}",
        session.current.title, app.settings.openrouter_model
    );
    let messages = session
        .history
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let role = match message.role {
                Role::Assistant => "OpenRouter",
                _ => "You",
            };
            v_flex()
                .id(("chat-message", index))
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(cx.theme().muted)
                .child(div().text_xs().text_color(muted).child(role))
                .child(div().text_sm().child(message.content.clone()))
        })
        .collect::<Vec<_>>();
    let suggestion = session.suggestion.as_ref().map(|suggestion| {
        let rows = [
            (Field::Description, nonempty(suggestion.description.clone())),
            (Field::Tags, nonempty(suggestion.tags.join(", "))),
        ]
        .into_iter()
        .map(|(field, proposed)| {
            let selector = Checkbox::new(format!("ai-field-{}", field.label()))
                .label(field.label())
                .checked(session.fields.contains(&field))
                .disabled(proposed.is_none() || busy || !app.can_write())
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    if let Some(session) = app.online.ai.as_mut() {
                        set_checked(&mut session.fields, field, *checked);
                        cx.notify();
                    }
                }));
            comparison_row(
                selector,
                cell(current_text(&session.current, field), cx),
                cell(proposed, cx),
                cx,
            )
        })
        .collect::<Vec<_>>();
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Review AI suggestions"),
            )
            .child(comparison_header(cx))
            .children(rows)
            .child(
                Button::new("apply-ai-fields")
                    .primary()
                    .small()
                    .label("Apply Selected")
                    .disabled(busy || !app.can_write() || session.fields.is_empty())
                    .on_click(cx.listener(|app, _, window, cx| app.apply_ai_metadata(window, cx))),
            )
    });
    v_flex().w_full().gap_3()
        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(header))
        .child(div().text_sm().text_color(muted).child(
            "Your question, recent chat and book metadata go to OpenRouter and the chosen model provider. Responses can be wrong and may incur charges.",
        ))
        .child(Checkbox::new("ai-share-excerpt")
            .label("Also share the opening excerpt (up to 12,000 characters)")
            .checked(session.excerpt).disabled(busy)
            .on_click(cx.listener(|app, checked: &bool, _, cx| {
                if let Some(session) = app.online.ai.as_mut() {
                    if session.excerpt != *checked {
                        session.excerpt = *checked;
                        session.history.clear();
                    }
                    cx.notify();
                }
            })))
        .child(h_flex().gap_2().child(Input::new(&app.online.question).flex_1().disabled(busy))
            .child(Button::new("ai-send").primary().small().label("Send").disabled(busy)
                .on_click(cx.listener(|app, _, window, cx| app.run_ai(false, window, cx)))))
        .child(h_flex().gap_2().flex_wrap()
            .child(Button::new("ai-suggest").outline().small().label("Suggest Description & Tags")
                .disabled(busy).on_click(cx.listener(|app, _, window, cx| app.run_ai(true, window, cx))))
            .child(Button::new("ai-configure").ghost().small().label("OpenRouter Preferences")
                .disabled(busy).on_click(cx.listener(|app, _, window, cx| {
                    app.online.ai = None;
                    window.close_dialog(cx);
                    app.open_preferences(window, cx);
                })))
            .when(!session.history.is_empty(), |this| this.child(
                Button::new("clear-chat").ghost().small().label("Clear Chat").disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Some(session) = app.online.ai.as_mut() {
                            session.history.clear();
                            cx.notify();
                        }
                    })),
            )))
        .when(!session.status.is_empty(), |this| this.child(
            div().text_sm().text_color(muted).child(session.status.clone()),
        ))
        .when(suggestion.is_some() || !messages.is_empty(), |this| this.child(
            div().id("ai-response").max_h(px(310.)).overflow_y_scroll().child(
                v_flex().gap_2().when_some(suggestion, |this, suggestion| this.child(suggestion))
                    .children(messages),
            ),
        ))
        .into_any_element()
}

fn query_field(title: &'static str, state: &Entity<InputState>, busy: bool) -> Div {
    field(title, Input::new(state).disabled(busy).w_full())
        .flex_1()
        .min_w_0()
}

fn field(title: &'static str, content: impl IntoElement) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_sm().child(title))
        .child(content)
}

fn comparison_header(cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_2()
        .text_xs()
        .text_color(muted)
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().w(px(112.)).flex_shrink_0().child("APPLY FIELD"))
        .child(div().flex_1().child("CURRENT"))
        .child(div().flex_1().child("PROPOSED"))
}

fn comparison_row(selector: Checkbox, current: AnyElement, proposed: AnyElement, cx: &App) -> Div {
    h_flex()
        .items_start()
        .gap_2()
        .py_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(div().w(px(112.)).flex_shrink_0().child(selector))
        .child(div().flex_1().min_w_0().child(current))
        .child(div().flex_1().min_w_0().child(proposed))
}

fn cell(value: Option<String>, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .when(value.is_none(), |this| {
            this.text_color(cx.theme().muted_foreground)
        })
        .child(value.unwrap_or_else(|| "—".into()))
        .into_any_element()
}

fn cover(image: Option<Arc<Image>>, width: f32, height: f32, cx: &App) -> AnyElement {
    match image {
        Some(image) => img(image)
            .w(px(width))
            .h(px(height))
            .rounded_sm()
            .flex_shrink_0()
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => div()
            .w(px(width))
            .h(px(height))
            .rounded_sm()
            .flex_shrink_0()
            .bg(cx.theme().muted)
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child("—")
            .into_any_element(),
    }
}

fn set_checked(fields: &mut Vec<Field>, field: Field, checked: bool) {
    fields.retain(|value| *value != field);
    if checked {
        fields.push(field);
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn current_text(metadata: &Metadata, field: Field) -> Option<String> {
    match field {
        Field::Title => Some(metadata.title.clone()),
        Field::Authors => nonempty(metadata.authors.join(", ")),
        Field::Publisher => metadata.publisher.clone(),
        Field::Date => metadata
            .pubdate
            .map(|date| date.format("%Y-%m-%d").to_string()),
        Field::Description => metadata
            .comments
            .as_deref()
            .map(html_to_text)
            .and_then(nonempty),
        Field::Tags => nonempty(metadata.tags.join(", ")),
        Field::Isbn => metadata.identifiers.get("isbn").cloned(),
        Field::Cover => None,
    }
}

fn candidate_text(candidate: &Candidate, field: Field) -> Option<String> {
    match field {
        Field::Title => Some(candidate.title.clone()),
        Field::Authors => nonempty(candidate.authors.join(", ")),
        Field::Publisher => candidate.publisher.clone(),
        Field::Date => candidate.published.clone(),
        Field::Description => candidate.description.clone(),
        Field::Tags => nonempty(candidate.tags.join(", ")),
        Field::Isbn => candidate.isbn.clone(),
        Field::Cover => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn field_selection_is_opt_in_and_does_not_duplicate_fields() {
        let mut fields = Vec::new();
        set_checked(&mut fields, Field::Title, true);
        set_checked(&mut fields, Field::Title, true);
        set_checked(&mut fields, Field::Description, true);
        set_checked(&mut fields, Field::Title, false);
        assert_eq!(fields, [Field::Description]);
    }
}
