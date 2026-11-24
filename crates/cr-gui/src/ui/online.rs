use std::sync::Arc;

use cr_core::ai::{apple, Provider, ResolvedProvider, Role};
use cr_core::formats::html::html_to_text;
use cr_core::online_metadata::{Candidate, Field, Source};
use cr_core::Metadata;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputContentType, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;

use super::ai;
use crate::app::CalibreApp;
use crate::online::{AiCopySource, BookTask, StatusTone};

pub fn metadata_preferences(app: &CalibreApp, cx: &App) -> AnyElement {
    v_flex()
        .gap_2()
        .child(field("Google Books API key", Input::new(&app.online.google_books_key).w_full()
            .content_type(InputContentType::Password)))
        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
            "Optional. A key typed here lasts until you close the app. Set GOOGLE_BOOKS_API_KEY to reuse it across launches.",
        ))
        .into_any_element()
}

pub fn preferences(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let busy = app.job.is_some();
    let client = app.ai_client(cx);
    let selected = app.settings.ai_provider;
    let resolved = client.resolved_provider();
    let budget = client.context_budget();
    let provider_buttons = Provider::ALL
        .into_iter()
        .enumerate()
        .map(|(index, provider)| {
            let button = Button::new(("ai-provider", index))
                .small()
                .icon(match provider {
                    Provider::Auto => Lucide::Sparkles,
                    Provider::AppleIntelligence => Lucide::Laptop,
                    Provider::OpenRouter => Lucide::Cloud,
                    Provider::LocalServer => Lucide::Server,
                })
                .label(provider.label())
                .disabled(busy)
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.set_ai_provider(provider, window, cx);
                }));
            if selected == provider {
                button.primary()
            } else {
                button.outline()
            }
        })
        .collect::<Vec<_>>();
    let provider_note = match selected {
        Provider::Auto => "Uses Apple Intelligence on this Mac when it is available, and OpenRouter otherwise.",
        Provider::AppleIntelligence => "Runs on this Mac. Needs macOS 26 or later on Apple silicon with Apple Intelligence turned on.",
        Provider::OpenRouter => "A cloud service with many models. Needs an API key, and usage is billed by OpenRouter.",
        Provider::LocalServer => "Any OpenAI-compatible server on your machine or network, such as Ollama (http://localhost:11434/v1) or LM Studio (http://localhost:1234/v1).",
    };
    let filter = app.online.model.read(cx).value().to_ascii_lowercase();
    let local_mode = selected == Provider::LocalServer;
    let current_model = if local_mode {
        app.settings.ai_local_model.clone()
    } else {
        app.settings.openrouter_model.clone()
    };
    let model_rows = app
        .online
        .models
        .iter()
        .enumerate()
        .filter(|(_, model)| {
            local_mode
                || model.id.to_ascii_lowercase().contains(&filter)
                || model.name.to_ascii_lowercase().contains(&filter)
                || filter == app.settings.openrouter_model.to_ascii_lowercase()
        })
        .map(|(index, model)| {
            let id = model.id.clone();
            let current = model.id == current_model;
            let context = model.context_length.map(|tokens| {
                if tokens >= 1000 {
                    format!("{}K context", tokens / 1000)
                } else {
                    format!("{tokens} context")
                }
            });
            let price = model
                .pricing
                .as_ref()
                .and_then(|pricing| pricing.rates())
                .map(|(input, output)| {
                    if input == 0. && output == 0. {
                        "Free".to_string()
                    } else {
                        format!(
                            "${:.2} in · ${:.2} out per 1M tokens",
                            input * 1_000_000.,
                            output * 1_000_000.
                        )
                    }
                });
            h_flex()
                .id(("ai-model-row", index))
                .w_full()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|style| style.bg(theme.muted))
                .when(current, |row| row.bg(theme.muted.opacity(0.6)))
                .on_click(cx.listener(move |app, _, window, cx| {
                    if local_mode {
                        app.save_local_model(id.clone(), window, cx);
                    } else {
                        app.save_openrouter_model(id.clone(), window, cx);
                    }
                }))
                .child(div().w(px(14.)).flex_shrink_0().when(current, |slot| {
                    slot.child(
                        Icon::new(Lucide::Check)
                            .size_3p5()
                            .text_color(theme.success),
                    )
                }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().child(model.name.clone()))
                        .when(model.name != model.id, |column| {
                            column.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(muted)
                                    .child(model.id.clone()),
                            )
                        }),
                )
                .when_some(price, |row, price| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child(price),
                    )
                })
                .when_some(context, |row, context| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .px_1p5()
                            .rounded(px(4.))
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.secondary_foreground)
                            .child(context),
                    )
                })
        })
        .collect::<Vec<_>>();
    let show_openrouter = matches!(selected, Provider::OpenRouter | Provider::Auto);
    let show_local = selected == Provider::LocalServer;
    let can_browse_models = resolved != ResolvedProvider::AppleIntelligence;
    let show_apple = cfg!(target_os = "macos")
        || matches!(selected, Provider::Auto | Provider::AppleIntelligence);
    let stat = |value: String, label: &'static str| {
        v_flex()
            .flex_1()
            .px_2()
            .py_1p5()
            .rounded(px(8.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(value),
            )
            .child(div().text_xs().text_color(muted).child(label))
    };
    v_flex()
        .gap_3()
        .child(
            ai::card(cx)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Requests go to {}", client.provider_name())),
                        )
                        .child(ai::provider_badge(&client, cx)),
                )
                .child(div().text_xs().text_color(muted).child(client.privacy_note()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(stat(
                            format!("{} characters", thousands(budget.excerpt_chars)),
                            "book text per request, at most",
                        ))
                        .child(stat(
                            format!("{} books", thousands(budget.catalog_books)),
                            "catalog entries per request, at most",
                        ))
                        .child(stat(
                            format!("{} KB", budget.catalog_bytes / 1000),
                            "catalog metadata per request, at most",
                        )),
                )
                .when(show_apple, |card| {
                    card.child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .text_xs()
                            .text_color(muted)
                            .child(Icon::new(Lucide::Laptop).size_3())
                            .child(format!(
                                "Apple Intelligence: {}",
                                apple::availability().summary()
                            )),
                    )
                }),
        )
        .child(
            v_flex()
                .gap_1p5()
                .child(h_flex().flex_wrap().gap_1().children(provider_buttons))
                .child(div().text_xs().text_color(muted).child(provider_note)),
        )
        .when(show_openrouter, |this| {
            this.child(field(
                "OpenRouter API key",
                Input::new(&app.online.key)
                    .w_full()
                    .content_type(InputContentType::Password),
            ))
            .child(div().text_xs().text_color(muted).child(
                "A key typed here lasts until you close the app. Set OPENROUTER_API_KEY to reuse it across launches.",
            ))
            .child(field(
                "OpenRouter model ID or catalog filter",
                Input::new(&app.online.model).w_full(),
            ))
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .text_color(muted)
                    .child("Saved model:")
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(app.settings.openrouter_model.clone()),
                    ),
            )
        })
        .when(show_local, |this| {
            this.child(field("Server base URL", Input::new(&app.online.local_url).w_full()))
                .child(field("Model ID", Input::new(&app.online.local_model).w_full()))
                .child(field(
                    "Optional API key",
                    Input::new(&app.online.local_key)
                        .w_full()
                        .content_type(InputContentType::Password),
                ))
        })
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .when(show_openrouter, |row| {
                    row.child(
                        Button::new("save-openrouter-model")
                            .outline()
                            .small()
                            .label("Use Model ID")
                            .on_click(cx.listener(|app, _, window, cx| {
                                let value = app.online.model.read(cx).value().to_string();
                                app.save_openrouter_model(value, window, cx);
                            })),
                    )
                })
                .when(show_local, |row| {
                    row.child(
                        Button::new("save-local-ai")
                            .outline()
                            .small()
                            .label("Save Local Server")
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.save_local_ai(window, cx);
                            })),
                    )
                })
                .child(
                    Button::new("test-ai-provider")
                        .outline()
                        .small()
                        .icon(Lucide::Network)
                        .label("Test Connection")
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.test_ai_connection(window, cx);
                        })),
                )
                .when(can_browse_models, |row| {
                    row.child(
                        Button::new("list-ai-models")
                            .outline()
                            .small()
                            .icon(Lucide::List)
                            .label("Browse Models")
                            .disabled(busy)
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.load_ai_models(window, cx);
                            })),
                    )
                }),
        )
        .when_some(app.online.connection.clone(), |this, status| {
            this.child(match app.online.connection_ok {
                None if busy => h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(Spinner::new().small().color(muted))
                    .child(status)
                    .into_any_element(),
                None => ai::status_line(StatusTone::Info, status, cx),
                Some(true) => ai::status_line(StatusTone::Success, status, cx),
                Some(false) => ai::status_line(StatusTone::Error, status, cx),
            })
        })
        .when(!model_rows.is_empty(), |tree| {
            tree.child(
                div()
                    .id("ai-models")
                    .max_h(px(220.))
                    .p_1()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_y_scroll()
                    .child(v_flex().children(model_rows)),
            )
        })
        .child(preset_editor(app, cx))
        .into_any_element()
}

