use std::collections::VecDeque;
use std::sync::Arc;

use cr_core::ai::{BookContext, Message, Model, OpenRouter, Role, Suggestion};
use cr_core::online_metadata::{Candidate, Catalog, Field, Query, Source};
use cr_core::{BookUpdate, Error, Library, Metadata};
use gpui_kit::component::input::InputState;
use gpui_kit::component::*;
use gpui_kit::*;

use crate::app::{set_input, text_input, CalibreApp, Screen, Tone};

pub struct Online {
    pub key: Entity<InputState>,
    pub model: Entity<InputState>,
    pub models: Vec<Model>,
    pub connection: Option<String>,
    pub title: Entity<InputState>,
    pub author: Entity<InputState>,
    pub isbn: Entity<InputState>,
    pub question: Entity<InputState>,
    pub metadata: Option<MetadataSession>,
    pub ai: Option<AiSession>,
}

impl Online {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        let key = text_input("OPENROUTER_API_KEY or session key", window, cx);
        key.update(cx, |input, cx| input.set_masked(true, window, cx));
        Self {
            key,
            model: text_input("Model ID", window, cx),
            models: Vec::new(),
            connection: None,
            title: text_input("Title", window, cx),
            author: text_input("Author", window, cx),
            isbn: text_input("ISBN (optional)", window, cx),
            question: text_input("Ask a question about this book", window, cx),
            metadata: None,
            ai: None,
        }
    }

    pub fn clear_dialogs(&mut self) {
        self.metadata = None;
        self.ai = None;
    }
}

pub struct MetadataSession {
    pub id: i64,
    pub current: Metadata,
    pub current_cover: Option<Arc<Image>>,
    pub remaining: VecDeque<i64>,
    pub reviewed: usize,
    pub total: usize,
    pub source: Source,
    pub results: Vec<SearchResult>,
    pub preview: Option<Preview>,
    pub status: String,
}

pub struct SearchResult {
    pub candidate: Candidate,
    pub thumbnail: Option<Arc<Image>>,
}

pub struct Preview {
    pub candidate: Candidate,
    pub cover: Option<Arc<Image>>,
    pub cover_bytes: Option<Vec<u8>>,
    pub fields: Vec<Field>,
}

pub struct AiSession {
    pub id: i64,
    pub current: Metadata,
    pub excerpt: bool,
    pub history: Vec<Message>,
    pub suggestion: Option<Suggestion>,
    pub fields: Vec<Field>,
    pub status: String,
}

fn picture(bytes: &[u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec()))
}

impl CalibreApp {
    fn openrouter(&self, cx: &App) -> OpenRouter {
        let key = self.online.key.read(cx).value();
        OpenRouter::new(Some(key.as_ref()), &self.settings.openrouter_model)
    }

    pub fn save_openrouter_model(
        &mut self,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = model.trim();
        let model = if model.is_empty() {
            cr_core::ai::DEFAULT_MODEL
        } else {
            model
        };
        set_input(&self.online.model, model.into(), window, cx);
        self.update_preferences(|settings| settings.openrouter_model = model.into(), cx);
    }

