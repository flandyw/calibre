use cr_core::ai::{
    CatalogContext, Citation, CleanupDraft, CleanupField, LibraryAnswer, Message, Role,
    SearchDraft, ShelfDraft, CLEANUP_LIMIT,
};
use cr_core::{BookSummary, Error, Library, ListOptions};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{set_input, text_input, CalibreApp, Screen, Shelf, Tone};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Librarian,
    Search,
    Next,
    Shelves,
    Cleanup,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Librarian => "Librarian",
            Self::Search => "Natural Search",
            Self::Next => "Read Next",
            Self::Shelves => "Smart Shelves",
            Self::Cleanup => "Metadata Cleanup",
        }
    }
}

pub struct Studio {
    prompt: Entity<InputState>,
    query: Entity<InputState>,
    pub session: Option<Session>,
}

impl Studio {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        Self {
            prompt: text_input("What are you looking for?", window, cx),
            query: text_input("Review and edit the draft query", window, cx),
            session: None,
        }
    }
}

pub struct Session {
    mode: Mode,
    selected: Vec<i64>,
    seed: Option<i64>,
    seed_title: Option<String>,
    selected_only: bool,
    virtual_library: bool,
    saved_shelves: Vec<usize>,
    status: String,
    reply: Option<Reply>,
    history: Vec<Message>,
}

enum Reply {
    Answer(LibraryAnswer, usize, usize),
    Search(SearchDraft),
    Shelves(Vec<ShelfDraft>, usize, usize),
    Cleanup(Vec<CleanupReview>),
}

struct CleanupReview {
    draft: CleanupDraft,
    book: BookSummary,
    fields: Vec<CleanupField>,
}