fn preset_editor(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> Div {
    let rows = app
        .settings
        .ai_presets
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            Button::new(("ai-edit-preset", index))
                .outline()
                .small()
                .label(preset.name.clone())
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.edit_ai_preset(Some(index), window, cx)
                }))
        })
        .collect::<Vec<_>>();
    v_flex().gap_2().border_t_1().pt_3()
        .child(div().font_weight(FontWeight::SEMIBOLD).child("Book action presets"))
        .child(div().text_sm().text_color(cx.theme().muted_foreground)
            .child("Create reusable prompts for the book context menu. Variables: {title}, {authors}, {description}, {series}, {tags}, {publisher}, {language}."))
        .child(h_flex().gap_2().flex_wrap().children(rows))
        .child(field("Action name", Input::new(&app.online.preset_name).w_full()))
        .child(field("Prompt template", Input::new(&app.online.preset_prompt).w_full()))
        .child(h_flex().gap_2()
            .child(Button::new("ai-save-preset").primary().small()
                .label(if app.online.preset_edit.is_some() { "Save Changes" } else { "Add Preset" })
                .on_click(cx.listener(|app, _, window, cx| app.save_ai_preset(window, cx))))
            .when(app.online.preset_edit.is_some(), |row| row
                .child(Button::new("ai-cancel-preset").ghost().small().label("Cancel Edit")
                    .on_click(cx.listener(|app, _, window, cx| app.edit_ai_preset(None, window, cx))))
                .child(Button::new("ai-delete-preset").ghost().small().label("Delete Preset")
                    .on_click(cx.listener(|app, _, window, cx| app.delete_ai_preset(window, cx))))))
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

