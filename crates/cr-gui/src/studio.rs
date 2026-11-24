use cr_core::ai::{
    CatalogContext, Citation, CleanupDraft, CleanupField, LibraryAnswer, Message, Role,
    SearchDraft, ShelfDraft, CLEANUP_LIMIT,
};
use cr_core::{BookSummary, Error, Library, ListOptions};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{set_input, text_input, CalibreApp, Screen, Shelf, Tone};
use crate::online::StatusTone;
use crate::ui::ai;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Librarian,
    Search,
    Next,
    Shelves,
    Cleanup,
}

impl Mode {
    const ALL: [Self; 5] = [
        Self::Librarian,
        Self::Search,
        Self::Next,
        Self::Shelves,
        Self::Cleanup,
    ];

    fn icon(self) -> Lucide {
        match self {
            Self::Librarian => Lucide::MessagesSquare,
            Self::Search => Lucide::TextSearch,
            Self::Next => Lucide::Compass,
            Self::Shelves => Lucide::LibraryBig,
            Self::Cleanup => Lucide::BrushCleaning,
        }
    }

    /// The short name on the mode switcher.
    fn tab(self) -> &'static str {
        match self {
            Self::Librarian => "Librarian",
            Self::Search => "Search",
            Self::Next => "Read Next",
            Self::Shelves => "Shelves",
            Self::Cleanup => "Cleanup",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Librarian => "Ask about your library",
            Self::Search => "Describe what to find",
            Self::Next => "Find your next read",
            Self::Shelves => "Group books into shelves",
            Self::Cleanup => "Tidy up metadata",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Librarian => "Answers cite books from your catalog. Click a cited book to jump to it.",
            Self::Search => "Your words become a search query that you can review and edit before it runs.",
            Self::Next => "Picks up to five books from your catalog and says why. Add a mood or leave it blank.",
            Self::Shelves => "Proposes themed groups you can save one by one. Add a theme or leave it blank.",
            Self::Cleanup => "Select books in the library, then review proposed fixes field by field. Nothing changes until you apply it.",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Librarian => "Ask a question about your library",
            Self::Search => "Describe the books to find",
            Self::Next => "Optional: a mood, a length, a genre",
            Self::Shelves => "Optional: a theme to group by",
            Self::Cleanup => "",
        }
    }

    fn examples(self) -> &'static [&'static str] {
        match self {
            Self::Librarian => &[
                "Which series have I started?",
                "What is good for a long flight?",
                "What do I own about the sea?",
            ],
            Self::Search => &[
                "Science fiction rated 4 stars or more",
                "Mysteries without a rating",
                "Books added this year",
            ],
            Self::Next => &["Something short and light", "A change of genre"],
            Self::Shelves => &["Group by mood", "Themes that cross genres"],
            Self::Cleanup => &[],
        }
    }

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
    pub(crate) prompt: Entity<InputState>,
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
    tone: StatusTone,
    /// The request shown as the user's turn while the reply is on its way.
    pending: Option<String>,
    scroll: ScrollHandle,
    reply: Option<Reply>,
    history: Vec<Message>,
    cited_ids: Vec<i64>,
}

enum Reply {
    Answer(LibraryAnswer, usize, usize),
    Search(SearchDraft),
    Shelves(Vec<ShelfDraft>, usize, usize),
    Cleanup(Vec<CleanupReview>),
}