impl CalibreApp {
    pub fn open_studio(
        &mut self,
        mode: Mode,
        use_seed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen != Screen::Library
            || self.library.is_none()
            || self.job.is_some()
            || self.editing
            || window.has_active_dialog(cx)
        {
            return;
        }
        set_input(&self.online.studio.prompt, String::new(), window, cx);
        let client = self.ai_client(cx);
        let title = format!("AI Library Studio · {}", client.provider_name());
        let seed = if use_seed { self.selected } else { None };
        let seed_title = seed.and_then(|id| {
            self.books
                .iter()
                .find(|book| book.id == id)
                .map(|book| book.title.clone())
        });
        self.online.studio.session = Some(Session {
            mode,
            selected: self.selection_ids(),
            seed,
            seed_title,
            selected_only: false,
            virtual_library: false,
            saved_shelves: Vec::new(),
            status: String::new(),
            reply: None,
            history: Vec::new(),
        });
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title(title.clone())
                .w(px(820.))
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |app, cx| {
                        app.online.studio.session = None;
                        cx.notify();
                    });
                });
            match weak.update(cx, |app, cx| body(app, cx)) {
                Ok(body) => dialog.child(body),
                Err(_) => dialog,
            }
        });
    }

    fn run_studio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.ai_client(cx);
        let provider_name = client.provider_name();
        let budget = client.context_budget();
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let root = library.root().to_path_buf();
        let prompt = self.online.studio.prompt.read(cx).value().trim().to_owned();
        let Some(session) = self.online.studio.session.as_mut() else {
            return;
        };
        let mode = session.mode;
        if matches!(mode, Mode::Librarian | Mode::Search) && prompt.is_empty() {
            session.status = "Enter a question or search request.".into();
            cx.notify();
            return;
        }
        let selected = session.selected.clone();
        let seed = session.seed;
        let selected_only = session.selected_only;
        let mut history = session.history.clone();
        let submitted = prompt.clone();
        session.status = format!("Waiting for {provider_name}…");
        session.saved_shelves.clear();
        if mode != Mode::Librarian {
            session.reply = None;
        }
        self.run_job(
            mode.label(),
            window,
            cx,
            move |job| {
                let library = Library::open(&root)?;
                job.check()?;
                if mode == Mode::Search {
                    return client.draft_search(&library, &prompt).map(Reply::Search);
                }
                if mode == Mode::Cleanup {
                    if selected.is_empty() || selected.len() > CLEANUP_LIMIT {
                        return Err(Error::Other(format!(
                            "Select 1–{CLEANUP_LIMIT} books for cleanup."
                        )));
                    }
                    let books = selected
                        .iter()
                        .map(|id| library.get_summary(*id))
                        .collect::<cr_core::Result<Vec<_>>>()?;
                    let drafts = client.cleanup(&books)?;
                    return Ok(Reply::Cleanup(
                        drafts
                            .into_iter()
                            .filter_map(|draft| {
                                let book = books.iter().find(|book| book.id == draft.id)?.clone();
                                CleanupField::ALL
                                    .into_iter()
                                    .any(|field| draft.values(&book, field).is_some())
                                    .then_some(CleanupReview {
                                        draft,
                                        book,
                                        fields: Vec::new(),
                                    })
                            })
                            .collect(),
                    ));
                }
                let books = if mode == Mode::Shelves && selected_only {
                    selected
                        .iter()
                        .map(|id| library.get_summary(*id))
                        .collect::<cr_core::Result<Vec<_>>>()?
                } else {
                    library.list_books(&ListOptions::default())?
                };
                let recent = if mode == Mode::Next && seed.is_none() {
                    library.recent_read_ids(12)?
                } else {
                    Vec::new()
                };
                let catalog = CatalogContext::for_request(
                    &books,
                    &recent,
                    seed.filter(|_| mode == Mode::Next),
                    &prompt,
                    budget,
                );
                let (included, total) = (catalog.included(), catalog.total);
                if mode == Mode::Shelves {
                    return client
                        .shelves(&catalog, &prompt)
                        .map(|rows| Reply::Shelves(rows, included, total));
                }
                let answer = if mode == Mode::Next {
                    client.recommend(&catalog, &prompt)?
                } else {
                    history.push(Message::new(Role::User, prompt));
                    client.librarian(&catalog, &history)?
                };
                Ok(Reply::Answer(answer, included, total))
            },
            move |app, result, window, cx| {
                let Some(session) = app.online.studio.session.as_mut() else {
                    return;
                };
                session.status.clear();
                match result {
                    Ok(reply) => {
                        if let Reply::Search(draft) = &reply {
                            set_input(&app.online.studio.query, draft.query.clone(), window, cx);
                        }
                        if let Reply::Answer(answer, ..) = &reply {
                            if mode == Mode::Librarian {
                                if session.history.len() >= 6 {
                                    session.history.drain(..2);
                                }
                                session.history.push(Message::new(Role::User, submitted));
                                session
                                    .history
                                    .push(Message::new(Role::Assistant, answer.answer.clone()));
                                set_input(&app.online.studio.prompt, String::new(), window, cx);
                            }
                        }
                        session.reply = Some(reply);
                    }
                    Err(error) => session.status = error.to_string(),
                }
            },
        );
    }

    fn studio_book(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.books.iter().any(|book| book.id == id) {
            self.shelf = Shelf::All;
            self.set_query(String::new(), window, cx);
        }
        if let Some(index) = self.books.iter().position(|book| book.id == id) {
            window.close_dialog(cx);
            self.click_index(index, false, false, window, cx);
        } else {
            self.announce(
                "This book is no longer in the library.",
                Tone::Info,
                window,
                cx,
            );
        }
    }

    fn save_studio_shelf(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.can_write() || self.job.is_some() {
            return;
        }
        let Some(session) = self.online.studio.session.as_mut() else {
            return;
        };
        let Some(Reply::Shelves(rows, ..)) = &session.reply else {
            return;
        };
        let Some(draft) = rows.get(index) else { return };
        let Some(library) = self.library.as_mut() else {
            return;
        };
        session.status = match draft.save(library, &draft.name, session.virtual_library) {
            Ok(()) => {
                if !session.saved_shelves.contains(&index) {
                    session.saved_shelves.push(index);
                }
                format!(
                    "Saved “{}”. Open Library Tools → Search to use it.",
                    draft.name
                )
            }
            Err(error) => error.to_string(),
        };
        cx.notify();
    }

    fn apply_studio_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_write() || self.job.is_some() {
            return;
        }
        let Some(session) = self.online.studio.session.as_mut() else {
            return;
        };
        let Some(Reply::Cleanup(rows)) = session.reply.as_mut() else {
            return;
        };
        let Some(library) = self.library.as_mut() else {
            return;
        };
        let mut failures = Vec::new();
        let mut applied = 0;
        for row in rows.iter_mut().filter(|row| !row.fields.is_empty()) {
            let id = row.book.id;
            let result = library.get_metadata(id).and_then(|current| {
                let update = row.draft.update(&current, &row.fields);
                library.update_book(id, &update)
            });
            match result {
                Ok(()) => {
                    applied += 1;
                    row.fields.clear();
                    if let Ok(summary) = library.get_summary(id) {
                        row.book = summary;
                    }
                }
                Err(error) => failures.push(format!("#{id}: {error}")),
            }
        }
        session.status = format!("Applied to {applied} books. {}", failures.join(" · "));
        if let Some(id) = self.selected {
            self.load_book(id, window, cx);
        }
        self.refresh_after_book_change(window, cx);
    }
}