const STARTER_QUESTIONS: [&str; 3] = [
    "What is this book about?",
    "Who would enjoy it?",
    "What are its main themes?",
];

pub fn ai_dialog(
    app: &CalibreApp,
    viewport_height: Pixels,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    let Some(session) = &app.online.ai else {
        return div().into_any_element();
    };
    let busy = app.job.is_some();
    let stopping = app.job.as_ref().is_some_and(|job| job.cancelling);
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let client = app.ai_client(cx);
    let has_reader_context =
        session.reader_chapter_text.is_some() || session.reader_passage.is_some();
    // The conversation takes the height the rest of the dialog leaves free.
    let reserved = if has_reader_context { 480. } else { 440. };
    let conversation_height = (viewport_height - px(reserved)).max(px(170.)).min(px(420.));

    let mut subtitle = if session.current.authors.is_empty() {
        "Unknown author".to_string()
    } else {
        session.current.authors.join(", ")
    };
    if let Some(format) = &session.reader_format {
        subtitle.push_str(&format!(" · reading {format}"));
    }
    let header = h_flex()
        .gap_3()
        .items_center()
        .child(ai::book_cover(
            app,
            session.id,
            &session.current.title,
            30.,
            cx,
        ))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(session.current.title.clone()),
                )
                .child(div().truncate().text_xs().text_color(muted).child(subtitle)),
        )
        .child(ai::provider_badge(&client, cx))
        .child(
            ai::icon_action("ai-configure", Lucide::Settings, "AI Preferences")
                .disabled(busy)
                .on_click(cx.listener(|app, _, window, cx| {
                    app.online.ai = None;
                    window.close_dialog(cx);
                    app.open_preferences(window, cx);
                })),
        );

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
        let chosen = session.fields.len();
        ai::assistant_turn(
            "ai-suggestion",
            ai::card(cx)
                .child(ai::card_title(
                    Lucide::WandSparkles,
                    "Suggested description and tags",
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Nothing changes until you tick a field and apply it."),
                )
                .child(comparison_header(cx))
                .children(rows)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("apply-ai-fields")
                                .primary()
                                .small()
                                .icon(Lucide::Check)
                                .label(match chosen {
                                    0 => "Apply Selected".to_string(),
                                    1 => "Apply 1 Field".to_string(),
                                    count => format!("Apply {count} Fields"),
                                })
                                .disabled(busy || !app.can_write() || chosen == 0)
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.apply_ai_metadata(window, cx)
                                })),
                        )
                        .child(
                            Button::new("dismiss-ai-suggestion")
                                .ghost()
                                .small()
                                .label("Dismiss")
                                .disabled(busy)
                                .on_click(cx.listener(|app, _, _, cx| {
                                    if let Some(session) = app.online.ai.as_mut() {
                                        session.suggestion = None;
                                        session.fields.clear();
                                        cx.notify();
                                    }
                                })),
                        ),
                ),
            cx,
        )
    });
    let translation = session
        .translation
        .as_ref()
        .map(|(language, translated, truncated)| {
            ai::assistant_turn(
                "ai-translation",
                ai::card(cx)
                    .child(
                        ai::card_title(
                            Lucide::Languages,
                            format!("Description in {language}"),
                            cx,
                        )
                        .child(copy_button("copy-ai-translation", translated.clone(), cx)),
                    )
                    .when(*truncated, |card| {
                        card.child(ai::status_line(
                            StatusTone::Info,
                            "Only the opening 4,000 characters were sent, so this cannot replace the full description.",
                            cx,
                        ))
                    })
                    .child(div().text_sm().child(translated.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("apply-ai-translation")
                                    .primary()
                                    .small()
                                    .icon(Lucide::Check)
                                    .label("Replace Description")
                                    .disabled(busy || *truncated || !app.can_write())
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        app.apply_ai_translation(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("dismiss-ai-translation")
                                    .ghost()
                                    .small()
                                    .label("Dismiss")
                                    .disabled(busy)
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(session) = app.online.ai.as_mut() {
                                            session.translation = None;
                                            session.sync_copyable_after_removal(
                                                AiCopySource::Translation,
                                            );
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
                cx,
            )
        });
    let local = (!session.local_result.is_empty()).then(|| {
        let icon = if session.local_title == "Reading time" {
            Lucide::Timer
        } else {
            Lucide::FileText
        };
        ai::assistant_turn(
            "ai-local-result",
            ai::card(cx)
                .child(
                    ai::card_title(icon, session.local_title.clone(), cx).child(copy_button(
                        "copy-ai-local",
                        session.local_copy_text(),
                        cx,
                    )),
                )
                .child(ai::markdown("ai-local-text", session.local_result.clone())),
            cx,
        )
    });

    // Cards sit where they happened in the conversation, not above it.
    let mut cards = [suggestion, translation, local];
    let mut turns: Vec<AnyElement> = Vec::new();
    for index in 0..=session.history.len() {
        for (slot, card) in cards.iter_mut().enumerate() {
            if session.card_at[slot].min(session.history.len()) == index {
                turns.extend(card.take());
            }
        }
        let Some(message) = session.history.get(index) else {
            continue;
        };
        turns.push(match message.role {
            Role::Assistant => ai::assistant_turn(
                ("chat-message", index),
                v_flex()
                    .gap_1()
                    .child(ai::markdown(("chat-text", index), message.content.clone()))
                    .child(copy_button(
                        ("copy-chat", index),
                        message.content.clone(),
                        cx,
                    )),
                cx,
            ),
            _ => ai::user_turn(("chat-message", index), message.content.clone(), cx),
        });
    }
    if let Some(pending) = &session.pending {
        turns.push(ai::user_turn("ai-pending", pending.clone(), cx));
        turns.push(if session.streaming.is_empty() {
            let label = if stopping {
                "Stopping…".to_string()
            } else if session.status.is_empty() {
                "Working…".to_string()
            } else {
                session.status.clone()
            };
            ai::thinking(label, cx)
        } else {
            ai::assistant_turn(
                "ai-streaming",
                ai::markdown("ai-streaming-text", session.streaming.clone()).stream_fade(true),
                cx,
            )
        });
    }
    let conversation = div()
        .id("ai-conversation")
        .w_full()
        .h(conversation_height)
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .overflow_y_scroll()
        .track_scroll(&session.scroll)
        .map(|area| {
            if turns.is_empty() {
                let starters =
                    STARTER_QUESTIONS
                        .into_iter()
                        .enumerate()
                        .map(|(index, question)| {
                            Button::new(("ai-starter", index))
                                .outline()
                                .small()
                                .label(question)
                                .disabled(busy)
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    app.ask_ai_question(question, window, cx);
                                }))
                        });
                area.child(
                    ai::empty_panel(
                        Lucide::Sparkles,
                        format!("Ask about {}", session.current.title),
                        client.privacy_note(),
                        cx,
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap_2()
                            .pt_1()
                            .children(starters),
                    ),
                )
            } else {
                area.child(v_flex().gap_3().p_3().children(turns))
            }
        });

    let excerpt_limit = client.context_budget().excerpt_chars;
    let excerpt_switch = Switch::new("ai-share-excerpt")
        .small()
        .label("Include opening excerpt")
        .tooltip(format!(
            "Sends up to {} characters from the start of the book with its metadata{}. Changing this starts a new chat.",
            thousands(excerpt_limit),
            if has_reader_context {
                ", for actions about the whole book"
            } else {
                ""
            }
        ))
        .checked(session.excerpt)
        .disabled(busy)
        .on_click(cx.listener(|app, checked: &bool, _, cx| {
            if let Some(session) = app.online.ai.as_mut() {
                if session.excerpt != *checked {
                    session.excerpt = *checked;
                    if session.reader_chapter_text.is_none() {
                        session.clear_chat();
                    }
                }
                cx.notify();
            }
        }));

    let task_chip = |id: &'static str,
                     icon: Lucide,
                     label: &'static str,
                     tooltip: &'static str,
                     task: BookTask,
                     cx: &mut Context<CalibreApp>| {
        ai::action_chip(id, icon, label)
            .tooltip(tooltip)
            .disabled(busy)
            .on_click(cx.listener(move |app, _, window, cx| app.run_book_task(task, window, cx)))
    };
    // What the reader attached, each next to the action that uses it.
    let reader_row = has_reader_context.then(|| {
        h_flex()
            .gap_2()
            .items_center()
            .when(session.reader_chapter_text.is_some(), |row| {
                let chapter = session
                    .reader_chapter_title
                    .clone()
                    .unwrap_or_else(|| "Current section".into());
                row.child(context_chip(
                    "ai-context-chapter",
                    Lucide::BookOpen,
                    chapter,
                    "The chapter open in the reader is attached to your questions.",
                    cx,
                ))
                .child(task_chip(
                    "ai-summarize-chapter",
                    Lucide::ScanText,
                    "Summarize Chapter",
                    "Summarize the chapter open in the reader",
                    BookTask::SummarizeChapter,
                    cx,
                ))
            })
            .when_some(session.reader_passage.as_deref(), |row, passage| {
                let preview: String = passage.chars().take(48).collect();
                let more = if passage.chars().count() > 48 {
                    "…"
                } else {
                    ""
                };
                row.child(context_chip(
                    "ai-context-passage",
                    Lucide::TextQuote,
                    format!("“{preview}{more}”"),
                    "The paragraph you clicked in the reader is attached.",
                    cx,
                ))
                .child(task_chip(
                    "ai-explain-passage",
                    Lucide::TextQuote,
                    "Explain Passage",
                    "Explain the paragraph you clicked in the reader",
                    BookTask::ExplainPassage,
                    cx,
                ))
            })
    });
    let actions = h_flex()
        .gap_2()
        .flex_wrap()
        .child(task_chip(
            "ai-suggest",
            Lucide::WandSparkles,
            "Describe & Tag",
            "Suggest a description and tags, then choose what to apply",
            BookTask::Suggest,
            cx,
        ))
        .child(task_chip(
            "ai-characters",
            Lucide::Users,
            "Characters",
            "A guide to the main characters",
            BookTask::Characters,
            cx,
        ))
        .child(task_chip(
            "ai-discuss",
            Lucide::MessagesSquare,
            "Discussion",
            "Questions for a book club or class",
            BookTask::Discussion,
            cx,
        ))
        .child(task_chip(
            "ai-time",
            Lucide::Timer,
            "Reading Time",
            "Counted on this computer. Nothing is sent.",
            BookTask::ReadingTime,
            cx,
        ));
    let recap_label = match session.reader_chapter_num {
        Some(num) => format!("Recap to Here (#{num})"),
        None => "Recap Through Chapter".into(),
    };
    let inputs =
        h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(
                Input::new(&app.online.chapter)
                    .small()
                    .w(px(100.))
                    .disabled(busy),
            )
            .child(
                ai::action_chip("ai-recap", Lucide::ListChecks, recap_label)
                    .tooltip("A spoiler-free recap that stops at this entry in Contents")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.run_book_task(BookTask::Recap, window, cx)
                    })),
            )
            .child(div().w(px(1.)).h(px(18.)).mx_1().bg(theme.border))
            .child(
                Input::new(&app.online.language)
                    .small()
                    .w(px(184.))
                    .disabled(busy),
            )
            .child(
                ai::action_chip("ai-translate", Lucide::Languages, "Translate Description")
                    .tooltip("Translate the description, then review it before replacing")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.run_book_task(BookTask::Translate, window, cx)
                    })),
            );
    let preset_chips = app
        .settings
        .ai_presets
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            ai::action_chip(
                ("ai-dialog-preset", index),
                Lucide::Bookmark,
                preset.name.clone(),
            )
            .ghost()
            .disabled(busy)
            .on_click(cx.listener(move |app, _, window, cx| {
                app.run_ai_preset_in_dialog(index, window, cx);
            }))
        })
        .collect::<Vec<_>>();

    let composer = h_flex()
        .gap_2()
        .items_center()
        .child(Input::new(&app.online.question).flex_1().disabled(busy))
        .child(if busy {
            Button::new("ai-stop")
                .outline()
                .icon(Lucide::Square)
                .label(if stopping { "Stopping…" } else { "Stop" })
                .disabled(stopping)
                .on_click(cx.listener(|app, _, _, cx| app.cancel_job(cx)))
        } else {
            Button::new("ai-send")
                .primary()
                .icon(Lucide::ArrowUp)
                .label("Send")
                .on_click(cx.listener(|app, _, window, cx| app.run_ai(false, window, cx)))
        });

    let context_summary = if session.reader_chapter_text.is_some() {
        "Sends metadata and the current chapter"
    } else if session.excerpt {
        "Sends metadata and the opening excerpt"
    } else {
        "Sends metadata only"
    };
    // Hints and confirmations take the summary's place, so only an error
    // moves the layout.
    let has_status = !busy && !session.status.is_empty();
    let quiet_status = has_status && session.tone != StatusTone::Error;
    let footer = h_flex()
        .gap_3()
        .items_center()
        .min_h(px(24.))
        .child(div().flex_shrink_0().child(excerpt_switch))
        .child(if quiet_status {
            div().flex_1().min_w_0().child(ai::status_line(
                session.tone,
                session.status.clone(),
                cx,
            ))
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(muted)
                .child(if session.usage.is_empty() {
                    context_summary.to_string()
                } else {
                    format!("{context_summary} · last reply {}", session.usage)
                })
        })
        .when(session.last_copyable.is_some(), |row| {
            row.child(
                ai::icon_action("copy-ai-result", Lucide::Copy, "Copy Last Response")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.copy_ai_result(window, cx);
                    })),
            )
        })
        .when(!session.history.is_empty(), |row| {
            row.child(
                ai::icon_action("clear-chat", Lucide::Eraser, "Clear Chat")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Some(session) = app.online.ai.as_mut() {
                            session.clear_chat();
                            cx.notify();
                        }
                    })),
            )
        });

    v_flex()
        .w_full()
        .gap_3()
        .child(header)
        .child(conversation)
        .children(reader_row)
        .child(actions)
        .child(inputs)
        .when(!preset_chips.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("Your presets"))
                    .children(preset_chips),
            )
        })
        .when(has_status && !quiet_status, |this| {
            this.child(ai::status_line(session.tone, session.status.clone(), cx))
        })
        .child(composer)
        .child(footer)
        .into_any_element()
}

/// A small copy button for one piece of AI output.
fn copy_button(id: impl Into<ElementId>, text: String, cx: &mut Context<CalibreApp>) -> Button {
    ai::icon_action(id, Lucide::Copy, "Copy")
        .xsmall()
        .on_click(cx.listener(move |app, _, window, cx| {
            app.copy_ai_text(text.clone(), window, cx);
        }))
}

/// What the reader attached to the conversation, as a pill.
fn context_chip(
    id: &'static str,
    icon: Lucide,
    label: String,
    tooltip: &'static str,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .min_w_0()
        .max_w(px(180.))
        .gap_1p5()
        .px_2()
        .py(px(2.))
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.secondary_foreground)
        .child(Icon::new(icon).size_3().text_color(theme.muted_foreground))
        .child(div().truncate().child(label))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
        })
        .into_any_element()
}

/// 12000 as "12,000".
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
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

    #[core::prelude::v1::test]
    fn thousands_groups_digits_in_threes() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(5_500), "5,500");
        assert_eq!(thousands(12_000), "12,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