impl Session {
    fn set_status(&mut self, tone: StatusTone, status: impl Into<String>) {
        self.tone = tone;
        self.status = status.into();
    }
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
        self.online.studio.prompt.update(cx, |input, cx| {
            input.set_placeholder(mode.placeholder(), window, cx)
        });
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
            tone: StatusTone::Info,
            pending: None,
            scroll: ScrollHandle::new(),
            reply: None,
            history: Vec::new(),
            cited_ids: Vec::new(),
        });
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title("AI Library Studio")
                .w(px(820.))
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |app, cx| {
                        app.online.studio.session = None;
                        cx.notify();
                    });
                });
            let height = window.viewport_size().height;
            match weak.update(cx, |app, cx| body(app, height, cx)) {
                Ok(body) => dialog.child(body),
                Err(_) => dialog,
            }
        });
    }

    fn set_studio_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.online.studio.session.as_mut() else {
            return;
        };
        if session.mode == mode {
            return;
        }
        session.mode = mode;
        session.reply = None;
        session.saved_shelves.clear();
        session.status.clear();
        session.history.clear();
        session.cited_ids.clear();
        session.scroll.set_offset(Point::default());
        set_input(&self.online.studio.prompt, String::new(), window, cx);
        self.online.studio.prompt.update(cx, |input, cx| {
            input.set_placeholder(mode.placeholder(), window, cx)
        });
        cx.notify();
    }

    fn run_studio_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.online.studio.query.read(cx).value().trim().to_owned();
        if !query.is_empty() {
            window.close_dialog(cx);
            self.shelf = Shelf::All;
            self.set_query(query, window, cx);
        }
    }

    pub(crate) fn run_studio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let client = self.ai_client(cx);
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
            session.set_status(
                StatusTone::Info,
                if mode == Mode::Search {
                    "Describe the books to find first."
                } else {
                    "Type a question first."
                },
            );
            cx.notify();
            return;
        }
        let selected = session.selected.clone();
        let seed = session.seed;
        let selected_only = session.selected_only;
        let mut history = session.history.clone();
        let cited_ids = session.cited_ids.clone();
        let submitted = prompt.clone();
        session.status.clear();
        session.pending = Some(match mode {
            Mode::Librarian | Mode::Search => prompt.clone(),
            Mode::Next if prompt.is_empty() => "What should I read next?".into(),
            Mode::Next => format!("What should I read next? {prompt}"),
            Mode::Shelves if prompt.is_empty() => "Group my books into shelves".into(),
            Mode::Shelves => format!("Group my books into shelves: {prompt}"),
            Mode::Cleanup => format!("Review the metadata of {} books", selected.len()),
        });
        session.scroll.scroll_to_bottom();
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
                } else if mode == Mode::Librarian {
                    cited_ids
                } else {
                    Vec::new()
                };
                let prior_query = if mode == Mode::Librarian && !history.is_empty() {
                    history
                        .iter()
                        .filter(|m| matches!(m.role, Role::User))
                        .map(|m| m.content.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    String::new()
                };
                let catalog = CatalogContext::for_conversation(
                    &books,
                    &recent,
                    seed.filter(|_| mode == Mode::Next),
                    &prompt,
                    &prior_query,
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
                session.pending = None;
                // A conversation follows its latest turn; a review starts at the top.
                if mode == Mode::Librarian {
                    session.scroll.scroll_to_bottom();
                } else {
                    session.scroll.set_offset(Point::default());
                }
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
                                for book in &answer.books {
                                    if !session.cited_ids.contains(&book.id) {
                                        session.cited_ids.push(book.id);
                                    }
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
                    Err(Error::Cancelled) => session.set_status(StatusTone::Info, "Stopped."),
                    Err(error) => session.set_status(StatusTone::Error, error.to_string()),
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
        match draft.save(library, &draft.name, session.virtual_library) {
            Ok(()) => {
                if !session.saved_shelves.contains(&index) {
                    session.saved_shelves.push(index);
                }
                let saved = format!(
                    "Saved “{}”. Open Library Tools → Search to use it.",
                    draft.name
                );
                session.set_status(StatusTone::Success, saved);
            }
            Err(error) => session.set_status(StatusTone::Error, error.to_string()),
        }
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
        // Books with nothing left to review leave the list.
        rows.retain(|row| {
            CleanupField::ALL
                .into_iter()
                .any(|field| row.draft.values(&row.book, field).is_some())
        });
        if rows.is_empty() {
            session.reply = None;
        }
        if failures.is_empty() {
            session.set_status(
                StatusTone::Success,
                format!("Applied changes to {applied} books."),
            );
        } else {
            session.set_status(
                StatusTone::Error,
                format!(
                    "Applied changes to {applied} books. Failed: {}",
                    failures.join(" · ")
                ),
            );
        }
        if let Some(id) = self.selected {
            self.load_book(id, window, cx);
        }
        self.refresh_after_book_change(window, cx);
    }
}

/// Cited books as rows that jump to the book in the library.
fn cites(app: &CalibreApp, rows: &[Citation], busy: bool, cx: &mut Context<CalibreApp>) -> Div {
    let theme = cx.theme().clone();
    let children = rows
        .iter()
        .map(|book| {
            let id = book.id;
            h_flex()
                .id(format!("studio-book-{id}"))
                .w_full()
                .gap_3()
                .items_center()
                .px_2()
                .py_1p5()
                .rounded(px(8.))
                .when(!busy, |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(theme.muted))
                        .on_click(
                            cx.listener(move |app, _, window, cx| app.studio_book(id, window, cx)),
                        )
                })
                .tooltip(|window, cx| Tooltip::new("Show in library").build(window, cx))
                .child(ai::book_cover(app, id, &book.title, 26., cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(book.title.clone()),
                        )
                        .when(!book.reason.is_empty(), |column| {
                            column.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(book.reason.clone()),
                            )
                        }),
                )
                .child(
                    Icon::new(Lucide::ArrowRight)
                        .size_4()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground),
                )
        })
        .collect::<Vec<_>>();
    v_flex().w_full().children(children)
}

