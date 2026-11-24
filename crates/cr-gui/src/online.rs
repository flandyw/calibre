use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use cr_core::ai::{
    reader_chapter_text, reading_minutes, recap_reader_messages_with_budget, AiClient, AiConfig,
    BookAction, BookContext, ChatReply, Message, Model, Provider, Role, Suggestion,
    DEFAULT_LOCAL_MODEL, DEFAULT_LOCAL_URL, READING_CJK_CHARS_PER_MINUTE, READING_WORDS_PER_MINUTE,
};
use cr_core::formats::reading::BlockKind;
use cr_core::jobs::JobContext;
use cr_core::online_metadata::{Candidate, Catalog, Field, Query, Source};
use cr_core::{BookUpdate, Error, Library, Metadata};
use futures::StreamExt as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::*;
use gpui_kit::*;

use crate::app::{set_input, text_input, CalibreApp, Screen, Tone};

pub struct Online {
    pub key: Entity<InputState>,
    pub local_url: Entity<InputState>,
    pub local_model: Entity<InputState>,
    pub local_key: Entity<InputState>,
    pub google_books_key: Entity<InputState>,
    pub model: Entity<InputState>,
    pub models: Vec<Model>,
    pub connection: Option<String>,
    /// Whether the last check or model listing worked; `None` while it runs.
    pub connection_ok: Option<bool>,
    pub title: Entity<InputState>,
    pub author: Entity<InputState>,
    pub isbn: Entity<InputState>,
    pub question: Entity<InputState>,
    pub chapter: Entity<InputState>,
    pub language: Entity<InputState>,
    pub preset_name: Entity<InputState>,
    pub preset_prompt: Entity<InputState>,
    pub preset_edit: Option<usize>,
    pub metadata: Option<MetadataSession>,
    pub ai: Option<AiSession>,
    pub studio: crate::studio::Studio,
}

impl Online {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        let key = text_input("OPENROUTER_API_KEY or session key", window, cx);
        key.update(cx, |input, cx| input.set_masked(true, window, cx));
        let local_key = text_input("Optional API key (leave blank for Ollama)", window, cx);
        local_key.update(cx, |input, cx| input.set_masked(true, window, cx));
        let google_books_key = text_input("GOOGLE_BOOKS_API_KEY or session key", window, cx);
        google_books_key.update(cx, |input, cx| input.set_masked(true, window, cx));
        Self {
            key,
            local_url: text_input(DEFAULT_LOCAL_URL, window, cx),
            local_model: text_input(DEFAULT_LOCAL_MODEL, window, cx),
            local_key,
            google_books_key,
            model: text_input("Model ID", window, cx),
            models: Vec::new(),
            connection: None,
            connection_ok: None,
            title: text_input("Title", window, cx),
            author: text_input("Author", window, cx),
            isbn: text_input("ISBN (optional)", window, cx),
            question: text_input("Ask a question about this book", window, cx),
            chapter: text_input("Contents #", window, cx),
            language: text_input("Language, e.g. Spanish", window, cx),
            preset_name: text_input("Action name", window, cx),
            preset_prompt: text_input("Prompt template", window, cx),
            preset_edit: None,
            metadata: None,
            ai: None,
            studio: crate::studio::Studio::new(window, cx),
        }
    }

    pub fn clear_dialogs(&mut self) -> bool {
        self.metadata.take().is_some()
            | self.ai.take().is_some()
            | self.studio.session.take().is_some()
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
    pub reader_format: Option<String>,
    pub reader_chapter_num: Option<usize>,
    pub reader_chapter_title: Option<String>,
    pub reader_chapter_text: Option<String>,
    pub reader_passage: Option<String>,
    pub excerpt: bool,
    pub history: Vec<Message>,
    pub suggestion: Option<Suggestion>,
    pub translation: Option<(String, String, bool)>,
    pub fields: Vec<Field>,
    pub status: String,
    pub tone: StatusTone,
    /// The request shown as the user's turn while the reply is on its way.
    pub pending: Option<String>,
    pub streaming: String,
    pub usage: String,
    pub local_title: String,
    pub local_result: String,
    pub last_copyable: Option<(AiCopySource, String)>,
    /// How many chat messages precede each result card, so the conversation
    /// reads in the order things happened: suggestion, translation, action.
    pub card_at: [usize; 3],
    /// When each card was last produced, to order cards at the same place.
    pub card_seq: [u32; 3],
    pub scroll: ScrollHandle,
}

