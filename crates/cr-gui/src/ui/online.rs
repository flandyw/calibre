use std::sync::Arc;

use cr_core::ai::{apple, Provider, ResolvedProvider, Role};
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
use crate::online::BookTask;

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
    let muted = cx.theme().muted_foreground;
    let busy = app.job.is_some();
    let client = app.ai_client(cx);
    let selected = app.settings.ai_provider;
    let resolved = client.resolved_provider();
    let apple_status = apple::availability();
    let active_pill = if client.is_on_device() {
        format!("On-device · {}", client.active_label())
    } else {
        format!("Cloud · {}", client.active_label())
    };
    let provider_buttons = Provider::ALL
        .into_iter()
        .enumerate()
        .map(|(index, provider)| {
            let button = Button::new(("ai-provider", index))
                .small()
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
    let model = app.online.model.read(cx).value().to_string();
    let filter = model.to_ascii_lowercase();
    let local_mode = selected == Provider::LocalServer;
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
            Button::new(("ai-model-row", index))
                .ghost()
                .small()
                .label(format!("{} · {}", model.name, id))
                .on_click(cx.listener(move |app, _, window, cx| {
                    if local_mode {
                        app.save_local_model(id.clone(), window, cx);
                    } else {
                        app.save_openrouter_model(id.clone(), window, cx);
                    }
                }))
        })
        .collect::<Vec<_>>();
    let show_openrouter = matches!(selected, Provider::OpenRouter | Provider::Auto);
    let show_local = selected == Provider::LocalServer;
    let can_browse_models = resolved != ResolvedProvider::AppleIntelligence;
    v_flex()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(cx.theme().muted)
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(active_pill))
                .child(div().text_xs().text_color(muted).child(client.privacy_note()))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Apple Intelligence: {}", apple_status.summary())),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(div().text_sm().child("Provider"))
                .child(h_flex().flex_wrap().gap_2().children(provider_buttons)),
        )
        .when(selected == Provider::Auto, |this| {
            this.child(div().text_xs().text_color(muted).child(
                "Auto uses Apple Intelligence on-device on macOS 26+ when available, and falls back to OpenRouter otherwise.",
            ))
        })
        .when(show_openrouter, |this| {
            this.child(div().text_sm().text_color(muted).child(
                "OpenRouter API key lasts until you close the app; set OPENROUTER_API_KEY to reuse it across launches.",
            ))
            .child(field(
                "OpenRouter API key",
                Input::new(&app.online.key)
                    .w_full()
                    .content_type(InputContentType::Password),
            ))
            .child(field(
                "OpenRouter model ID or catalog filter",
                Input::new(&app.online.model).w_full(),
            ))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!("Saved OpenRouter model: {}", app.settings.openrouter_model)),
            )
        })
        .when(show_local, |this| {
            this.child(div().text_sm().text_color(muted).child(
                "Connect to any OpenAI-compatible local server (Ollama at http://localhost:11434/v1, LM Studio at http://localhost:1234/v1). Book data stays on your machine.",
            ))
            .child(field("Server base URL", Input::new(&app.online.local_url).w_full()))
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
                        .label("Test Provider")
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
                            .label("Browse Models")
                            .disabled(busy)
                            .on_click(cx.listener(|app, _, window, cx| {
                                app.load_ai_models(window, cx);
                            })),
                    )
                }),
        )
        .when_some(app.online.connection.clone(), |this, status| {
            this.child(div().text_sm().text_color(muted).child(status))
        })
        .when(!model_rows.is_empty(), |tree| {
            tree.child(
                div()
                    .id("ai-models")
                    .max_h(px(176.))
                    .overflow_y_scroll()
                    .child(v_flex().items_start().children(model_rows)),
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

pub fn ai_dialog(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let Some(session) = &app.online.ai else {
        return div().into_any_element();
    };
    let busy = app.job.is_some();
    let muted = cx.theme().muted_foreground;
    let client = app.ai_client(cx);
    let provider_name = client.provider_name();
    let privacy_badge = if client.is_on_device() {
        format!("On-device · {}", client.active_label())
    } else {
        format!("Cloud · {}", client.active_label())
    };
    let header = match &session.reader_format {
        Some(format) => format!("{} · {format}", session.current.title),
        None => session.current.title.clone(),
    };
    let has_reader_context =
        session.reader_chapter_text.is_some() || session.reader_passage.is_some();
    let has_copyable = session.last_copyable.is_some();
    let messages = session
        .history
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let role = match message.role {
                Role::Assistant => provider_name,
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
    let translation = session
        .translation
        .as_ref()
        .map(|(language, translated, truncated)| {
            v_flex()
                .gap_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().muted)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Translated description ({language})")),
                )
                .when(*truncated, |box_el| {
                    box_el.child(div().text_xs().text_color(muted).child(
                        "Only the opening 4,000 characters were sent for translation. Apply is disabled to avoid overwriting the rest of the description.",
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
                                .label("Apply to Description")
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
                                        cx.notify();
                                    }
                                })),
                        ),
                )
        });
    let preset_chips = app
        .settings
        .ai_presets
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            Button::new(("ai-dialog-preset", index))
                .ghost()
                .small()
                .label(preset.name.clone())
                .disabled(busy)
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.run_ai_preset_in_dialog(index, window, cx);
                }))
        })
        .collect::<Vec<_>>();
    let excerpt_limit = client.context_budget().excerpt_chars;
    let recap_label = if let Some(num) = session.reader_chapter_num {
        format!("Recap to Here (#{num})")
    } else {
        "Recap Through Chapter".into()
    };
    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(header))
                .child(
                    div()
                        .text_xs()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(cx.theme().muted)
                        .text_color(muted)
                        .child(privacy_badge),
                ),
        )
        .child(div().text_xs().text_color(muted).child(client.privacy_note()))
        .when(has_reader_context, |this| {
            let chapter_line = session
                .reader_chapter_title
                .as_deref()
                .map(|t| format!("Current chapter: {t}"))
                .unwrap_or_else(|| "Current reader section attached".into());
            let passage_preview = session.reader_passage.as_deref().map(|p| {
                let snippet: String = p.chars().take(120).collect();
                if p.chars().count() > 120 {
                    format!("Selected passage: “{snippet}…”")
                } else {
                    format!("Selected passage: “{snippet}”")
                }
            });
            this.child(
                v_flex()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(chapter_line))
                    .when_some(passage_preview, |box_el, preview| {
                        box_el.child(div().text_xs().text_color(muted).child(preview))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .when(session.reader_passage.is_some(), |row| {
                                row.child(
                                    Button::new("ai-explain-passage")
                                        .primary()
                                        .small()
                                        .label("Explain Passage")
                                        .disabled(busy)
                                        .on_click(cx.listener(|app, _, window, cx| {
                                            app.run_book_task(BookTask::ExplainPassage, window, cx);
                                        })),
                                )
                            })
                            .when(session.reader_chapter_text.is_some(), |row| {
                                row.child(
                                    Button::new("ai-summarize-chapter")
                                        .outline()
                                        .small()
                                        .label("Summarize Chapter")
                                        .disabled(busy)
                                        .on_click(cx.listener(|app, _, window, cx| {
                                            app.run_book_task(
                                                BookTask::SummarizeChapter,
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    ),
            )
        })
        .when(!has_reader_context, |this| {
            this.child(
                Checkbox::new("ai-share-excerpt")
                    .label(format!(
                        "Also share opening excerpt (up to {excerpt_limit} characters)"
                    ))
                    .checked(session.excerpt)
                    .disabled(busy)
                    .on_click(cx.listener(|app, checked: &bool, _, cx| {
                        if let Some(session) = app.online.ai.as_mut() {
                            if session.excerpt != *checked {
                                session.excerpt = *checked;
                                session.history.clear();
                            }
                            cx.notify();
                        }
                    })),
            )
        })
        .child(
            h_flex()
                .gap_2()
                .child(Input::new(&app.online.question).flex_1().disabled(busy))
                .child(
                    Button::new("ai-send")
                        .primary()
                        .small()
                        .label("Send")
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| app.run_ai(false, window, cx))),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("ai-suggest")
                        .outline()
                        .small()
                        .label("Suggest Description & Tags")
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| app.run_ai(true, window, cx))),
                )
                .children(
                    [
                        ("ai-characters", "Characters", BookTask::Characters),
                        ("ai-discuss", "Discussion Questions", BookTask::Discussion),
                        ("ai-time", "Reading Time", BookTask::ReadingTime),
                    ]
                    .into_iter()
                    .map(|(id, label, task)| {
                        Button::new(id)
                            .outline()
                            .small()
                            .label(label)
                            .disabled(busy)
                            .on_click(cx.listener(move |app, _, window, cx| {
                                app.run_book_task(task, window, cx)
                            }))
                    })
                    .collect::<Vec<_>>(),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(Input::new(&app.online.chapter).w(px(95.)).disabled(busy))
                .child(
                    Button::new("ai-recap")
                        .outline()
                        .small()
                        .label(recap_label)
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.run_book_task(BookTask::Recap, window, cx)
                        })),
                )
                .child(Input::new(&app.online.language).w(px(160.)).disabled(busy))
                .child(
                    Button::new("ai-translate")
                        .outline()
                        .small()
                        .label("Translate Description")
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.run_book_task(BookTask::Translate, window, cx)
                        })),
                ),
        )
        .when(!preset_chips.is_empty(), |this| {
            this.child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("Presets:"))
                    .children(preset_chips),
            )
        })
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Recap stops at the chosen Contents entry. Reading Time is computed locally."),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .when(has_copyable, |row| {
                            row.child(
                                Button::new("copy-ai-result")
                                    .ghost()
                                    .small()
                                    .label("Copy Response")
                                    .disabled(busy)
                                    .on_click(cx.listener(|app, _, window, cx| {
                                        app.copy_ai_result(window, cx);
                                    })),
                            )
                        })
                        .when(!session.history.is_empty(), |row| {
                            row.child(
                                Button::new("clear-chat")
                                    .ghost()
                                    .small()
                                    .label("Clear Chat")
                                    .disabled(busy)
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(session) = app.online.ai.as_mut() {
                                            session.history.clear();
                                            cx.notify();
                                        }
                                    })),
                            )
                        })
                        .child(
                            Button::new("ai-configure")
                                .ghost()
                                .small()
                                .label("AI Preferences")
                                .disabled(busy)
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.online.ai = None;
                                    window.close_dialog(cx);
                                    app.open_preferences(window, cx);
                                })),
                        ),
                ),
        )
        .when(!session.status.is_empty(), |this| {
            this.child(div().text_sm().text_color(muted).child(session.status.clone()))
        })
        .when(!session.usage.is_empty(), |this| {
            this.child(div().text_xs().text_color(muted).child(session.usage.clone()))
        })
        .when(
            suggestion.is_some()
                || translation.is_some()
                || !messages.is_empty()
                || !session.streaming.is_empty()
                || !session.local_result.is_empty(),
            |this| {
                this.child(
                    div()
                        .id("ai-response")
                        .max_h(px(310.))
                        .overflow_y_scroll()
                        .child(
                            v_flex()
                                .gap_2()
                                .when_some(suggestion, |this, suggestion| this.child(suggestion))
                                .when_some(translation, |this, translation| this.child(translation))
                                .children(messages)
                                .when(!session.streaming.is_empty(), |this| {
                                    this.child(div().text_sm().p_2().child(session.streaming.clone()))
                                })
                                .when(!session.local_result.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .p_2()
                                            .rounded_md()
                                            .bg(cx.theme().muted)
                                            .child(session.local_result.clone()),
                                    )
                                }),
                        ),
                )
            },
        )
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