/// An answer in the assistant's voice with the books it cites underneath.
fn answer_turn(
    app: &CalibreApp,
    id: impl Into<ElementId>,
    text_id: impl Into<ElementId>,
    answer: &str,
    books: &[Citation],
    busy: bool,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    ai::assistant_turn(
        id,
        v_flex()
            .gap_2()
            .child(ai::markdown(text_id, answer.to_string()))
            .when(!books.is_empty(), |column| {
                column.child(cites(app, books, busy, cx))
            }),
        cx,
    )
}

fn cleanup_rows(app: &mut CalibreApp) -> Option<&mut Vec<CleanupReview>> {
    match app.online.studio.session.as_mut()?.reply.as_mut()? {
        Reply::Cleanup(rows) => Some(rows),
        _ => None,
    }
}

pub(crate) fn body(
    app: &CalibreApp,
    viewport_height: Pixels,
    cx: &mut Context<CalibreApp>,
) -> AnyElement {
    let Some(session) = &app.online.studio.session else {
        return div().into_any_element();
    };
    let busy = app.job.is_some();
    let stopping = app.job.as_ref().is_some_and(|job| job.cancelling);
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let client = app.ai_client(cx);
    let provider_name = client.provider_name();
    let mode = session.mode;
    let results_height = (viewport_height - px(380.)).max(px(200.)).min(px(460.));

    let tabs = h_flex()
        .gap_1()
        .p_1()
        .rounded(px(10.))
        .bg(theme.muted.opacity(0.6))
        .children(Mode::ALL.into_iter().map(|tab| {
            let active = mode == tab;
            let button = Button::new(tab.label())
                .small()
                .icon(tab.icon())
                .label(tab.tab())
                .tooltip(tab.label())
                .disabled(busy)
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.set_studio_mode(tab, window, cx);
                }));
            if active {
                button.secondary()
            } else {
                button.ghost()
            }
        }));
    let header = h_flex()
        .gap_2()
        .items_center()
        .child(tabs)
        .child(div().flex_1())
        .child(ai::provider_badge(&client, cx))
        .child(
            ai::icon_action("studio-preferences", Lucide::Settings, "AI Preferences")
                .disabled(busy)
                .on_click(cx.listener(|app, _, window, cx| {
                    app.online.studio.session = None;
                    window.close_dialog(cx);
                    app.open_preferences(window, cx);
                })),
        );

    let mut turns: Vec<AnyElement> = Vec::new();
    match &session.reply {
        Some(Reply::Answer(answer, included, total)) => {
            if mode == Mode::Librarian {
                let last = session.history.len().saturating_sub(1);
                for (index, message) in session.history.iter().enumerate() {
                    turns.push(match message.role {
                        Role::Assistant if index == last => answer_turn(
                            app,
                            ("studio-history", index),
                            ("studio-history-text", index),
                            &message.content,
                            &answer.books,
                            busy,
                            cx,
                        ),
                        Role::Assistant => answer_turn(
                            app,
                            ("studio-history", index),
                            ("studio-history-text", index),
                            &message.content,
                            &[],
                            busy,
                            cx,
                        ),
                        _ => ai::user_turn(("studio-history", index), message.content.clone(), cx),
                    });
                }
            } else {
                turns.push(answer_turn(
                    app,
                    "studio-answer",
                    "studio-answer-text",
                    &answer.answer,
                    &answer.books,
                    busy,
                    cx,
                ));
            }
            turns.push(ai::catalog_meter(*included, *total, cx));
        }
        Some(Reply::Search(draft)) => turns.push(
            ai::card(cx)
                .child(ai::card_title(Lucide::TextSearch, "Draft search", cx))
                .when(!draft.note.is_empty(), |card| {
                    card.child(ai::status_line(StatusTone::Info, draft.note.clone(), cx))
                })
                .child(
                    Input::new(&app.online.studio.query)
                        .w_full()
                        .font_family(theme.mono_font_family.clone()),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            Button::new("studio-run-query")
                                .primary()
                                .small()
                                .icon(Lucide::Search)
                                .label("Run Search")
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.run_studio_query(window, cx);
                                })),
                        )
                        .child(div().text_xs().text_color(muted).child(
                            "Edit the query if it is not quite right. Running it filters the library.",
                        )),
                )
                .into_any_element(),
        ),
        Some(Reply::Shelves(rows, included, total)) => {
            turns.push(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(ai::catalog_meter(*included, *total, cx)))
                    .child(
                        Switch::new("studio-virtual-shelf")
                            .small()
                            .label("Save as virtual library")
                            .tooltip("Off saves each shelf as a saved search instead")
                            .checked(session.virtual_library)
                            .on_click(cx.listener(|app, checked: &bool, _, cx| {
                                if let Some(session) = app.online.studio.session.as_mut() {
                                    session.virtual_library = *checked;
                                    cx.notify();
                                }
                            })),
                    )
                    .into_any_element(),
            );
            if rows.is_empty() {
                turns.push(ai::status_line(StatusTone::Info, "No shelves were proposed.", cx));
            }
            for (index, shelf) in rows.iter().enumerate() {
                let saved = session.saved_shelves.contains(&index);
                turns.push(
                    ai::card(cx)
                        .id(("studio-shelf", index))
                        .child(
                            ai::card_title(Lucide::LibraryBig, shelf.name.clone(), cx)
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("{} books", shelf.books.len())),
                                )
                                .child(
                                    Button::new(("studio-save-shelf", index))
                                        .small()
                                        .icon(if saved { Lucide::Check } else { Lucide::Bookmark })
                                        .label(if saved { "Saved" } else { "Save Shelf" })
                                        .disabled(busy || saved || !app.can_write())
                                        .on_click(cx.listener(move |app, _, _, cx| {
                                            app.save_studio_shelf(index, cx)
                                        }))
                                        .map(|button| {
                                            if saved {
                                                button.ghost()
                                            } else {
                                                button.outline()
                                            }
                                        }),
                                ),
                        )
                        .child(div().text_sm().child(shelf.reason.clone()))
                        .child(cites(app, &shelf.books, busy, cx))
                        .into_any_element(),
                );
            }
        }
        Some(Reply::Cleanup(rows)) => {
            let proposed: usize = rows
                .iter()
                .map(|row| {
                    CleanupField::ALL
                        .into_iter()
                        .filter(|field| row.draft.values(&row.book, *field).is_some())
                        .count()
                })
                .sum();
            let checked: usize = rows.iter().map(|row| row.fields.len()).sum();
            if rows.is_empty() {
                turns.push(ai::status_line(
                    StatusTone::Success,
                    "Nothing to clean up. The metadata of these books already looks consistent.",
                    cx,
                ));
            } else {
                turns.push(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(muted)
                                .child(format!(
                                    "{proposed} proposed changes in {} books · {checked} ticked",
                                    rows.len()
                                )),
                        )
                        .child(
                            Button::new("studio-cleanup-select-all")
                                .ghost()
                                .small()
                                .label("Tick All")
                                .disabled(busy || !app.can_write())
                                .on_click(cx.listener(|app, _, _, cx| {
                                    if let Some(rows) = cleanup_rows(app) {
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
                                .label("Untick All")
                                .disabled(busy || checked == 0)
                                .on_click(cx.listener(|app, _, _, cx| {
                                    if let Some(rows) = cleanup_rows(app) {
                                        for row in rows.iter_mut() {
                                            row.fields.clear();
                                        }
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            Button::new("studio-apply-cleanup")
                                .primary()
                                .small()
                                .icon(Lucide::Check)
                                .label(match checked {
                                    0 => "Apply Ticked".to_string(),
                                    1 => "Apply 1 Change".to_string(),
                                    count => format!("Apply {count} Changes"),
                                })
                                .disabled(busy || !app.can_write() || checked == 0)
                                .on_click(cx.listener(|app, _, window, cx| {
                                    app.apply_studio_cleanup(window, cx)
                                })),
                        )
                        .into_any_element(),
                );
            }
            for (index, row) in rows.iter().enumerate() {
                let fields = CleanupField::ALL
                    .into_iter()
                    .filter_map(|field| {
                        let (before, after) = row.draft.values(&row.book, field)?;
                        Some(
                            h_flex()
                                .gap_3()
                                .items_start()
                                .pt_2()
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    div().w(px(150.)).flex_shrink_0().child(
                                        Checkbox::new(format!(
                                            "studio-cleanup-{index}-{}",
                                            field.label()
                                        ))
                                        .label(field.label())
                                        .checked(row.fields.contains(&field))
                                        .disabled(busy || !app.can_write())
                                        .on_click(cx.listener(
                                            move |app, checked: &bool, _, cx| {
                                                if let Some(rows) = cleanup_rows(app) {
                                                    let fields = &mut rows[index].fields;
                                                    fields.retain(|f| *f != field);
                                                    if *checked {
                                                        fields.push(field);
                                                    }
                                                    cx.notify();
                                                }
                                            },
                                        )),
                                    ),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .line_through()
                                                .child(if before.is_empty() {
                                                    "(empty)".to_string()
                                                } else {
                                                    before
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1p5()
                                                .items_start()
                                                .child(
                                                    Icon::new(Lucide::ArrowRight)
                                                        .size_3()
                                                        .mt(px(4.))
                                                        .flex_shrink_0()
                                                        .text_color(theme.success),
                                                )
                                                .child(div().flex_1().min_w_0().text_sm().child(after)),
                                        ),
                                ),
                        )
                    })
                    .collect::<Vec<_>>();
                turns.push(
                    ai::card(cx)
                        .id(("studio-cleanup-book", index))
                        .child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .child(ai::book_cover(app, row.book.id, &row.book.title, 26., cx))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(row.book.title.clone()),
                                        )
                                        .when(!row.draft.reason.is_empty(), |column| {
                                            column.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(row.draft.reason.clone()),
                                            )
                                        }),
                                ),
                        )
                        .children(fields)
                        .into_any_element(),
                );
            }
        }
        None => {
            // An unanswered follow-up keeps the conversation so far in view.
            if mode == Mode::Librarian {
                for (index, message) in session.history.iter().enumerate() {
                    turns.push(match message.role {
                        Role::Assistant => answer_turn(
                            app,
                            ("studio-history", index),
                            ("studio-history-text", index),
                            &message.content,
                            &[],
                            busy,
                            cx,
                        ),
                        _ => ai::user_turn(("studio-history", index), message.content.clone(), cx),
                    });
                }
            }
        }
    }
    if let Some(pending) = &session.pending {
        turns.push(ai::user_turn("studio-pending", pending.clone(), cx));
        turns.push(ai::thinking(
            if stopping {
                "Stopping…".to_string()
            } else {
                format!("Waiting for {provider_name}…")
            },
            cx,
        ));
    }
    let results = div()
        .id("studio-response")
        .w_full()
        .h(results_height)
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .overflow_y_scroll()
        .track_scroll(&session.scroll)
        .map(|area| {
            if turns.is_empty() {
                let examples = mode.examples().iter().enumerate().map(|(index, example)| {
                    Button::new(("studio-example", index))
                        .outline()
                        .small()
                        .label(*example)
                        .disabled(busy)
                        .on_click(cx.listener(move |app, _, window, cx| {
                            set_input(&app.online.studio.prompt, (*example).into(), window, cx);
                            app.run_studio(window, cx);
                        }))
                });
                area.child(
                    ai::empty_panel(mode.icon(), mode.title(), mode.hint(), cx).child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap_2()
                            .pt_1()
                            .children(examples),
                    ),
                )
            } else {
                area.child(v_flex().gap_3().p_3().children(turns))
            }
        });

    let privacy = if client.is_on_device() {
        format!("{} No book text is shared here.", client.privacy_note())
    } else {
        format!(
            "Sends your request and catalog metadata to {provider_name}. No book text is shared here."
        )
    };
    // Hints and confirmations take the privacy line's place, so only an
    // error moves the layout.
    let has_status = !busy && !session.status.is_empty();
    let quiet_status = has_status && session.tone != StatusTone::Error;
    let cleanup_ready = !session.selected.is_empty() && session.selected.len() <= CLEANUP_LIMIT;
    let scope = h_flex()
        .gap_3()
        .items_center()
        .min_h(px(24.))
        .when(mode == Mode::Shelves, |row| {
            row.child(
                Switch::new("studio-selected-scope")
                    .small()
                    .label(match session.selected.len() {
                        1 => "Only the selected book".to_string(),
                        count => format!("Only the {count} selected books"),
                    })
                    .checked(session.selected_only)
                    .disabled(busy || session.selected.is_empty())
                    .on_click(cx.listener(|app, checked: &bool, _, cx| {
                        if let Some(session) = app.online.studio.session.as_mut() {
                            session.selected_only = *checked;
                            cx.notify();
                        }
                    })),
            )
        })
        .when(mode == Mode::Next, |row| {
            let (icon, label) = match (&session.seed, &session.seed_title) {
                (Some(_), Some(title)) => (Lucide::Bookmark, format!("Starting from {title}")),
                (Some(id), None) => (Lucide::Bookmark, format!("Starting from book #{id}")),
                _ => (Lucide::Clock, "Based on recent reads and ratings".to_string()),
            };
            row.child(
                h_flex()
                    .id("studio-seed")
                    .flex_shrink_0()
                    .max_w(px(320.))
                    .gap_1p5()
                    .text_xs()
                    .child(Icon::new(icon).size_3().text_color(muted))
                    .child(div().truncate().child(label))
                    .tooltip(|window, cx| {
                        Tooltip::new(
                            "To start from one book, right-click it and choose AI Tools, then Read Next from This Book.",
                        )
                        .build(window, cx)
                    }),
            )
        })
        .when(mode == Mode::Cleanup, |row| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(if cleanup_ready { muted } else { theme.warning })
                    .child(match session.selected.len() {
                        0 => format!("Select 1 to {CLEANUP_LIMIT} books in the library first"),
                        count if count > CLEANUP_LIMIT => {
                            format!("{count} selected, the limit is {CLEANUP_LIMIT} per review")
                        }
                        1 => "1 book selected".to_string(),
                        count => format!("{count} books selected"),
                    }),
            )
        })
        .child(if quiet_status {
            div()
                .flex_1()
                .min_w_0()
                .child(ai::status_line(session.tone, session.status.clone(), cx))
        } else {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1p5()
                .items_center()
                .child(Icon::new(Lucide::ShieldCheck).size_3().flex_shrink_0().text_color(muted))
                .child(div().truncate().text_xs().text_color(muted).child(privacy))
        })
        .when(mode == Mode::Librarian && !session.history.is_empty(), |row| {
            row.child(
                ai::icon_action("studio-clear-history", Lucide::Eraser, "Clear Conversation")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| {
                        if let Some(session) = app.online.studio.session.as_mut() {
                            session.history.clear();
                            session.cited_ids.clear();
                            session.reply = None;
                            session.status.clear();
                            cx.notify();
                        }
                    })),
            )
        });

    let run = if busy {
        Button::new("studio-stop")
            .outline()
            .icon(Lucide::Square)
            .label(if stopping { "Stopping…" } else { "Stop" })
            .disabled(stopping)
            .on_click(cx.listener(|app, _, _, cx| app.cancel_job(cx)))
    } else {
        Button::new("studio-generate")
            .primary()
            .icon(mode.icon())
            .label(match mode {
                Mode::Librarian => "Ask".to_string(),
                Mode::Search => "Draft Search".to_string(),
                Mode::Next => "Recommend".to_string(),
                Mode::Shelves => "Curate Shelves".to_string(),
                Mode::Cleanup => match session.selected.len() {
                    1 => "Review 1 Book".to_string(),
                    count if cleanup_ready => format!("Review {count} Books"),
                    _ => "Review Books".to_string(),
                },
            })
            .disabled(mode == Mode::Cleanup && !cleanup_ready)
            .on_click(cx.listener(|app, _, window, cx| app.run_studio(window, cx)))
    };
    let composer = h_flex()
        .gap_2()
        .items_center()
        .when(mode != Mode::Cleanup, |row| {
            row.child(Input::new(&app.online.studio.prompt).flex_1().disabled(busy))
        })
        .when(mode == Mode::Cleanup, |row| {
            row.child(div().flex_1().text_sm().text_color(muted).child(
                "Fixes title casing, author names, series and genre tags. You choose what to apply.",
            ))
        })
        .child(run);

    v_flex()
        .gap_3()
        .w_full()
        .child(header)
        .child(results)
        .child(scope)
        .when(has_status && !quiet_status, |this| {
            this.child(ai::status_line(session.tone, session.status.clone(), cx))
        })
        .child(composer)
        .into_any_element()
}