fn cites(rows: &[Citation], busy: bool, cx: &mut Context<CalibreApp>) -> Div {
    let children = rows
        .iter()
        .map(|book| {
            let id = book.id;
            h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new(format!("studio-book-{id}"))
                        .outline()
                        .small()
                        .label(book.title.clone())
                        .disabled(busy)
                        .on_click(
                            cx.listener(move |app, _, window, cx| app.studio_book(id, window, cx)),
                        ),
                )
                .child(div().text_sm().child(book.reason.clone()))
        })
        .collect::<Vec<_>>();
    v_flex().gap_1().children(children)
}

fn body(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let Some(session) = &app.online.studio.session else {
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
    let mode_buttons = [
        Mode::Librarian,
        Mode::Search,
        Mode::Next,
        Mode::Shelves,
        Mode::Cleanup,
    ]
    .into_iter()
    .map(|mode| {
        let active = session.mode == mode;
        let button = Button::new(mode.label())
            .small()
            .label(mode.label())
            .disabled(busy)
            .on_click(cx.listener(move |app, _, _, cx| {
                if let Some(session) = app.online.studio.session.as_mut() {
                    if session.mode != mode {
                        session.mode = mode;
                        session.reply = None;
                        session.saved_shelves.clear();
                        session.status.clear();
                        session.history.clear();
                        cx.notify();
                    }
                }
            }));
        if active {
            button.primary()
        } else {
            button.outline()
        }
    })
    .collect::<Vec<_>>();
    let hint = match session.mode {
        Mode::Librarian => "Ask a question about your catalog, then click a cited book to select it.",
        Mode::Search => "Describe a search. Review and edit the generated query before running it.",
        Mode::Next => "Optionally describe your mood. Uses a chosen book or recent reads and ratings.",
        Mode::Shelves => "Optionally describe a theme. Preview and save each group independently.",
        Mode::Cleanup => "Preview title casing, author sort, series and genre tags. Check only the fields you want to edit.",
    };
    let librarian_history = if session.mode == Mode::Librarian && session.history.len() > 2 {
        let prior = &session.history[..session.history.len().saturating_sub(2)];
        prior
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let role = match message.role {
                    Role::Assistant => provider_name,
                    _ => "You",
                };
                v_flex()
                    .id(("studio-history", index))
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .child(div().text_xs().text_color(muted).child(role))
                    .child(div().text_sm().child(message.content.clone()))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let response = match &session.reply {
        None => div().into_any_element(),
        Some(Reply::Answer(answer, included, total)) => v_flex()
            .gap_2()
            .children(librarian_history)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Catalog context: {included} of {total} books")),
            )
            .child(
                v_flex()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .child(div().text_sm().child(answer.answer.clone()))
                    .child(cites(&answer.books, busy, cx)),
            )
            .into_any_element(),
        Some(Reply::Search(draft)) => v_flex()
            .gap_2()
            .child(div().text_sm().child(draft.note.clone()))
            .child(Input::new(&app.online.studio.query).w_full())
            .child(
                Button::new("studio-run-query")
                    .primary()
                    .small()
                    .label("Run Reviewed Query")
                    .on_click(cx.listener(|app, _, window, cx| {
                        let query = app.online.studio.query.read(cx).value().trim().to_owned();
                        if !query.is_empty() {
                            window.close_dialog(cx);
                            app.shelf = Shelf::All;
                            app.set_query(query, window, cx);
                        }
                    })),
            )
            .into_any_element(),
        Some(Reply::Shelves(rows, included, total)) => {
            let shelves = rows
                .iter()
                .enumerate()
                .map(|(index, shelf)| {
                    let saved = session.saved_shelves.contains(&index);
                    v_flex()
                        .id(("studio-shelf", index))
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(shelf.name.clone()),
                        )
                        .child(div().text_sm().child(shelf.reason.clone()))
                        .child(div().text_xs().text_color(muted).child(shelf.query()))
                        .child(cites(&shelf.books, busy, cx))
                        .child(
                            Button::new(("studio-save-shelf", index))
                                .outline()
                                .small()
                                .label(if saved { "Saved" } else { "Save This Shelf" })
                                .disabled(busy || saved || !app.can_write())
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.save_studio_shelf(index, cx)
                                })),
                        )
                })
                .collect::<Vec<_>>();
            v_flex()
                .gap_2()
                .child(div().text_xs().text_color(muted).child(format!(
                    "Catalog context: {included} of {total} books · {} proposals",
                    rows.len()
                )))
                .child(
                    Checkbox::new("studio-virtual-shelf")
                        .label("Save as virtual library instead of saved search")
                        .checked(session.virtual_library)
                        .on_click(cx.listener(|app, checked: &bool, _, cx| {
                            if let Some(session) = app.online.studio.session.as_mut() {
                                session.virtual_library = *checked;
                                cx.notify();
                            }
                        })),
                )
                .children(shelves)
                .into_any_element()
        }
        Some(Reply::Cleanup(rows)) => {
            let reviews = rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let fields = CleanupField::ALL
                        .into_iter()
                        .filter_map(|field| {
                            let (before, after) = row.draft.values(&row.book, field)?;
                            Some(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        Checkbox::new(format!(
                                            "studio-cleanup-{index}-{}",
                                            field.label()
                                        ))
                                        .label(field.label())
                                        .checked(row.fields.contains(&field))
                                        .disabled(busy || !app.can_write())
                                        .on_click(
                                            cx.listener(move |app, checked: &bool, _, cx| {
                                                if let Some(Reply::Cleanup(rows)) = app
                                                    .online
                                                    .studio
                                                    .session
                                                    .as_mut()
                                                    .and_then(|s| s.reply.as_mut())
                                                {
                                                    let fields = &mut rows[index].fields;
                                                    fields.retain(|f| *f != field);
                                                    if *checked {
                                                        fields.push(field);
                                                    }
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    )
                                    .child(div().text_sm().text_color(muted).child(format!("Current: {before}")))
                                    .child(div().text_sm().child(format!("Proposed: {after}"))),
                            )
                        })
                        .collect::<Vec<_>>();
                    v_flex()
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(row.book.title.clone()),
                        )
                        .child(div().text_xs().text_color(muted).child(row.draft.reason.clone()))
                        .children(fields)
                })
                .collect::<Vec<_>>();
            v_flex()
                .gap_2()
                .when(!rows.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("studio-cleanup-select-all")
                                    .ghost()
                                    .small()
                                    .label("Select All")
                                    .disabled(busy || !app.can_write())
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(Reply::Cleanup(rows)) = app
                                            .online
                                            .studio
                                            .session
                                            .as_mut()
                                            .and_then(|s| s.reply.as_mut())
                                        {
                                            for row in rows.iter_mut() {
                                                row.fields = CleanupField::ALL
                                                    .into_iter()
                                                    .filter(|field| {
                                                        row.draft.values(&row.book, *field).is_some()
                                                    })
                                                    .collect();
                                            }
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("studio-cleanup-clear")
                                    .ghost()
                                    .small()
                                    .label("Clear")
                                    .disabled(busy)
                                    .on_click(cx.listener(|app, _, _, cx| {
                                        if let Some(Reply::Cleanup(rows)) = app
                                            .online
                                            .studio
                                            .session
                                            .as_mut()
                                            .and_then(|s| s.reply.as_mut())
                                        {
                                            for row in rows.iter_mut() {
                                                row.fields.clear();
                                            }
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                })
                .when(rows.is_empty(), |this| this.child("No changes suggested."))
                .children(reviews)
                .child(
                    Button::new("studio-apply-cleanup")
                        .primary()
                        .small()
                        .label("Apply Checked Fields")
                        .disabled(
                            busy || !app.can_write() || rows.iter().all(|r| r.fields.is_empty()),
                        )
                        .on_click(
                            cx.listener(|app, _, window, cx| app.apply_studio_cleanup(window, cx)),
                        ),
                )
                .into_any_element()
        }
    };
    let privacy_copy = if client.is_on_device() {
        format!("{} No book text is shared here.", client.privacy_note())
    } else {
        format!(
            "Sending shares your request and catalog metadata with {provider_name}. No book text is shared here."
        )
    };
    v_flex().gap_3().w_full()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_2()
                .child(h_flex().gap_2().flex_wrap().children(mode_buttons))
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
        .child(div().text_sm().child(hint))
        .child(div().text_xs().text_color(muted).child(privacy_copy))
        .when(session.mode != Mode::Cleanup, |this| this.child(Input::new(&app.online.studio.prompt).w_full().disabled(busy)))
        .when(session.mode == Mode::Shelves, |this| this.child(Checkbox::new("studio-selected-scope")
            .label(format!("Only use the {} selected books", session.selected.len())).checked(session.selected_only)
            .disabled(busy || session.selected.is_empty()).on_click(cx.listener(|app, checked: &bool, _, cx| {
                if let Some(session) = app.online.studio.session.as_mut() { session.selected_only = *checked; cx.notify(); }
            }))))
        .when(session.mode == Mode::Next, |this| this.child(div().text_sm().child(match (&session.seed, &session.seed_title) {
            (Some(_), Some(title)) => format!("Seed book: {title}"),
            (Some(id), None) => format!("Seed book: #{id}"),
            _ => "Using recent reads and ratings. To use one book as a seed, right-click it → Read Next from This Book…".into(),
        })))
        .when(session.mode == Mode::Cleanup, |this| this.child(format!("{} selected · up to {CLEANUP_LIMIT} per review", session.selected.len())))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("studio-generate")
                        .primary()
                        .small()
                        .label(if session.mode == Mode::Librarian {
                            "Ask Librarian"
                        } else {
                            "Generate Preview"
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| app.run_studio(window, cx))),
                )
                .when(session.mode == Mode::Librarian && !session.history.is_empty(), |row| {
                    row.child(
                        Button::new("studio-clear-history")
                            .ghost()
                            .small()
                            .label("Clear Conversation")
                            .disabled(busy)
                            .on_click(cx.listener(|app, _, _, cx| {
                                if let Some(session) = app.online.studio.session.as_mut() {
                                    session.history.clear();
                                    session.reply = None;
                                    cx.notify();
                                }
                            })),
                    )
                })
                .child(
                    Button::new("studio-preferences")
                        .ghost()
                        .small()
                        .label("AI Preferences")
                        .disabled(busy)
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.online.studio.session = None;
                            window.close_dialog(cx);
                            app.open_preferences(window, cx);
                        })),
                ),
        )
        .when(!session.status.is_empty(), |this| this.child(div().text_sm().text_color(muted).child(session.status.clone())))
        .child(div().id("studio-response").max_h(px(330.)).overflow_y_scroll().child(response))
        .into_any_element()
}