/// Runs a provider call that cannot be interrupted on a thread of its own,
/// so Stop takes effect at once. An abandoned call finishes in the
/// background and what it returns, a reply or an error, is dropped. `work`
/// gets the job so it can check for Stop before it sends anything.
pub(crate) fn until_stopped<T: Send + 'static>(
    job: &JobContext,
    work: impl FnOnce(&JobContext) -> cr_core::Result<T> + Send + 'static,
) -> cr_core::Result<T> {
    job.check()?;
    let (sender, receiver) = mpsc::channel();
    let worker_job = job.clone();
    std::thread::Builder::new()
        .name("ai-request".into())
        .spawn(move || {
            let _ = sender.send(work(&worker_job));
        })?;
    loop {
        job.check()?;
        match receiver.recv_timeout(Duration::from_millis(40)) {
            Ok(result) => {
                job.check()?;
                return result;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::Other("The AI request ended unexpectedly.".into()));
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusTone {
    #[default]
    Info,
    Success,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AiCopySource {
    Chat,
    Local,
    Translation,
}

impl AiSession {
    pub fn sync_copyable_after_removal(&mut self, removed: AiCopySource) {
        if self
            .last_copyable
            .as_ref()
            .is_some_and(|(source, _)| *source == removed)
        {
            self.last_copyable = if !self.local_result.trim().is_empty() {
                Some((AiCopySource::Local, self.local_copy_text()))
            } else if let Some(last) = self
                .history
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::Assistant))
            {
                Some((AiCopySource::Chat, last.content.clone()))
            } else if let Some((_, translation, _)) = &self.translation {
                Some((AiCopySource::Translation, translation.clone()))
            } else {
                None
            };
        }
    }
}

impl AiSession {
    /// The latest action result as it is copied: its title, then the text.
    pub fn local_copy_text(&self) -> String {
        if self.local_title.is_empty() {
            self.local_result.clone()
        } else {
            format!("{}\n\n{}", self.local_title, self.local_result)
        }
    }

    /// Appends one question and its answer, dropping the oldest exchange
    /// once the conversation holds six.
    pub fn push_exchange(&mut self, question: String, answer: String) {
        if self.history.len() >= 12 {
            self.history.drain(..2);
            for at in &mut self.card_at {
                *at = at.saturating_sub(2);
            }
        }
        self.last_copyable = Some((AiCopySource::Chat, answer.clone()));
        self.history.push(Message::new(Role::User, question));
        self.history.push(Message::new(Role::Assistant, answer));
    }

    /// Puts a result card after everything shown so far.
    pub fn place_card(&mut self, slot: usize) {
        self.card_at[slot] = self.history.len();
        self.card_seq[slot] = self.card_seq.iter().max().copied().unwrap_or(0) + 1;
    }

    /// The card slots in the order their cards were produced.
    pub fn card_order(&self) -> [usize; 3] {
        let mut order = [0, 1, 2];
        order.sort_by_key(|slot| self.card_seq[*slot]);
        order
    }

    pub fn clear_chat(&mut self) {
        self.history.clear();
        self.card_at = [0; 3];
        self.sync_copyable_after_removal(AiCopySource::Chat);
    }

    pub fn set_status(&mut self, tone: StatusTone, status: impl Into<String>) {
        self.tone = tone;
        self.status = status.into();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BookTask {
    Chat,
    Suggest,
    Characters,
    Discussion,
    Translate,
    Recap,
    ReadingTime,
    ExplainPassage,
    SummarizeChapter,
}

fn picture(bytes: &[u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec()))
}

impl CalibreApp {
    pub(crate) fn ai_client(&self, cx: &App) -> AiClient {
        let key = self.online.key.read(cx).value().to_string();
        let local_key = self.online.local_key.read(cx).value().to_string();
        AiClient::new(AiConfig {
            provider: self.settings.ai_provider,
            openrouter_key: (!key.trim().is_empty()).then_some(key),
            openrouter_model: self.settings.openrouter_model.clone(),
            local_url: self.settings.ai_local_url.clone(),
            local_key: (!local_key.trim().is_empty()).then_some(local_key),
            local_model: self.settings.ai_local_model.clone(),
        })
    }

    pub fn set_ai_provider(
        &mut self,
        provider: Provider,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.online.connection = None;
        self.online.models.clear();
        self.update_preferences(|settings| settings.ai_provider = provider, cx);
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

    pub fn save_local_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.online.local_url.read(cx).value().trim().to_string();
        let url = if url.is_empty() {
            DEFAULT_LOCAL_URL.to_string()
        } else {
            url
        };
        let model = self.online.local_model.read(cx).value().trim().to_string();
        let model = if model.is_empty() {
            DEFAULT_LOCAL_MODEL.to_string()
        } else {
            model
        };
        if self.settings.ai_local_url != url {
            self.online.models.clear();
            self.online.connection = None;
        }
        set_input(&self.online.local_url, url.clone(), window, cx);
        set_input(&self.online.local_model, model.clone(), window, cx);
        self.update_preferences(
            |settings| {
                settings.ai_local_url = url;
                settings.ai_local_model = model;
            },
            cx,
        );
    }

    pub fn save_local_model(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        let model = model.trim();
        let model = if model.is_empty() {
            DEFAULT_LOCAL_MODEL
        } else {
            model
        };
        set_input(&self.online.local_model, model.into(), window, cx);
        self.update_preferences(|settings| settings.ai_local_model = model.into(), cx);
    }

    pub fn test_ai_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.ai_client(cx);
        let provider = client.provider_name();
        self.online.connection = Some(format!("Checking {provider}…"));
        self.online.connection_ok = None;
        self.run_job(
            format!("Testing {provider}"),
            window,
            cx,
            move |_| client.test_connection(),
            |app, result, _, _| {
                app.online.connection_ok = Some(result.is_ok());
                app.online.connection = Some(match result {
                    Ok(summary) => summary,
                    Err(error) => error.to_string(),
                });
            },
        );
    }

    pub fn load_ai_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.ai_client(cx);
        let provider = client.provider_name();
        self.online.connection = Some(format!("Loading models from {provider}…"));
        self.online.connection_ok = None;
        self.run_job(
            format!("Loading {provider} models"),
            window,
            cx,
            move |_| client.models(),
            |app, result, _, _| {
                app.online.connection_ok = Some(result.is_ok());
                app.online.connection = Some(match result {
                    Ok(models) => {
                        app.online.models = models;
                        format!(
                            "{} models available. Click one to use it.",
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
        let catalog = Catalog::default()
            .with_google_books_key(Some(self.online.google_books_key.read(cx).value().as_ref()));
        self.run_job("Searching book catalogs", window, cx, move |job| {
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
        if self.editing || self.job.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let client = self.ai_client(cx);
        let excerpt_limit = client.context_budget().excerpt_chars;
        let (
            id,
            current,
            reader_format,
            reader_chapter_num,
            reader_chapter_title,
            reader_chapter_text,
            reader_passage,
        ) = match self.screen {
            Screen::Library => {
                let (Some(id), Some(current)) = (self.selected, self.metadata.clone()) else {
                    return;
                };
                (id, current, None, None, None, None, None)
            }
            Screen::Reader => {
                let Some((id, format)) = &self.reader_library_format else {
                    return;
                };
                let Some(library) = &self.library else { return };
                let current = match library.get_metadata(*id) {
                    Ok(current) => current,
                    Err(error) => {
                        self.announce(error.to_string(), Tone::Error, window, cx);
                        return;
                    }
                };
                let locator = self.reader_locator();
                let (chapter_num, chapter_title, chapter_text, passage) =
                    if let Some(book) = &self.book {
                        let toc_idx = book
                            .toc
                            .iter()
                            .enumerate()
                            .rev()
                            .find(|(_, entry)| entry.target <= locator)
                            .map(|(i, _)| i);
                        let (title, text) = toc_idx
                            .and_then(|idx| reader_chapter_text(book, idx, excerpt_limit))
                            .map(|(t, body)| (Some(t), Some(body)))
                            .unwrap_or_else(|| {
                                let next_anchor_block = book
                                    .toc
                                    .iter()
                                    .find(|entry| entry.target > locator)
                                    .filter(|entry| entry.target.section == self.section_index)
                                    .map(|entry| entry.target.block);
                                let body = book
                                    .sections
                                    .get(self.section_index)
                                    .map(|section| {
                                        let limit_blocks =
                                            next_anchor_block.unwrap_or(section.blocks.len());
                                        let mut out = String::new();
                                        for block in section.blocks.iter().take(limit_blocks) {
                                            if matches!(
                                                block.kind,
                                                BlockKind::Image(_) | BlockKind::Rule
                                            ) {
                                                continue;
                                            }
                                            out.push_str(&block.text);
                                            out.push('\n');
                                            if out.len() >= excerpt_limit * 4 {
                                                break;
                                            }
                                        }
                                        out.trim().chars().take(excerpt_limit).collect::<String>()
                                    })
                                    .filter(|s| !s.is_empty());
                                (None, body)
                            });
                        let passage = self
                            .reader_block
                            .and_then(|block_idx| {
                                book.sections
                                    .get(self.section_index)
                                    .and_then(|section| section.blocks.get(block_idx))
                            })
                            .map(|block| block.text.trim().to_string())
                            .filter(|text| !text.is_empty());
                        (toc_idx.map(|i| i + 1), title, text, passage)
                    } else {
                        (None, None, None, None)
                    };
                (
                    *id,
                    current,
                    Some(format.clone()),
                    chapter_num,
                    chapter_title,
                    chapter_text,
                    passage,
                )
            }
            _ => return,
        };
        set_input(&self.online.question, String::new(), window, cx);
        set_input(
            &self.online.chapter,
            reader_chapter_num
                .map(|n| n.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(&self.online.language, String::new(), window, cx);
        self.online.ai = Some(AiSession {
            id,
            current,
            reader_format,
            reader_chapter_num,
            reader_chapter_title,
            reader_chapter_text,
            reader_passage,
            excerpt: false,
            history: Vec::new(),
            suggestion: None,
            translation: None,
            fields: Vec::new(),
            status: String::new(),
            tone: StatusTone::Info,
            pending: None,
            streaming: String::new(),
            usage: String::new(),
            local_title: String::new(),
            local_result: String::new(),
            last_copyable: None,
            card_at: [0; 3],
            card_seq: [0; 3],
            scroll: ScrollHandle::new(),
        });
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title("Ask AI")
                .w(px(760.))
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |app, cx| {
                        app.online.ai = None;
                        cx.notify();
                    });
                });
            let height = window.viewport_size().height;
            match weak.update(cx, |app, cx| crate::ui::online::ai_dialog(app, height, cx)) {
                Ok(body) => dialog.child(body),
                Err(_) => dialog,
            }
        });
    }

    pub fn open_ai_preset(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(preset) = self.settings.ai_presets.get(index).cloned() else {
            return;
        };
        self.open_ai(window, cx);
        if let Some(session) = &self.online.ai {
            set_input(
                &self.online.question,
                preset.render(&session.current),
                window,
                cx,
            );
        }
    }

    pub fn edit_ai_preset(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.online.preset_edit = index;
        let (name, prompt) = index
            .and_then(|index| self.settings.ai_presets.get(index))
            .map(|preset| (preset.name.clone(), preset.prompt.clone()))
            .unwrap_or_default();
        set_input(&self.online.preset_name, name, window, cx);
        set_input(&self.online.preset_prompt, prompt, window, cx);
        cx.notify();
    }

    pub fn save_ai_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.online.preset_name.read(cx).value().trim().to_owned();
        let prompt = self.online.preset_prompt.read(cx).value().trim().to_owned();
        let index = self.online.preset_edit;
        if name.is_empty()
            || prompt.is_empty()
            || name.chars().count() > 80
            || prompt.chars().count() > 4000
        {
            self.announce(
                "Enter a preset name (up to 80 characters) and a prompt (up to 4,000).",
                Tone::Info,
                window,
                cx,
            );
            return;
        }
        if self
            .settings
            .ai_presets
            .iter()
            .enumerate()
            .any(|(i, preset)| Some(i) != index && preset.name.eq_ignore_ascii_case(&name))
        {
            self.announce(
                "That AI preset name is already in use.",
                Tone::Info,
                window,
                cx,
            );
            return;
        }
        self.update_preferences(
            |settings| {
                let preset = cr_core::ai::PromptPreset { name, prompt };
                if let Some(item) = index.and_then(|index| settings.ai_presets.get_mut(index)) {
                    *item = preset;
                } else {
                    settings.ai_presets.push(preset);
                }
            },
            cx,
        );
        self.edit_ai_preset(None, window, cx);
    }

    pub fn delete_ai_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self
            .online
            .preset_edit
            .filter(|&i| i < self.settings.ai_presets.len())
        {
            self.update_preferences(
                |settings| {
                    settings.ai_presets.remove(index);
                },
                cx,
            );
            self.edit_ai_preset(None, window, cx);
        }
    }

    pub fn run_ai(&mut self, suggest: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.run_book_task(
            if suggest {
                BookTask::Suggest
            } else {
                BookTask::Chat
            },
            window,
            cx,
        );
    }

    pub fn run_ai_preset_in_dialog(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preset) = self.settings.ai_presets.get(index).cloned() else {
            return;
        };
        let Some(session) = &self.online.ai else {
            return;
        };
        let rendered = preset.render(&session.current);
        set_input(&self.online.question, rendered, window, cx);
        self.run_book_task(BookTask::Chat, window, cx);
    }

    pub fn copy_ai_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.online.ai else {
            return;
        };
        let Some((_, text)) = session.last_copyable.clone() else {
            return;
        };
        self.copy_ai_text(text, window, cx);
    }

    pub fn copy_ai_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(session) = self.online.ai.as_mut() {
            session.set_status(StatusTone::Success, "Copied to clipboard.");
        }
        self.expire_ai_status(window, cx);
        self.announce("Copied to clipboard.", Tone::Info, window, cx);
    }

    /// Clears the current confirmation after a few seconds, unless something
    /// else has replaced it by then.
    fn expire_ai_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self
            .online
            .ai
            .as_ref()
            .map(|session| session.status.clone())
        else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(4))
                .await;
            let _ = this.update(cx, |app, cx| {
                if let Some(session) = app.online.ai.as_mut() {
                    if session.status == shown && session.tone == StatusTone::Success {
                        session.status.clear();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Sends `question` as a chat turn, as if it had been typed and sent.
    pub fn ask_ai_question(
        &mut self,
        question: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online.ai.is_none() || self.job.is_some() {
            return;
        }
        set_input(&self.online.question, question.into(), window, cx);
        self.run_book_task(BookTask::Chat, window, cx);
    }

    pub fn apply_ai_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.online.ai else {
            return;
        };
        let Some((language, text, truncated)) = session.translation.clone() else {
            return;
        };
        if truncated {
            return;
        }
        let id = session.id;
        let suggestion = Suggestion {
            description: text,
            tags: Vec::new(),
        };
        let update = BookUpdate {
            comments: Some(Some(suggestion.description_html())),
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
                session.translation = None;
                session.sync_copyable_after_removal(AiCopySource::Translation);
                session.set_status(
                    StatusTone::Success,
                    format!("Applied {language} description."),
                );
            }
        }
    }

    pub fn run_book_task(&mut self, task: BookTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let client = self.ai_client(cx);
        let provider_name = client.provider_name();
        let budget = client.context_budget();
        let models = self.online.models.clone();
        let model = client.active_model().to_string();
        let Some(session) = self.online.ai.as_mut() else {
            return;
        };
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let prompt = self.online.question.read(cx).value().trim().to_string();
        let chapter = self
            .online
            .chapter
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .ok();
        let language = self.online.language.read(cx).value().trim().to_string();
        if task == BookTask::Chat && prompt.is_empty()
            || task == BookTask::Recap && !chapter.is_some_and(|chapter| chapter > 0)
            || task == BookTask::Translate
                && (language.is_empty() || session.current.comments.is_none())
            || task == BookTask::ExplainPassage && session.reader_passage.is_none()
            || task == BookTask::SummarizeChapter && session.reader_chapter_text.is_none()
        {
            let hint = match task {
                BookTask::Recap => {
                    "Enter the position in Contents of the last chapter you've read."
                }
                BookTask::Translate if session.current.comments.is_none() => {
                    "This book has no description to translate."
                }
                BookTask::Translate => "Enter a target language first.",
                BookTask::ExplainPassage => {
                    "Click a paragraph in the reader first, then open Ask AI."
                }
                BookTask::SummarizeChapter => "Open a book in the reader to summarize its chapter.",
                _ => "Type a question first.",
            };
            session.set_status(StatusTone::Info, hint);
            cx.notify();
            return;
        }
        let excerpt = session.excerpt
            && !matches!(
                task,
                BookTask::Recap
                    | BookTask::ReadingTime
                    | BookTask::Translate
                    | BookTask::ExplainPassage
                    | BookTask::SummarizeChapter
            );
        let root = library.root().to_path_buf();
        let (id, metadata) = (session.id, session.current.clone());
        let reader_format = session.reader_format.clone();
        let reader_chapter_title = session.reader_chapter_title.clone();
        let reader_chapter_text = session.reader_chapter_text.clone();
        let reader_passage = session.reader_passage.clone();
        let history = session.history.clone();
        session.set_status(
            StatusTone::Info,
            if task == BookTask::ReadingTime {
                "Counting words locally…".to_string()
            } else {
                format!("Waiting for {provider_name}…")
            },
        );
        session.pending = Some(match task {
            BookTask::Chat => prompt.clone(),
            BookTask::Suggest => "Suggest a description and tags".into(),
            BookTask::Characters => "Who are the main characters?".into(),
            BookTask::Discussion => "Give me discussion questions".into(),
            BookTask::Translate => format!("Translate the description into {language}"),
            BookTask::Recap => format!(
                "Recap the story through chapter {}",
                chapter.unwrap_or_default()
            ),
            BookTask::ReadingTime => "How long will this take to read?".into(),
            BookTask::ExplainPassage => "Explain the selected passage".into(),
            BookTask::SummarizeChapter => "Summarize this chapter".into(),
        });
        session.streaming.clear();
        session.usage.clear();
        session.local_title.clear();
        session.local_result.clear();
        session.scroll.scroll_to_bottom();
        if task == BookTask::Suggest {
            session.suggestion = None;
            session.fields.clear();
        }
        if task == BookTask::Translate {
            session.translation = None;
        }
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<String>();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(part) = receiver.next().await {
                let _ = this.update(cx, |app, cx| {
                    if let Some(session) = app
                        .online
                        .ai
                        .as_mut()
                        .filter(|session| session.id == id && app.job.is_some())
                    {
                        session.streaming.push_str(&part);
                        session.scroll.scroll_to_bottom();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        let translate_lang = language.clone();
        let translation_truncated = session
            .current
            .comments
            .as_deref()
            .is_some_and(|comments| comments.chars().count() > 4000.min(budget.excerpt_chars));
        self.run_job(
            if task == BookTask::ReadingTime {
                "Estimating reading time".to_string()
            } else {
                format!("Asking {provider_name}")
            },
            window,
            cx,
            move |job| {
                let needs_book = excerpt || task == BookTask::ReadingTime;
                let text = needs_book
                    .then(|| Library::open(&root)?.read_book(id, reader_format.as_deref()))
                    .transpose()?;
                job.check()?;
                let context = if matches!(
                    task,
                    BookTask::ExplainPassage | BookTask::SummarizeChapter
                ) || (task == BookTask::Chat && reader_chapter_text.is_some())
                {
                    BookContext::for_reader(
                        &metadata,
                        reader_chapter_title.as_deref(),
                        reader_chapter_text.as_deref(),
                        reader_passage.as_deref(),
                        budget.excerpt_chars,
                    )
                } else {
                    BookContext::new_with_budget(
                        &metadata,
                        text.as_ref().filter(|_| excerpt),
                        budget.excerpt_chars,
                    )
                };
                let (label, messages) = match task {
                    BookTask::Suggest => {
                        return until_stopped(job, move |_| client.suggest(&context))
                            .map(AiReply::Suggestion);
                    }
                    BookTask::ReadingTime => {
                        let minutes = text.as_ref().map_or(0, reading_minutes);
                        return Ok(AiReply::Local(format!(
                            "About {minutes} minutes (local estimate: {READING_WORDS_PER_MINUTE} words or {READING_CJK_CHARS_PER_MINUTE} CJK characters per minute)."
                        )));
                    }
                    BookTask::Recap => {
                        let chapter = chapter
                            .ok_or_else(|| Error::Other("Enter a chapter number.".into()))?;
                        let book = Library::open(&root)?.open_book(id, reader_format.as_deref())?;
                        (
                            format!("Recap through chapter {chapter}"),
                            recap_reader_messages_with_budget(
                                &metadata,
                                &book,
                                chapter,
                                budget.excerpt_chars,
                            )?,
                        )
                    }
                    _ => {
                        let (label, ask) = match task {
                            BookTask::Characters => (
                                BookAction::Characters.label(),
                                BookAction::Characters.prompt(),
                            ),
                            BookTask::Discussion => (
                                BookAction::Discussion.label(),
                                BookAction::Discussion.prompt(),
                            ),
                            BookTask::Translate => (
                                BookAction::Translate(&language).label(),
                                BookAction::Translate(&language).prompt(),
                            ),
                            BookTask::ExplainPassage => (
                                BookAction::ExplainPassage.label(),
                                BookAction::ExplainPassage.prompt(),
                            ),
                            BookTask::SummarizeChapter => (
                                BookAction::SummarizeChapter.label(),
                                BookAction::SummarizeChapter.prompt(),
                            ),
                            _ => (prompt.clone(), prompt),
                        };
                        let mut messages = vec![context.system_message()];
                        if task == BookTask::Chat {
                            let turn_cap = (budget.excerpt_chars / 2).clamp(500, 4_000);
                            let mut remaining = budget.excerpt_chars;
                            let mut bounded_pairs = Vec::new();
                            for pair in history.chunks(2).rev() {
                                let mut truncated_pair = Vec::with_capacity(pair.len());
                                let mut pair_len = 0;
                                for message in pair {
                                    let mut msg = message.clone();
                                    msg.content = msg.content.chars().take(turn_cap).collect();
                                    pair_len += msg.content.chars().count();
                                    truncated_pair.push(msg);
                                }
                                if pair_len > remaining {
                                    break;
                                }
                                remaining -= pair_len;
                                bounded_pairs.push(truncated_pair);
                            }
                            for pair in bounded_pairs.into_iter().rev() {
                                messages.extend(pair);
                            }
                        }
                        messages.push(Message::new(Role::User, ask));
                        (label, messages)
                    }
                };
                job.check()?;
                let reply = client.chat_stream(&messages, |part| {
                    job.check()?;
                    let _ = sender.unbounded_send(part.to_owned());
                    Ok(())
                })?;
                let usage = reply
                    .usage
                    .as_ref()
                    .map(|usage| {
                        let model = reply.model.as_deref().unwrap_or(&model);
                        let pricing = models
                            .iter()
                            .find(|item| item.id == model)
                            .and_then(|item| item.pricing.clone());
                        let pricing = if pricing.is_none()
                            && usage.estimated_cost(None).is_none()
                            && !client.is_on_device()
                            && job.check().is_ok()
                        {
                            client.models().ok().and_then(|models| {
                                models.into_iter().find(|item| item.id == model)?.pricing
                            })
                        } else {
                            pricing
                        };
                        let cost = usage
                            .estimated_cost(pricing.as_ref())
                            .map(|cost| format!(" · about ${cost:.6} USD"))
                            .unwrap_or_default();
                        format!(
                            "{} input + {} output tokens{cost}",
                            usage.prompt_tokens, usage.completion_tokens
                        )
                    })
                    .unwrap_or_default();
                Ok(AiReply::Answer(label, reply, usage))
            },
            move |app, result, window, cx| {
                let Some(session) = app.online.ai.as_mut() else {
                    return;
                };
                session.set_status(StatusTone::Info, "");
                let partial = std::mem::take(&mut session.streaming);
                let pending = session.pending.take();
                session.scroll.scroll_to_bottom();
                match result {
                    Ok(AiReply::Suggestion(suggestion)) => {
                        session.place_card(0);
                        session.suggestion = Some(suggestion);
                    }
                    Ok(AiReply::Answer(prompt, answer, usage)) => {
                        session.usage = usage;
                        if task == BookTask::Translate {
                            let text = answer.text.trim().to_string();
                            session.last_copyable = (!text.is_empty())
                                .then_some((AiCopySource::Translation, text.clone()));
                            session.place_card(1);
                            session.translation = Some((
                                translate_lang.clone(),
                                text,
                                translation_truncated,
                            ));
                            return;
                        }
                        if task != BookTask::Chat {
                            session.place_card(2);
                            session.local_title = prompt;
                            session.local_result = answer.text;
                            session.last_copyable =
                                Some((AiCopySource::Local, session.local_copy_text()));
                            return;
                        }
                        session.push_exchange(prompt, answer.text);
                        set_input(&app.online.question, String::new(), window, cx);
                    }
                    Ok(AiReply::Local(answer)) => {
                        session.place_card(2);
                        session.local_title = "Reading time".into();
                        session.local_result = answer;
                        session.last_copyable =
                            Some((AiCopySource::Local, session.local_copy_text()));
                    }
                    Err(Error::Cancelled) => {
                        // A stopped chat reply keeps what had arrived.
                        match pending {
                            Some(question)
                                if task == BookTask::Chat && !partial.trim().is_empty() =>
                            {
                                session.push_exchange(question, partial.trim().to_string());
                                set_input(&app.online.question, String::new(), window, cx);
                                session.set_status(
                                    StatusTone::Info,
                                    "Stopped. The partial reply was kept.",
                                );
                            }
                            _ => session.set_status(StatusTone::Info, "Stopped."),
                        }
                    }
                    Err(error) => session.set_status(StatusTone::Error, error.to_string()),
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
                session.set_status(StatusTone::Success, "Selected fields applied.");
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
    Answer(String, ChatReply, String),
    Local(String),
    Suggestion(Suggestion),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> AiSession {
        AiSession {
            id: 1,
            current: Metadata::default(),
            reader_format: None,
            reader_chapter_num: None,
            reader_chapter_title: None,
            reader_chapter_text: None,
            reader_passage: None,
            excerpt: false,
            history: Vec::new(),
            suggestion: None,
            translation: None,
            fields: Vec::new(),
            status: String::new(),
            tone: StatusTone::Info,
            pending: None,
            streaming: String::new(),
            usage: String::new(),
            local_title: String::new(),
            local_result: String::new(),
            last_copyable: None,
            card_at: [0; 3],
            card_seq: [0; 3],
            scroll: ScrollHandle::new(),
        }
    }

    #[core::prelude::v1::test]
    fn stop_does_not_wait_for_the_provider_or_show_its_error() {
        let (release, blocked) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel();
        let handle = cr_core::jobs::spawn("ask", move |job| {
            until_stopped(job, move |_| -> cr_core::Result<()> {
                started.send(()).ok();
                // Stands in for a provider that answers late, with an error.
                blocked.recv().ok();
                Err(Error::Other("provider failed".into()))
            })
        })
        .unwrap();
        running.recv().unwrap();
        handle.cancel();
        // The provider is still blocked, so this only returns if Stop won.
        assert!(matches!(handle.join(), Err(Error::Cancelled)));
        drop(release);
    }

    #[core::prelude::v1::test]
    fn work_abandoned_on_stop_can_see_it_was_stopped() {
        let (release, blocked) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel();
        let (seen, outcome) = mpsc::channel();
        let handle = cr_core::jobs::spawn("ask", move |job| {
            until_stopped(job, move |job| -> cr_core::Result<()> {
                started.send(()).ok();
                // Stands in for loading the library before the request.
                blocked.recv().ok();
                seen.send(job.is_cancelled()).ok();
                job.check()
            })
        })
        .unwrap();
        running.recv().unwrap();
        handle.cancel();
        assert!(matches!(handle.join(), Err(Error::Cancelled)));
        release.send(()).unwrap();
        assert!(outcome.recv().unwrap());
    }

    #[core::prelude::v1::test]
    fn a_finished_provider_call_passes_its_result_through() {
        let job = JobContext::detached();
        assert_eq!(until_stopped(&job, |_| Ok(7)).unwrap(), 7);
        let failed = until_stopped(&job, |_| -> cr_core::Result<()> {
            Err(Error::Other("provider failed".into()))
        });
        assert!(matches!(failed, Err(Error::Other(message)) if message == "provider failed"));
    }

    #[core::prelude::v1::test]
    fn dropping_the_oldest_exchange_keeps_cards_in_place() {
        let mut session = session();
        for turn in 0..6 {
            session.push_exchange(format!("q{turn}"), format!("a{turn}"));
        }
        // A card produced after the second exchange, one before any chat.
        session.card_at = [4, 0, 12];
        session.push_exchange("q6".into(), "a6".into());
        assert_eq!(session.history.len(), 12);
        assert_eq!(session.history[0].content, "q1");
        assert_eq!(session.card_at, [2, 0, 10]);
        assert!(matches!(
            &session.last_copyable,
            Some((AiCopySource::Chat, text)) if text == "a6"
        ));
    }

    #[core::prelude::v1::test]
    fn cards_at_the_same_place_keep_the_order_they_were_produced() {
        let mut session = session();
        // A translation, then a suggestion, with no chat in between.
        session.place_card(1);
        session.place_card(0);
        assert_eq!(session.card_at, [0; 3]);
        assert_eq!(session.card_order(), [2, 1, 0]);
        // Asking for the translation again moves it below the suggestion.
        session.place_card(1);
        assert_eq!(session.card_order(), [2, 0, 1]);
    }

    #[core::prelude::v1::test]
    fn clearing_the_chat_falls_back_to_the_latest_action_result() {
        let mut session = session();
        session.local_title = "Character guide".into();
        session.local_result = "Ilse, the mapmaker's daughter.".into();
        session.card_at[2] = 2;
        session.push_exchange("Who is Ilse?".into(), "The narrator.".into());
        session.clear_chat();
        assert!(session.history.is_empty());
        assert_eq!(session.card_at, [0; 3]);
        assert!(matches!(
            &session.last_copyable,
            Some((AiCopySource::Local, text))
                if text == "Character guide\n\nIlse, the mapmaker's daughter."
        ));
    }
}