    pub fn test_openrouter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.openrouter(cx);
        self.online.connection = Some("Checking your API key…".into());
        self.run_job(
            "Testing OpenRouter",
            window,
            cx,
            move |_| client.test_connection(),
            |app, result, _, _| {
                app.online.connection = Some(match result {
                    Ok(()) => "Connected · API key verified".into(),
                    Err(error) => error.to_string(),
                });
            },
        );
    }

    pub fn load_openrouter_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.openrouter(cx);
        self.online.connection = Some("Loading OpenRouter models…".into());
        self.run_job(
            "Loading OpenRouter models",
            window,
            cx,
            move |_| client.models(),
            |app, result, _, _| {
                app.online.connection = Some(match result {
                    Ok(models) => {
                        app.online.models = models;
                        format!(
                            "{} models available. Type a filter above or choose a model below.",
                            app.online.models.len()
                        )
                    }
                    Err(error) => error.to_string(),
                });
            },
        );
    }

    pub fn open_online_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Library
            || self.editing
            || self.job.is_some()
            || window.has_active_dialog(cx)
        {
            return;
        }
        let mut ids = VecDeque::from(self.selection_ids());
        let total = ids.len();
        let Some(id) = ids.pop_front() else { return };
        self.prepare_metadata(id, ids, total, Source::OpenLibrary, window, cx);
        if self.online.metadata.is_none() {
            return;
        }
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title("Download Metadata")
                .w(px(900.))
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |app, cx| {
                        app.online.metadata = None;
                        cx.notify();
                    });
                });
            match weak.update(cx, |app, cx| crate::ui::online::metadata_dialog(app, cx)) {
                Ok(body) => dialog.child(body),
                Err(_) => dialog,
            }
        });
    }

    fn prepare_metadata(
        &mut self,
        id: i64,
        remaining: VecDeque<i64>,
        total: usize,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let current = match library.get_metadata(id) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.announce(error.to_string(), Tone::Error, window, cx);
                return;
            }
        };
        let query = Query::from_book(&current);
        set_input(&self.online.title, query.title, window, cx);
        set_input(&self.online.author, query.author, window, cx);
        set_input(&self.online.isbn, query.isbn, window, cx);
        let current_cover = current.cover.as_deref().map(picture);
        let reviewed = total - remaining.len();
        self.online.metadata = Some(MetadataSession {
            id,
            current,
            current_cover,
            remaining,
            reviewed,
            total,
            source,
            results: Vec::new(),
            preview: None,
            status: String::new(),
        });
        cx.notify();
    }

    pub fn search_online_metadata(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.online.metadata.as_mut() else {
            return;
        };
        session.source = source;
        session.results.clear();
        session.preview = None;
        session.status = format!("Searching {source}…");
        let query = Query {
            title: self.online.title.read(cx).value().to_string(),
            author: self.online.author.read(cx).value().to_string(),
            isbn: self.online.isbn.read(cx).value().to_string(),
        };
        self.run_job("Searching book catalogs", window, cx, move |job| {
            let catalog = Catalog::default();
            let candidates = catalog.search(source, &query)?;
            job.check()?;
            std::thread::scope(|scope| {
                candidates.into_iter().map(|candidate| {
                    let catalog = &catalog;
                    scope.spawn(move || {
                        let thumbnail = catalog.cover(&candidate, true).ok().flatten();
                        (candidate, thumbnail)
                    })
                }).collect::<Vec<_>>().into_iter().map(|handle| {
                    handle.join().map_err(|_| Error::Other("Cover preview worker stopped".into()))
                }).collect()
            })
        }, move |app, result: cr_core::Result<Vec<(Candidate, Option<Vec<u8>>)>>, _, _| {
            let Some(session) = app.online.metadata.as_mut() else { return };
            session.status = match result {
                Ok(rows) => {
                    session.results = rows.into_iter().map(|(candidate, thumbnail)| SearchResult {
                        candidate,
                        thumbnail: thumbnail.as_deref().map(picture),
                    }).collect();
                    if session.results.is_empty() {
                        format!("No results from {source}. Try editing the query or switching catalogs.")
                    } else {
                        format!("{} results from {source} · choose one to compare", session.results.len())
                    }
                }
                Err(error) => error.to_string(),
            };
        });
    }

    pub fn preview_online_candidate(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.online.metadata.as_mut() else {
            return;
        };
        let Some(result) = session.results.get(index) else {
            return;
        };
        let mut candidate = result.candidate.clone();
        session.status = "Loading metadata and cover preview…".into();
        session.preview = None;
        self.run_job(
            "Preparing metadata preview",
            window,
            cx,
            move |job| {
                let catalog = Catalog::default();
                let description = catalog.enrich(&mut candidate);
                job.check()?;
                let cover = catalog.cover(&candidate, false);
                Ok((candidate, description, cover))
            },
            |app, result, _, _| {
                let Some(session) = app.online.metadata.as_mut() else {
                    return;
                };
                match result {
                    Ok((candidate, description, cover)) => {
                        let mut notices: Vec<String> = description
                            .err()
                            .map(|e| e.to_string())
                            .into_iter()
                            .collect();
                        let cover_bytes = match cover {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                notices.push(error.to_string());
                                None
                            }
                        };
                        session.status = if notices.is_empty() {
                            format!(
                                "Review candidate from {} · select fields to apply",
                                candidate.source
                            )
                        } else {
                            notices.join(" · ")
                        };
                        session.preview = Some(Preview {
                            candidate,
                            cover: cover_bytes.as_deref().map(picture),
                            cover_bytes,
                            fields: Vec::new(),
                        });
                    }
                    Err(error) => session.status = error.to_string(),
                }
            },
        );
    }

    pub fn advance_online_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.online.metadata.take() else {
            return;
        };
        let mut remaining = session.remaining;
        if let Some(id) = remaining.pop_front() {
            self.prepare_metadata(id, remaining, session.total, session.source, window, cx);
        }
        if self.online.metadata.is_none() {
            window.close_dialog(cx);
        }
        cx.notify();
    }

    pub fn apply_online_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.online.metadata else {
            return;
        };
        let Some(preview) = &session.preview else {
            return;
        };
        let id = session.id;
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let update = match preview.candidate.update_for_book(
            library,
            id,
            &preview.fields,
            preview.cover_bytes.clone(),
        ) {
            Ok(update) => update,
            Err(error) => {
                self.announce(error.to_string(), Tone::Error, window, cx);
                return;
            }
        };
        if self.apply_online_update(id, update, window, cx) {
            self.advance_online_metadata(window, cx);
        }
    }

    pub fn open_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Library
            || self.editing
            || self.job.is_some()
            || window.has_active_dialog(cx)
        {
            return;
        }
        let (Some(id), Some(current)) = (self.selected, self.metadata.clone()) else {
            return;
        };
        set_input(&self.online.question, String::new(), window, cx);
        self.online.ai = Some(AiSession {
            id,
            current,
            excerpt: false,
            history: Vec::new(),
            suggestion: None,
            fields: Vec::new(),
            status: String::new(),
        });
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title("Ask AI · OpenRouter")
                .w(px(780.))
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |app, cx| {
                        app.online.ai = None;
                        cx.notify();
                    });
                });
            match weak.update(cx, |app, cx| crate::ui::online::ai_dialog(app, cx)) {
                Ok(body) => dialog.child(body),
                Err(_) => dialog,
            }
        });
    }

    pub fn run_ai(&mut self, suggest: bool, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.openrouter(cx);
        let Some(session) = self.online.ai.as_mut() else {
            return;
        };
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let prompt = self.online.question.read(cx).value().trim().to_string();
        if !suggest && prompt.is_empty() {
            session.status = "Enter a question first.".into();
            cx.notify();
            return;
        }
        let excerpt = session.excerpt;
        let root = library.root().to_path_buf();
        let (id, metadata) = (session.id, session.current.clone());
        let history = session.history.clone();
        session.status = "Waiting for OpenRouter…".into();
        if suggest {
            session.suggestion = None;
            session.fields.clear();
        }
        self.run_job(
            "Asking OpenRouter",
            window,
            cx,
            move |job| {
                let excerpt = excerpt
                    .then(|| Library::open(&root)?.read_book(id, None))
                    .transpose()?;
                job.check()?;
                let context = BookContext::new(&metadata, excerpt.as_ref());
                if suggest {
                    client.suggest(&context).map(AiReply::Suggestion)
                } else {
                    let mut messages = vec![context.system_message()];
                    messages.extend(history.into_iter().map(|mut message| {
                        message.content = message.content.chars().take(4_000).collect();
                        message
                    }));
                    messages.push(Message::new(Role::User, &prompt));
                    client
                        .chat(&messages)
                        .map(|answer| AiReply::Answer(prompt, answer))
                }
            },
            |app, result, window, cx| {
                let Some(session) = app.online.ai.as_mut() else {
                    return;
                };
                session.status.clear();
                match result {
                    Ok(AiReply::Suggestion(suggestion)) => {
                        session.suggestion = Some(suggestion);
                    }
                    Ok(AiReply::Answer(prompt, answer)) => {
                        if session.history.len() >= 12 {
                            session.history.drain(..2);
                        }
                        session.history.push(Message::new(Role::User, prompt));
                        session.history.push(Message::new(Role::Assistant, answer));
                        set_input(&app.online.question, String::new(), window, cx);
                    }
                    Err(error) => session.status = error.to_string(),
                }
            },
        );
    }

    pub fn apply_ai_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.online.ai else {
            return;
        };
        let Some(suggestion) = &session.suggestion else {
            return;
        };
        let id = session.id;
        let update = BookUpdate {
            comments: session
                .fields
                .contains(&Field::Description)
                .then(|| Some(suggestion.description_html())),
            tags: session
                .fields
                .contains(&Field::Tags)
                .then(|| suggestion.tags.clone()),
            ..BookUpdate::default()
        };
        if self.apply_online_update(id, update, window, cx) {
            if let Some(session) = self.online.ai.as_mut() {
                if let Some(current) = self
                    .library
                    .as_ref()
                    .and_then(|lib| lib.get_metadata(id).ok())
                {
                    session.current = current;
                }
                session.suggestion = None;
                session.fields.clear();
                session.status = "Selected fields applied.".into();
            }
        }
    }

    fn apply_online_update(
        &mut self,
        id: i64,
        update: BookUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.job.is_some() || update == BookUpdate::default() {
            return false;
        }
        let Some(library) = self.library.as_mut() else {
            return false;
        };
        match library.update_book(id, &update) {
            Ok(()) => {
                if let Some(selected) = self.selected {
                    self.load_book(selected, window, cx);
                }
                self.refresh_after_book_change(window, cx);
                self.announce("Selected metadata applied.", Tone::Success, window, cx);
                true
            }
            Err(error) => {
                self.announce(error.to_string(), Tone::Error, window, cx);
                false
            }
        }
    }
}

enum AiReply {
    Answer(String, String),
    Suggestion(Suggestion),
}
