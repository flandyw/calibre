use std::collections::VecDeque;
use std::sync::Arc;

use cr_core::ai::{
    reader_chapter_text, reading_minutes, recap_reader_messages_with_budget, AiClient, AiConfig,
    BookAction, BookContext, ChatReply, Message, Model, Provider, Role, Suggestion,
    DEFAULT_LOCAL_MODEL, DEFAULT_LOCAL_URL, READING_CJK_CHARS_PER_MINUTE,
    READING_WORDS_PER_MINUTE,
};
use cr_core::formats::reading::BlockKind;
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
            title: text_input("Title", window, cx),
            author: text_input("Author", window, cx),
            isbn: text_input("ISBN (optional)", window, cx),
            question: text_input("Ask a question about this book", window, cx),
            chapter: text_input("Contents #", window, cx),
            language: text_input("Target language (e.g. Spanish)", window, cx),
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
    pub translation: Option<(String, String)>,
    pub fields: Vec<Field>,
    pub status: String,
    pub streaming: String,
    pub usage: String,
    pub local_result: String,
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
        self.run_job(
            format!("Testing {provider}"),
            window,
            cx,
            move |_| client.test_connection(),
            |app, result, _, _| {
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
        self.run_job(
            format!("Loading {provider} models"),
            window,
            cx,
            move |_| client.models(),
            |app, result, _, _| {
                app.online.connection = Some(match result {
                    Ok(models) => {
                        app.online.models = models;
                        format!(
                            "{} models available. Choose a model below.",
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
                                let body = book
                                    .sections
                                    .get(self.section_index)
                                    .map(|section| {
                                        let mut out = String::new();
                                        for block in &section.blocks {
                                            if matches!(block.kind, BlockKind::Image(_) | BlockKind::Rule) {
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
            streaming: String::new(),
            usage: String::new(),
            local_result: String::new(),
        });
        let title = format!("Ask AI · {}", client.provider_name());
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let closing = weak.clone();
            let dialog = dialog
                .title(title.clone())
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
        let text = if let Some((_, translation)) = &session.translation {
            translation.clone()
        } else if !session.local_result.trim().is_empty() {
            session.local_result.clone()
        } else if let Some(last) = session
            .history
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::Assistant))
        {
            last.content.clone()
        } else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(session) = self.online.ai.as_mut() {
            session.status = "Copied response to clipboard.".into();
        }
        self.announce("Copied response to clipboard.", Tone::Info, window, cx);
    }

    pub fn apply_ai_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.online.ai else {
            return;
        };
        let Some((language, text)) = session.translation.clone() else {
            return;
        };
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
                session.status = format!("Applied {language} description.");
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
            session.status = match task {
                BookTask::Recap => {
                    "Enter the position in Contents of the last chapter you've read."
                }
                BookTask::Translate => "Enter a target language for a book with a description.",
                BookTask::ExplainPassage => {
                    "Click a paragraph in the reader first, then open Ask AI."
                }
                BookTask::SummarizeChapter => "Open a book in the reader to summarize its chapter.",
                _ => "Enter a question first.",
            }
            .into();
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
        session.status = if task == BookTask::ReadingTime {
            "Counting words locally…".into()
        } else {
            format!("Waiting for {provider_name}…")
        };
        session.streaming.clear();
        session.usage.clear();
        session.local_result.clear();
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
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        let translate_lang = language.clone();
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
                    BookTask::Suggest => return client.suggest(&context).map(AiReply::Suggestion),
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
                            messages.extend(history.into_iter().map(|mut message| {
                                message.content = message.content.chars().take(4_000).collect();
                                message
                            }));
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
                session.status.clear();
                session.streaming.clear();
                match result {
                    Ok(AiReply::Suggestion(suggestion)) => {
                        session.suggestion = Some(suggestion);
                    }
                    Ok(AiReply::Answer(prompt, answer, usage)) => {
                        session.usage = usage;
                        if task == BookTask::Translate {
                            session.translation =
                                Some((translate_lang.clone(), answer.text.trim().to_string()));
                            return;
                        }
                        if task != BookTask::Chat {
                            session.local_result = format!("{prompt}\n\n{}", answer.text);
                            return;
                        }
                        if session.history.len() >= 12 {
                            session.history.drain(..2);
                        }
                        session.history.push(Message::new(Role::User, prompt));
                        session
                            .history
                            .push(Message::new(Role::Assistant, answer.text));
                        set_input(&app.online.question, String::new(), window, cx);
                    }
                    Ok(AiReply::Local(answer)) => session.local_result = answer,
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
    Answer(String, ChatReply, String),
    Local(String),
    Suggestion(Suggestion),
}
