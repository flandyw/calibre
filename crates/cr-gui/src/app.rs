//! The calibre-rust desktop interface, built with GPUI and gpui-kit.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use cr_core::device::{self, Device, SendOptions};
use cr_core::formats::{epub as epub_formats, html};
use cr_core::library::{BookUpdate, Library, ListOptions};
use cr_core::metadata::{htmlize_comments, parse_user_date, BookSummary, Metadata};
use cr_core::text::{self, split_authors};
use cr_core::Chapter;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::*;
use gpui_kit::*;
use image::DynamicImage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Reader,
    Epub,
}

#[derive(Clone)]
struct EpubFile {
    relative: String,
    path: PathBuf,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CropDragMode {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy)]
struct CropDrag {
    mode: CropDragMode,
    start_x: f32,
    start_y: f32,
    display_width: f32,
    display_height: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

const EXPORT_TEMPLATE: &str = "{author_sort}/{title} ({id})/{title} - {author}";

pub struct CalibreApp {
    library: Option<Library>,
    devices: Vec<Device>,
    books: Vec<BookSummary>,
    selected: Option<i64>,
    metadata: Option<Metadata>,
    cover_path: Option<PathBuf>,
    selected_formats: Vec<String>,
    selected_path: Option<String>,
    chapters: Vec<Chapter>,
    chapter_index: usize,
    reader_title: String,
    screen: Screen,
    editing: bool,
    confirm_remove: bool,
    dark: bool,
    status: String,
    query: String,

    // Cover crop state.
    cropping: bool,
    crop_book_id: Option<i64>,
    crop_source: Option<DynamicImage>,
    crop_preview_path: Option<PathBuf>,
    crop_preview_counter: u64,
    crop_drag: Option<CropDrag>,

    // EPUB editor state.
    epub_book_id: Option<i64>,
    epub_dir: Option<PathBuf>,
    epub_files: Vec<EpubFile>,
    epub_selected: Option<usize>,
    epub_dirty: bool,

    library_path: Entity<InputState>,
    search: Entity<InputState>,
    title_input: Entity<InputState>,
    authors_input: Entity<InputState>,
    series_input: Entity<InputState>,
    series_index_input: Entity<InputState>,
    publisher_input: Entity<InputState>,
    tags_input: Entity<InputState>,
    rating_input: Entity<InputState>,
    languages_input: Entity<InputState>,
    identifiers_input: Entity<InputState>,
    pubdate_input: Entity<InputState>,
    comments_input: Entity<TextareaState>,
    epub_editor: Entity<TextareaState>,
    crop_left: Entity<InputState>,
    crop_top: Entity<InputState>,
    crop_width: Entity<InputState>,
    crop_height: Entity<InputState>,

    _subscriptions: Vec<Subscription>,
}

impl CalibreApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library_path = text_input("Path to calibre library", window, cx);
        let search = text_input("Search title, author, tag or series", window, cx);
        let title_input = text_input("Title", window, cx);
        let authors_input = text_input("Authors, separated by &", window, cx);
        let series_input = text_input("Series", window, cx);
        let series_index_input = text_input("Series index", window, cx);
        let publisher_input = text_input("Publisher", window, cx);
        let tags_input = text_input("Tags, comma separated", window, cx);
        let rating_input = text_input("Rating 0-10", window, cx);
        let languages_input = text_input("Languages, comma separated", window, cx);
        let identifiers_input = text_input("isbn=..., asin=...", window, cx);
        let pubdate_input = text_input("Publication date (YYYY-MM-DD)", window, cx);
        let comments_input = text_area("Comments / description", 6, window, cx);
        let epub_editor = text_area("EPUB source", 20, window, cx);
        let crop_left = text_input("Left %", window, cx);
        let crop_top = text_input("Top %", window, cx);
        let crop_width = text_input("Width %", window, cx);
        let crop_height = text_input("Height %", window, cx);

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe_in(&search, window, |this, state, event, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    this.reload(cx);
                    cx.notify();
                }
            }),
        );
        subscriptions.push(cx.subscribe_in(
            &library_path,
            window,
            |this, _state, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let path = PathBuf::from(this.library_path.read(cx).value().to_string());
                    this.open_library(path, window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &epub_editor,
            window,
            |this, _state, event, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.epub_dirty = true;
                    cx.notify();
                }
            },
        ));

        let mut app = Self {
            library: None,
            devices: Vec::new(),
            books: Vec::new(),
            selected: None,
            metadata: None,
            cover_path: None,
            selected_formats: Vec::new(),
            selected_path: None,
            chapters: Vec::new(),
            chapter_index: 0,
            reader_title: String::new(),
            screen: Screen::Library,
            editing: false,
            confirm_remove: false,
            dark: false,
            status: "Open a library or create a new one.".to_string(),
            query: String::new(),
            cropping: false,
            crop_book_id: None,
            crop_source: None,
            crop_preview_path: None,
            crop_preview_counter: 0,
            crop_drag: None,
            epub_book_id: None,
            epub_dir: None,
            epub_files: Vec::new(),
            epub_selected: None,
            epub_dirty: false,
            library_path,
            search,
            title_input,
            authors_input,
            series_input,
            series_index_input,
            publisher_input,
            tags_input,
            rating_input,
            languages_input,
            identifiers_input,
            pubdate_input,
            comments_input,
            epub_editor,
            crop_left,
            crop_top,
            crop_width,
            crop_height,
            _subscriptions: subscriptions,
        };

        let default_path = default_library_path();
        set_input(
            &app.library_path,
            default_path.to_string_lossy().to_string(),
            window,
            cx,
        );
        app.devices = device::discover();

        if default_path.join("metadata.db").is_file() {
            if let Ok(library) = Library::open(&default_path) {
                app.library = Some(library);
                app.reload(cx);
                if let Some(id) = app.books.first().map(|book| book.id) {
                    app.load_book(id, window, cx);
                }
                app.status = format!(
                    "Opened {} ({} books, {} device(s))",
                    default_path.display(),
                    app.books.len(),
                    app.devices.len()
                );
            }
        }
        app
    }

    fn reload(&mut self, _cx: &mut Context<Self>) {
        let query = self.query.trim().to_string();
        let options = ListOptions {
            query: if query.is_empty() { None } else { Some(query) },
            ..Default::default()
        };
        let result = self
            .library
            .as_ref()
            .map(|library| library.list_books(&options));
        match result {
            Some(Ok(books)) => self.books = books,
            Some(Err(err)) => {
                self.books.clear();
                self.status = format!("Search failed: {err}");
            }
            None => self.books.clear(),
        }
    }

    fn load_book(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_crop();
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let metadata = match library.get_metadata(id) {
            Ok(metadata) => metadata,
            Err(err) => {
                self.status = format!("Could not load book #{id}: {err}");
                return;
            }
        };
        let formats = library.formats(id).unwrap_or_default();
        let relative = library.book_path(id).ok();
        let cover_path = relative
            .as_ref()
            .map(|relative| library.root().join(relative).join("cover.jpg"))
            .filter(|path| path.is_file());

        self.fill_form(&metadata, window, cx);
        self.metadata = Some(metadata);
        self.selected = Some(id);
        self.selected_formats = formats;
        self.selected_path = relative;
        self.cover_path = cover_path;
        self.editing = false;
        self.confirm_remove = false;
        self.screen = Screen::Library;
    }

    fn select_book(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.load_book(id, window, cx);
        cx.notify();
    }

    fn fill_form(&self, metadata: &Metadata, window: &mut Window, cx: &mut Context<Self>) {
        set_input(&self.title_input, metadata.title.clone(), window, cx);
        set_input(
            &self.authors_input,
            metadata.authors.join(" & "),
            window,
            cx,
        );
        set_input(
            &self.series_input,
            metadata.series.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.series_index_input,
            text::format_series_index(metadata.series_index),
            window,
            cx,
        );
        set_input(
            &self.publisher_input,
            metadata.publisher.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_input(&self.tags_input, metadata.tags.join(", "), window, cx);
        set_input(
            &self.rating_input,
            metadata
                .rating
                .map(|rating| rating.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.languages_input,
            metadata.languages.join(", "),
            window,
            cx,
        );
        set_input(
            &self.identifiers_input,
            metadata
                .identifiers
                .iter()
                .map(|(kind, value)| format!("{kind}={value}"))
                .collect::<Vec<_>>()
                .join(", "),
            window,
            cx,
        );
        set_input(
            &self.pubdate_input,
            metadata
                .pubdate
                .as_ref()
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_textarea(
            &self.comments_input,
            metadata.comments.clone().unwrap_or_default(),
            window,
            cx,
        );
    }

    fn install_library(
        &mut self,
        library: Library,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discard_epub_editor();
        self.library = Some(library);
        self.query.clear();
        set_input(&self.search, String::new(), window, cx);
        set_input(
            &self.library_path,
            path.to_string_lossy().to_string(),
            window,
            cx,
        );
        self.devices = device::discover();
        self.selected = None;
        self.metadata = None;
        self.cover_path = None;
        self.selected_formats.clear();
        self.selected_path = None;
        self.reload(cx);
        if let Some(id) = self.books.first().map(|book| book.id) {
            self.load_book(id, window, cx);
        }
        self.status = format!(
            "Opened {} ({} books, {} device(s))",
            path.display(),
            self.books.len(),
            self.devices.len()
        );
        cx.notify();
    }

    fn open_library(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match Library::open(&path) {
            Ok(library) => self.install_library(library, &path, window, cx),
            Err(err) => {
                self.status = format!(
                    "Could not open {}: {err}. Use New to create it.",
                    path.display()
                );
                cx.notify();
            }
        }
    }

    fn create_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.library_path.read(cx).value().to_string();
        if raw.trim().is_empty() {
            self.status = "Enter a folder for the new library.".to_string();
            cx.notify();
            return;
        }
        let path = PathBuf::from(raw.trim());
        match Library::create(&path) {
            Ok(library) => self.install_library(library, &path, window, cx),
            Err(err) => {
                self.status = format!("Could not create {}: {err}", path.display());
                cx.notify();
            }
        }
    }

    fn browse_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a calibre library folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| {
                        this.open_library(path, window, cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn browse_add_books(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.is_none() {
            self.status = "Open a library before adding books.".to_string();
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add ebooks to the library".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    this.add_paths(paths, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let mut added = 0usize;
        let mut last_id = None;
        let mut last_error = None;
        if let Some(library) = self.library.as_mut() {
            for path in paths {
                match library.add_book(&path) {
                    Ok(book) => {
                        added += 1;
                        last_id = Some(book.id);
                    }
                    Err(err) => {
                        last_error = Some(format!("{}: {err}", path.display()));
                    }
                }
            }
        }
        self.query.clear();
        set_input(&self.search, String::new(), window, cx);
        self.reload(cx);
        if let Some(id) = last_id {
            self.load_book(id, window, cx);
        }
        self.status = match last_error {
            Some(err) => format!("Added {added} book(s). Last error: {err}"),
            None => format!("Added {added} book(s)."),
        };
        cx.notify();
    }

    fn browse_cover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a cover image".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| {
                        let data = match std::fs::read(&path) {
                            Ok(data) => data,
                            Err(err) => {
                                this.status = format!("Could not read cover: {err}");
                                cx.notify();
                                return;
                            }
                        };
                        let result = this
                            .library
                            .as_mut()
                            .map(|library| library.set_cover(id, Some(&data)));
                        match result {
                            Some(Ok(())) => {
                                this.load_book(id, window, cx);
                                this.status = format!("Cover updated from {}", path.display());
                            }
                            Some(Err(err)) => {
                                this.status = format!("Cover update failed: {err}");
                            }
                            None => {
                                this.status = "Open a library first.".to_string();
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn browse_export_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.is_none() {
            self.status = "Open a library before exporting.".to_string();
            cx.notify();
            return;
        }
        let ids: Vec<i64> = self.books.iter().map(|book| book.id).collect();
        if ids.is_empty() {
            self.status = "There are no books in the current list to export.".to_string();
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder for the exported books".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(destination) = paths.into_iter().next() {
                    this.update_in(cx, |this, _, cx| {
                        this.export_books(ids, destination, cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn browse_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose an export folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(destination) = paths.into_iter().next() {
                    this.update_in(cx, |this, _, cx| {
                        this.export_selected(id, destination, cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn export_selected(&mut self, id: i64, destination: PathBuf, cx: &mut Context<Self>) {
        self.export_books(vec![id], destination, cx);
    }

    fn export_books(&mut self, ids: Vec<i64>, destination: PathBuf, cx: &mut Context<Self>) {
        if ids.is_empty() {
            self.status = "There are no books to export.".to_string();
            cx.notify();
            return;
        }
        let book_count = ids.len();
        let result = self
            .library
            .as_ref()
            .map(|library| library.export_books(&ids, &destination, EXPORT_TEMPLATE, None));
        match result {
            Some(Ok(paths)) if paths.is_empty() => {
                self.status = "Nothing to export: these books have no format files.".to_string();
            }
            Some(Ok(paths)) => {
                self.status = format!(
                    "Exported {} file(s) from {book_count} book(s) to {}.",
                    paths.len(),
                    destination.display()
                );
            }
            Some(Err(err)) => self.status = format!("Export failed: {err}"),
            None => self.status = "Open a library first.".to_string(),
        }
        cx.notify();
    }

    fn begin_cover_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let Some(path) = self.cover_path.clone() else {
            self.status = "This book has no cover to crop. Choose a cover first.".to_string();
            cx.notify();
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.status = format!("Could not read cover: {err}");
                cx.notify();
                return;
            }
        };
        let source = match image::load_from_memory(&bytes) {
            Ok(source) => source,
            Err(err) => {
                self.status = format!("Unsupported cover image: {err}");
                cx.notify();
                return;
            }
        };
        set_input(&self.crop_left, "0".to_string(), window, cx);
        set_input(&self.crop_top, "0".to_string(), window, cx);
        set_input(&self.crop_width, "100".to_string(), window, cx);
        set_input(&self.crop_height, "100".to_string(), window, cx);
        self.crop_source = Some(source);
        self.crop_book_id = Some(id);
        self.cropping = true;
        self.editing = false;
        if let Err(err) = self.update_crop_preview(cx) {
            self.status = err;
        } else {
            self.status =
                "Adjust the crop or choose a preset, then save the cropped cover.".to_string();
        }
        cx.notify();
    }

    fn update_crop_preview(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let data = {
            let Some(source) = self.crop_source.as_ref() else {
                return Err("No cover is loaded for cropping.".to_string());
            };
            let (left, top, width, height) = self.crop_percentages(cx)?;
            let (x, y, w, h) =
                crop_pixels(source.width(), source.height(), left, top, width, height);
            let cropped = source.crop_imm(x, y, w, h);
            let mut data = Vec::new();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 90);
            encoder
                .encode_image(&cropped.to_rgb8())
                .map_err(|err| format!("Could not encode crop preview: {err}"))?;
            data
        };
        if let Some(old) = self.crop_preview_path.take() {
            let _ = std::fs::remove_file(old);
        }
        self.crop_preview_counter += 1;
        let path = std::env::temp_dir().join(format!(
            "calibre-rust-cover-{}-{}.jpg",
            std::process::id(),
            self.crop_preview_counter
        ));
        std::fs::write(&path, &data)
            .map_err(|err| format!("Could not write crop preview: {err}"))?;
        self.crop_preview_path = Some(path);
        Ok(())
    }

    fn crop_percentages(&self, cx: &gpui_kit::App) -> Result<(f32, f32, f32, f32), String> {
        let value = |input: &Entity<InputState>, name: &str| -> Result<f32, String> {
            let raw = input.read(cx).value().trim().to_string();
            raw.parse::<f32>()
                .map_err(|_| format!("{name} must be a number from 0 to 100."))
        };
        let mut left = value(&self.crop_left, "Left")?;
        let mut top = value(&self.crop_top, "Top")?;
        let mut width = value(&self.crop_width, "Width")?;
        let mut height = value(&self.crop_height, "Height")?;
        if !(left.is_finite() && top.is_finite() && width.is_finite() && height.is_finite()) {
            return Err("Crop values must be finite numbers.".to_string());
        }
        left = left.clamp(0.0, 99.0);
        top = top.clamp(0.0, 99.0);
        width = width.clamp(1.0, 100.0 - left);
        height = height.clamp(1.0, 100.0 - top);
        Ok((left, top, width, height))
    }

    fn crop_view_dims(&self, cx: &gpui_kit::App) -> (f32, f32) {
        let Some(source) = self.crop_source.as_ref() else {
            return (220., 330.);
        };
        let (_, _, width, height) = self
            .crop_percentages(cx)
            .unwrap_or((0.0, 0.0, 100.0, 100.0));
        fit_dims(
            source.width() as f32 * width / 100.0,
            source.height() as f32 * height / 100.0,
            240.,
            380.,
        )
    }

    fn start_crop_drag(
        &mut self,
        position: Point<Pixels>,
        bounds: (f32, f32, f32, f32),
        cx: &mut Context<Self>,
    ) {
        let (origin_x, origin_y, display_width, display_height) = bounds;
        if display_width < 2.0 || display_height < 2.0 {
            return;
        }
        let (left, top, width, height) = match self.crop_percentages(cx) {
            Ok(values) => values,
            Err(err) => {
                self.status = err;
                cx.notify();
                return;
            }
        };
        let local_x = ((position.x.as_f32() - origin_x) / display_width * 100.0).clamp(0.0, 100.0);
        let local_y = ((position.y.as_f32() - origin_y) / display_height * 100.0).clamp(0.0, 100.0);
        let edge_x = (10.0 / display_width * 100.0).max(1.0);
        let edge_y = (10.0 / display_height * 100.0).max(1.0);
        let near_left = (local_x - left).abs() <= edge_x;
        let near_right = (local_x - (left + width)).abs() <= edge_x;
        let near_top = (local_y - top).abs() <= edge_y;
        let near_bottom = (local_y - (top + height)).abs() <= edge_y;
        let inside_x = local_x > left && local_x < left + width;
        let inside_y = local_y > top && local_y < top + height;

        let mode = if near_left && near_top {
            CropDragMode::TopLeft
        } else if near_right && near_top {
            CropDragMode::TopRight
        } else if near_left && near_bottom {
            CropDragMode::BottomLeft
        } else if near_right && near_bottom {
            CropDragMode::BottomRight
        } else if near_left && inside_y {
            CropDragMode::Left
        } else if near_right && inside_y {
            CropDragMode::Right
        } else if near_top && inside_x {
            CropDragMode::Top
        } else if near_bottom && inside_x {
            CropDragMode::Bottom
        } else if inside_x && inside_y {
            CropDragMode::Move
        } else {
            return;
        };

        self.crop_drag = Some(CropDrag {
            mode,
            start_x: position.x.as_f32(),
            start_y: position.y.as_f32(),
            display_width,
            display_height,
            left,
            top,
            width,
            height,
        });
    }

    fn drag_crop(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.crop_drag else {
            return;
        };
        let delta_x = (position.x.as_f32() - drag.start_x) / drag.display_width * 100.0;
        let delta_y = (position.y.as_f32() - drag.start_y) / drag.display_height * 100.0;
        let (mut left, mut top, mut width, mut height) =
            (drag.left, drag.top, drag.width, drag.height);

        match drag.mode {
            CropDragMode::Move => {
                left = (drag.left + delta_x).clamp(0.0, 100.0 - drag.width);
                top = (drag.top + delta_y).clamp(0.0, 100.0 - drag.height);
            }
            CropDragMode::Left => {
                let right = drag.left + drag.width;
                left = (drag.left + delta_x).clamp(0.0, right - 1.0);
                width = right - left;
            }
            CropDragMode::Right => {
                width = (drag.width + delta_x).clamp(1.0, 100.0 - drag.left);
            }
            CropDragMode::Top => {
                let bottom = drag.top + drag.height;
                top = (drag.top + delta_y).clamp(0.0, bottom - 1.0);
                height = bottom - top;
            }
            CropDragMode::Bottom => {
                height = (drag.height + delta_y).clamp(1.0, 100.0 - drag.top);
            }
            CropDragMode::TopLeft => {
                let right = drag.left + drag.width;
                let bottom = drag.top + drag.height;
                left = (drag.left + delta_x).clamp(0.0, right - 1.0);
                top = (drag.top + delta_y).clamp(0.0, bottom - 1.0);
                width = right - left;
                height = bottom - top;
            }
            CropDragMode::TopRight => {
                let bottom = drag.top + drag.height;
                top = (drag.top + delta_y).clamp(0.0, bottom - 1.0);
                width = (drag.width + delta_x).clamp(1.0, 100.0 - drag.left);
                height = bottom - top;
            }
            CropDragMode::BottomLeft => {
                let right = drag.left + drag.width;
                left = (drag.left + delta_x).clamp(0.0, right - 1.0);
                width = right - left;
                height = (drag.height + delta_y).clamp(1.0, 100.0 - drag.top);
            }
            CropDragMode::BottomRight => {
                width = (drag.width + delta_x).clamp(1.0, 100.0 - drag.left);
                height = (drag.height + delta_y).clamp(1.0, 100.0 - drag.top);
            }
        }

        set_input(&self.crop_left, format!("{left:.2}"), window, cx);
        set_input(&self.crop_top, format!("{top:.2}"), window, cx);
        set_input(&self.crop_width, format!("{width:.2}"), window, cx);
        set_input(&self.crop_height, format!("{height:.2}"), window, cx);
        cx.notify();
    }

    fn end_crop_drag(&mut self, cx: &mut Context<Self>) {
        if self.crop_drag.take().is_none() {
            return;
        }
        match self.update_crop_preview(cx) {
            Ok(()) => self.status = "Crop preview updated. Save to apply it.".to_string(),
            Err(err) => self.status = err,
        }
        cx.notify();
    }

    fn apply_crop_preset(
        &mut self,
        target_aspect: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preset = {
            let Some(source) = self.crop_source.as_ref() else {
                return;
            };
            let source_aspect = source.width() as f32 / source.height().max(1) as f32;
            match target_aspect {
                Some(target) if source_aspect > target => {
                    let width = 100.0 * target / source_aspect;
                    ((100.0 - width) / 2.0, 0.0, width, 100.0)
                }
                Some(target) => {
                    let height = 100.0 * source_aspect / target;
                    (0.0, (100.0 - height) / 2.0, 100.0, height)
                }
                None => (0.0, 0.0, 100.0, 100.0),
            }
        };
        set_input(&self.crop_left, format!("{:.2}", preset.0), window, cx);
        set_input(&self.crop_top, format!("{:.2}", preset.1), window, cx);
        set_input(&self.crop_width, format!("{:.2}", preset.2), window, cx);
        set_input(&self.crop_height, format!("{:.2}", preset.3), window, cx);
        match self.update_crop_preview(cx) {
            Ok(()) => self.status = "Crop preset applied.".to_string(),
            Err(err) => self.status = err,
        }
        cx.notify();
    }

    fn apply_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.crop_book_id.or(self.selected) else {
            return;
        };
        let data = {
            let Some(source) = self.crop_source.as_ref() else {
                self.status = "No cover is loaded for cropping.".to_string();
                cx.notify();
                return;
            };
            let (left, top, width, height) = match self.crop_percentages(cx) {
                Ok(values) => values,
                Err(err) => {
                    self.status = err;
                    cx.notify();
                    return;
                }
            };
            let (x, y, w, h) =
                crop_pixels(source.width(), source.height(), left, top, width, height);
            let cropped = source.crop_imm(x, y, w, h);
            let mut data = Vec::new();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 92);
            if let Err(err) = encoder.encode_image(&cropped.to_rgb8()) {
                self.status = format!("Could not encode cropped cover: {err}");
                cx.notify();
                return;
            }
            data
        };
        let result = self
            .library
            .as_mut()
            .map(|library| library.set_cover(id, Some(&data)));
        match result {
            Some(Ok(())) => {
                self.clear_crop();
                self.load_book(id, window, cx);
                self.reload(cx);
                self.status = "Cover cropped and saved.".to_string();
            }
            Some(Err(err)) => self.status = format!("Cover crop failed: {err}"),
            None => self.status = "Open a library first.".to_string(),
        }
        cx.notify();
    }

    fn cancel_crop(&mut self, cx: &mut Context<Self>) {
        self.clear_crop();
        self.status = "Cover crop cancelled.".to_string();
        cx.notify();
    }

    fn clear_crop(&mut self) {
        if let Some(path) = self.crop_preview_path.take() {
            let _ = std::fs::remove_file(path);
        }
        self.cropping = false;
        self.crop_book_id = None;
        self.crop_source = None;
        self.crop_drag = None;
    }

    fn begin_epub_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if !self
            .selected_formats
            .iter()
            .any(|format| format.eq_ignore_ascii_case("EPUB"))
        {
            self.status = "This book has no EPUB file to edit.".to_string();
            cx.notify();
            return;
        }
        let dir = std::env::temp_dir().join(format!("calibre-rust-edit-{id}-{}", temp_stamp()));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.status = format!("Could not create editor folder: {err}");
            cx.notify();
            return;
        }
        let result = self
            .library
            .as_ref()
            .map(|library| library.unpack_book(id, "EPUB", &dir));
        match result {
            Some(Ok(_)) => {}
            Some(Err(err)) => {
                let _ = std::fs::remove_dir_all(&dir);
                self.status = format!("Could not unpack EPUB: {err}");
                cx.notify();
                return;
            }
            None => {
                let _ = std::fs::remove_dir_all(&dir);
                self.status = "Open a library first.".to_string();
                cx.notify();
                return;
            }
        }
        let files = scan_epub_files(&dir);
        if files.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            self.status = "This EPUB has no editable text files.".to_string();
            cx.notify();
            return;
        }
        self.epub_book_id = Some(id);
        self.epub_dir = Some(dir);
        self.epub_files = files;
        self.epub_selected = None;
        self.epub_dirty = false;
        self.screen = Screen::Epub;
        self.load_epub_file(0, window, cx);
        self.status = format!(
            "Editing EPUB for book #{id} ({} editable file(s)).",
            self.epub_files.len()
        );
        cx.notify();
    }

    fn load_epub_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.epub_selected == Some(index) {
            return;
        }
        if let Err(err) = self.save_current_epub_file(cx) {
            self.status = err;
            cx.notify();
            return;
        }
        let Some(file) = self.epub_files.get(index).cloned() else {
            return;
        };
        match std::fs::read_to_string(&file.path) {
            Ok(text) => {
                set_textarea(&self.epub_editor, text, window, cx);
                self.epub_selected = Some(index);
                self.epub_dirty = false;
                self.status = format!("Editing {}.", file.relative);
            }
            Err(err) => {
                self.status = format!("Could not read {}: {err}", file.relative);
            }
        }
        cx.notify();
    }

    fn save_current_epub_file(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.epub_dirty {
            return Ok(());
        }
        let Some(index) = self.epub_selected else {
            return Ok(());
        };
        let Some(file) = self.epub_files.get(index).cloned() else {
            return Ok(());
        };
        let text = self.epub_editor.read(cx).value().to_string();
        std::fs::write(&file.path, text)
            .map_err(|err| format!("Could not save {}: {err}", file.relative))?;
        self.epub_dirty = false;
        Ok(())
    }

    fn save_epub_action(&mut self, cx: &mut Context<Self>) {
        let name = self
            .epub_selected
            .and_then(|index| self.epub_files.get(index))
            .map(|file| file.relative.clone())
            .unwrap_or_default();
        match self.save_current_epub_file(cx) {
            Ok(()) => self.status = format!("Saved {name}."),
            Err(err) => self.status = err,
        }
        cx.notify();
    }

    fn repack_epub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(id), Some(dir)) = (self.epub_book_id, self.epub_dir.clone()) else {
            return;
        };
        if let Err(err) = self.save_current_epub_file(cx) {
            self.status = err;
            cx.notify();
            return;
        }
        let output =
            std::env::temp_dir().join(format!("calibre-rust-repack-{id}-{}.epub", temp_stamp()));
        let repacked = epub_formats::repack(&dir, &output);
        match repacked {
            Ok(count) => {
                let replaced = self
                    .library
                    .as_mut()
                    .map(|library| library.replace_format(id, "EPUB", &output));
                match replaced {
                    Some(Ok(())) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        let _ = std::fs::remove_file(&output);
                        self.epub_book_id = None;
                        self.epub_dir = None;
                        self.epub_files.clear();
                        self.epub_selected = None;
                        self.epub_dirty = false;
                        self.screen = Screen::Library;
                        self.load_book(id, window, cx);
                        self.reload(cx);
                        self.status =
                            format!("Repacked {count} file(s) and replaced the library EPUB.");
                    }
                    Some(Err(err)) => {
                        let _ = std::fs::remove_file(&output);
                        self.status = format!("Could not replace the library EPUB: {err}");
                    }
                    None => {
                        let _ = std::fs::remove_file(&output);
                        self.status = "Open a library first.".to_string();
                    }
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&output);
                self.status = format!("Could not repack EPUB: {err}");
            }
        }
        cx.notify();
    }

    fn close_epub_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.epub_dirty {
            self.close_epub_editor_now(cx);
            return;
        }
        let receiver = window.prompt(
            PromptLevel::Warning,
            "Save changes before closing the EPUB editor?",
            Some("Discard will lose edits made since the last save."),
            &["Save & close", "Discard", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match receiver.await {
            Ok(0) => {
                this.update_in(cx, |this, _, cx| match this.save_current_epub_file(cx) {
                    Ok(()) => this.close_epub_editor_now(cx),
                    Err(err) => {
                        this.status = err;
                        cx.notify();
                    }
                })
                .ok();
            }
            Ok(1) => {
                this.update_in(cx, |this, _, cx| this.close_epub_editor_now(cx))
                    .ok();
            }
            _ => {}
        })
        .detach();
    }

    fn close_epub_editor_now(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.epub_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.epub_book_id = None;
        self.epub_files.clear();
        self.epub_selected = None;
        self.epub_dirty = false;
        self.screen = Screen::Library;
        self.status = "Closed the EPUB editor.".to_string();
        cx.notify();
    }

    fn discard_epub_editor(&mut self) {
        if let Some(dir) = self.epub_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.epub_book_id = None;
        self.epub_files.clear();
        self.epub_selected = None;
        self.epub_dirty = false;
        if self.screen == Screen::Epub {
            self.screen = Screen::Library;
        }
    }

    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(metadata) = self.metadata.clone() {
            self.fill_form(&metadata, window, cx);
            self.editing = true;
            self.status = "Editing metadata. Save to write metadata.opf.".to_string();
            cx.notify();
        }
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(metadata) = self.metadata.clone() {
            self.fill_form(&metadata, window, cx);
        }
        self.editing = false;
        self.status = "Changes discarded.".to_string();
        cx.notify();
    }

    fn save_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let get = |input: &Entity<InputState>| input.read(cx).value().to_string();

        let title = get(&self.title_input).trim().to_string();
        if title.is_empty() {
            self.status = "Title cannot be empty.".to_string();
            cx.notify();
            return;
        }
        let authors = split_authors(&get(&self.authors_input));
        let series = get(&self.series_input).trim().to_string();
        let series_index = match text::parse_series_index(&get(&self.series_index_input)) {
            Some(index) => index,
            None => {
                self.status = "Series index must be a number.".to_string();
                cx.notify();
                return;
            }
        };
        let publisher = get(&self.publisher_input).trim().to_string();
        let tags = split_commas(&get(&self.tags_input));
        let rating_raw = get(&self.rating_input).trim().to_string();
        let rating = if rating_raw.is_empty() {
            Some(None)
        } else {
            match rating_raw.parse::<u8>() {
                Ok(value) => Some(Some(value.min(10))),
                Err(_) => {
                    self.status = "Rating must be a number from 0 to 10.".to_string();
                    cx.notify();
                    return;
                }
            }
        };
        let languages = split_commas(&get(&self.languages_input));
        let identifiers = match parse_identifiers(&get(&self.identifiers_input)) {
            Ok(identifiers) => identifiers,
            Err(err) => {
                self.status = err;
                cx.notify();
                return;
            }
        };
        let pubdate_raw = get(&self.pubdate_input).trim().to_string();
        let pubdate = if pubdate_raw.is_empty() {
            Some(None)
        } else {
            match parse_user_date(&pubdate_raw) {
                Some(date) => Some(Some(date)),
                None => {
                    self.status = "Publication date must look like YYYY-MM-DD.".to_string();
                    cx.notify();
                    return;
                }
            }
        };
        let comments_raw = self.comments_input.read(cx).value().to_string();
        let comments = if comments_raw.trim().is_empty() {
            Some(None)
        } else {
            Some(Some(htmlize_comments(&comments_raw)))
        };

        let update = BookUpdate {
            title: Some(title),
            authors: Some(authors),
            series: Some(if series.is_empty() {
                None
            } else {
                Some(series)
            }),
            series_index: Some(series_index),
            publisher: Some(if publisher.is_empty() {
                None
            } else {
                Some(publisher)
            }),
            tags: Some(tags),
            rating,
            comments,
            identifiers: Some(identifiers),
            languages: Some(languages),
            pubdate,
            ..Default::default()
        };

        let Some(library) = self.library.as_mut() else {
            self.status = "Open a library first.".to_string();
            cx.notify();
            return;
        };
        let result = library.update_book(id, &update);
        match result {
            Ok(()) => {
                self.load_book(id, window, cx);
                self.reload(cx);
                self.status = "Metadata saved and metadata.opf updated.".to_string();
            }
            Err(err) => {
                self.status = format!("Save failed: {err}");
            }
        }
        cx.notify();
    }

    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if !self.confirm_remove {
            self.confirm_remove = true;
            self.status = format!("Click Remove again to delete book #{id} and its files.");
            cx.notify();
            return;
        }
        let result = self
            .library
            .as_mut()
            .map(|library| library.remove_book(id, true));
        match result {
            Some(Ok(())) => {
                self.selected = None;
                self.metadata = None;
                self.cover_path = None;
                self.selected_formats.clear();
                self.selected_path = None;
                self.confirm_remove = false;
                self.reload(cx);
                self.status = format!("Removed book #{id}.");
            }
            Some(Err(err)) => self.status = format!("Remove failed: {err}"),
            None => self.status = "Open a library first.".to_string(),
        }
        cx.notify();
    }

    fn start_reading(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let result = self
            .library
            .as_ref()
            .map(|library| library.read_book(id, None));
        match result {
            Some(Ok(book)) if !book.chapters.is_empty() => {
                self.chapters = book.chapters;
                self.chapter_index = 0;
                self.reader_title = self
                    .metadata
                    .as_ref()
                    .map(|metadata| metadata.title.clone())
                    .unwrap_or_else(|| format!("Book #{id}"));
                self.screen = Screen::Reader;
                self.status = format!("Reading {} chapter(s).", self.chapters.len());
            }
            Some(Ok(_)) => {
                self.status =
                    "No readable text in this format (try EPUB, FB2, TXT or HTML).".to_string();
            }
            Some(Err(err)) => self.status = format!("Cannot read this book: {err}"),
            None => self.status = "Open a library first.".to_string(),
        }
        cx.notify();
    }

    fn step_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.chapters.is_empty() {
            return;
        }
        let next = self.chapter_index as isize + delta;
        if next >= 0 && (next as usize) < self.chapters.len() {
            self.chapter_index = next as usize;
            cx.notify();
        }
    }

    fn set_chapter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.chapters.len() {
            self.chapter_index = index;
            cx.notify();
        }
    }

    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        self.devices = device::discover();
        self.status = format!("{} device(s) detected.", self.devices.len());
        cx.notify();
    }

    fn send_selected(&mut self, device: Device, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let result = self
            .library
            .as_ref()
            .map(|library| device::send(library, &device, &[id], &SendOptions::default()));
        match result {
            Some(Ok(report)) => {
                if let Some(sent) = report.sent.first() {
                    self.status = format!(
                        "Sent {} ({}) to {}.",
                        sent.title,
                        format_size(sent.bytes),
                        device.name
                    );
                } else if let Some((_, reason)) = report.skipped.first() {
                    self.status = format!("Nothing sent: {reason}");
                }
            }
            Some(Err(err)) => self.status = format!("Send failed: {err}"),
            None => self.status = "Open a library first.".to_string(),
        }
        cx.notify();
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        Theme::change(
            if self.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
        cx.notify();
    }
}

impl Render for CalibreApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let header = header(self, cx);
        let body = match self.screen {
            Screen::Reader => reader_view(self, cx),
            Screen::Epub => epub_editor_view(self, cx),
            Screen::Library => library_view(self, cx),
        };
        let status = status_bar(self, cx);

        v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(header)
            .child(body)
            .child(status)
    }
}

fn header(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let surface = cx.theme().secondary;
    let background = cx.theme().background;
    let border = cx.theme().border;
    let primary = cx.theme().primary;

    h_flex()
        .h(px(56.))
        .px_4()
        .gap_2()
        .items_center()
        .bg(surface)
        .border_b_1()
        .border_color(border)
        .child(
            div()
                .w(px(28.))
                .h(px(28.))
                .rounded(px(7.))
                .bg(primary)
                .text_color(background)
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .child("cr"),
        )
        .child(Label::new("calibre-rust").font_weight(FontWeight::BOLD))
        .child(div().w(px(16.)))
        .child(Input::new(&app.library_path).w(px(360.)))
        .child(
            Button::new("open-library")
                .primary()
                .label("Open")
                .on_click(cx.listener(|this, _, window, cx| {
                    let path = PathBuf::from(this.library_path.read(cx).value().to_string());
                    this.open_library(path, window, cx);
                })),
        )
        .child(
            Button::new("new-library")
                .label("New")
                .on_click(cx.listener(|this, _, window, cx| this.create_library(window, cx))),
        )
        .child(
            Button::new("browse-library")
                .label("Browse...")
                .on_click(cx.listener(|this, _, window, cx| this.browse_library(window, cx))),
        )
        .child(div().flex_1())
        .child(
            Button::new("add-books")
                .primary()
                .icon(IconName::Plus)
                .label("Add books")
                .on_click(cx.listener(|this, _, window, cx| this.browse_add_books(window, cx))),
        )
        .child(
            Button::new("export-all")
                .label(if app.query.is_empty() {
                    "Export all"
                } else {
                    "Export filtered"
                })
                .on_click(cx.listener(|this, _, window, cx| this.browse_export_all(window, cx))),
        )
        .child(
            Button::new("refresh-devices")
                .icon(IconName::RefreshCw)
                .label("Devices")
                .on_click(cx.listener(|this, _, _, cx| this.refresh_devices(cx))),
        )
        .child(
            Button::new("toggle-theme")
                .label(if app.dark { "Light" } else { "Dark" })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
        )
        .into_any_element()
}

fn library_view(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let background = cx.theme().background;
    if app.library.is_none() {
        return empty_state(
            "No library open",
            "Enter the path to a calibre library above, choose a folder, or create a new one.",
            cx,
        );
    }
    h_flex()
        .flex_1()
        .overflow_hidden()
        .bg(background)
        .child(sidebar(app, cx))
        .child(detail_panel(app, cx))
        .into_any_element()
}

fn sidebar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let surface = cx.theme().secondary;
    let border = cx.theme().border;
    let primary = cx.theme().primary;
    let muted = cx.theme().muted_foreground;

    let rows: Vec<AnyElement> = app
        .books
        .iter()
        .map(|book| {
            let id = book.id;
            let selected = app.selected == Some(id);
            let title = book.title.clone();
            let authors = if book.authors.is_empty() {
                "Unknown".to_string()
            } else {
                book.authors.join(" & ")
            };
            let formats = book.formats.join(" ");
            let muted = muted;
            let selected_bg = primary.opacity(0.12);
            let hover_bg = primary.opacity(0.06);
            h_flex()
                .id(("book", id as usize))
                .w_full()
                .px_3()
                .py_2()
                .gap_3()
                .rounded(px(6.))
                .cursor_pointer()
                .bg(if selected { selected_bg } else { surface })
                .hover(move |style| style.bg(hover_bg))
                .on_click(cx.listener(move |this, _, window, cx| this.select_book(id, window, cx)))
                .child(div().w(px(3.)).h(px(34.)).rounded(px(2.)).bg(if selected {
                    primary
                } else {
                    surface
                }))
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .child(Label::new(title).font_weight(FontWeight::MEDIUM))
                        .child(Label::new(authors).text_xs().text_color(muted)),
                )
                .child(div().text_xs().text_color(muted).child(formats))
                .into_any_element()
        })
        .collect();

    let count = format!("{} book(s)", app.books.len());
    v_flex()
        .w(px(348.))
        .h_full()
        .bg(surface)
        .border_r_1()
        .border_color(border)
        .child(
            v_flex()
                .p_3()
                .gap_2()
                .child(
                    Input::new(&app.search)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .child(Label::new(count).text_xs().text_color(muted))
                        .child(
                            Label::new(if app.query.is_empty() {
                                "All books".to_string()
                            } else {
                                format!("Filter: {}", app.query)
                            })
                            .text_xs()
                            .text_color(muted),
                        ),
                ),
        )
        .child(
            div()
                .id("library-scroll")
                .flex_1()
                .overflow_y_scroll()
                .child(v_flex().px_2().pb_3().gap_1().children(rows)),
        )
        .into_any_element()
}

fn detail_panel(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let border = cx.theme().border;
    if app.cropping {
        return crop_panel(app, cx);
    }
    let Some(metadata) = &app.metadata else {
        return v_flex()
            .flex_1()
            .child(empty_state(
                "Select a book",
                "Choose a book in the library to see its metadata, read it or send it to a device.",
                cx,
            ))
            .into_any_element();
    };
    let Some(id) = app.selected else {
        return empty_state("Select a book", "No book selected.", cx);
    };

    let muted = cx.theme().muted_foreground;
    let primary = cx.theme().primary;
    let surface = cx.theme().secondary;

    let title = metadata.title.clone();
    let authors = if metadata.authors.is_empty() {
        "Unknown".to_string()
    } else {
        metadata.authors.join(" & ")
    };
    let series = metadata.series.clone();
    let rating = metadata.rating;
    let formats = app.selected_formats.join(" · ");
    let tags = metadata.tags.clone();

    let cover = cover_element(app, cx);

    let mut meta_badges = h_flex().gap_2().flex_wrap();
    if let Some(series) = series.clone() {
        meta_badges = meta_badges.child(badge(
            format!(
                "{} #{}",
                series,
                text::format_series_index(metadata.series_index)
            ),
            primary.opacity(0.12),
            primary,
        ));
    }
    if let Some(rating) = rating {
        meta_badges = meta_badges.child(badge(
            format!("{}/5", rating / 2),
            primary.opacity(0.12),
            primary,
        ));
    }
    meta_badges = meta_badges.child(badge(
        if formats.is_empty() {
            "no formats".to_string()
        } else {
            formats
        },
        surface,
        muted,
    ));

    let header =
        h_flex()
            .gap_5()
            .items_start()
            .child(cover)
            .child(
                v_flex()
                    .flex_1()
                    .gap_2()
                    .child(Label::new(title).text_xl().font_weight(FontWeight::BOLD))
                    .child(Label::new(authors).text_color(muted))
                    .child(meta_badges)
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .children(tags.iter().map(|tag| badge(tag.clone(), surface, muted))),
                    ),
            )
            .child(
                v_flex()
                    .w(px(170.))
                    .gap_2()
                    .child(
                        Button::new("read-book")
                            .primary()
                            .icon(IconName::BookOpen)
                            .label("Read")
                            .on_click(cx.listener(|this, _, _, cx| this.start_reading(cx))),
                    )
                    .child(if app.editing {
                        Button::new("cancel-edit").label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.cancel_edit(window, cx)),
                        )
                    } else {
                        Button::new("edit-book").label("Edit metadata").on_click(
                            cx.listener(|this, _, window, cx| this.begin_edit(window, cx)),
                        )
                    })
                    .child(
                        Button::new("change-cover").label("Cover...").on_click(
                            cx.listener(|this, _, window, cx| this.browse_cover(window, cx)),
                        ),
                    )
                    .child(Button::new("crop-cover").label("Crop cover").on_click(
                        cx.listener(|this, _, window, cx| this.begin_cover_crop(window, cx)),
                    ))
                    .child(Button::new("edit-epub").label("Edit EPUB...").on_click(
                        cx.listener(|this, _, window, cx| this.begin_epub_edit(window, cx)),
                    ))
                    .child(Button::new("export-book").label("Export...").on_click(
                        cx.listener(|this, _, window, cx| this.browse_export(window, cx)),
                    ))
                    .child(
                        Button::new("remove-book")
                            .danger()
                            .label(if app.confirm_remove {
                                "Confirm remove"
                            } else {
                                "Remove"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx))),
                    ),
            );

    let body = if app.editing {
        edit_form(app, cx)
    } else {
        details(app, metadata, cx)
    };

    v_flex()
        .id("details-scroll")
        .flex_1()
        .h_full()
        .overflow_y_scroll()
        .p_6()
        .gap_5()
        .child(header)
        .child(div().h(px(1.)).w_full().bg(border))
        .child(body)
        .child(devices_panel(app, id, cx))
        .into_any_element()
}

fn details(app: &CalibreApp, metadata: &Metadata, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;

    let series = match &metadata.series {
        Some(series) => format!(
            "{} #{}",
            series,
            text::format_series_index(metadata.series_index)
        ),
        None => String::new(),
    };
    let identifiers = metadata
        .identifiers
        .iter()
        .map(|(kind, value)| format!("{kind}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    let comments = metadata
        .comments
        .as_deref()
        .map(html::html_to_text)
        .unwrap_or_default();
    let comments = if comments.chars().count() > 1200 {
        format!("{}…", comments.chars().take(1200).collect::<String>())
    } else {
        comments
    };

    v_flex()
        .w_full()
        .gap_3()
        .child(metadata_row("Series", series, muted))
        .child(metadata_row(
            "Publisher",
            metadata.publisher.clone().unwrap_or_default(),
            muted,
        ))
        .child(metadata_row("Tags", metadata.tags.join(", "), muted))
        .child(metadata_row(
            "Rating",
            metadata
                .rating
                .map(|rating| format!("{}/5", rating / 2))
                .unwrap_or_default(),
            muted,
        ))
        .child(metadata_row(
            "Languages",
            metadata.languages.join(", "),
            muted,
        ))
        .child(metadata_row("Identifiers", identifiers, muted))
        .child(metadata_row(
            "Published",
            metadata
                .pubdate
                .as_ref()
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            muted,
        ))
        .child(metadata_row(
            "Added",
            metadata
                .timestamp
                .as_ref()
                .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default(),
            muted,
        ))
        .child(metadata_row(
            "Formats",
            app.selected_formats.join(", "),
            muted,
        ))
        .child(metadata_row(
            "Path",
            app.selected_path.clone().unwrap_or_default(),
            muted,
        ))
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .pt_2()
                .child(Label::new("Comments").text_xs().text_color(muted))
                .child(
                    div()
                        .id("comments-scroll")
                        .w_full()
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .p_3()
                        .rounded(px(6.))
                        .bg(cx.theme().secondary)
                        .border_1()
                        .border_color(border)
                        .text_sm()
                        .child(if comments.trim().is_empty() {
                            "No comments.".to_string()
                        } else {
                            comments
                        }),
                ),
        )
        .into_any_element()
}

fn edit_form(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;

    let field_input =
        |label: &str, input: &Entity<InputState>| field(label, Input::new(input).w_full(), muted);

    v_flex()
        .w_full()
        .gap_4()
        .child(
            h_flex()
                .flex_wrap()
                .gap_4()
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Title", &app.title_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Authors", &app.authors_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Series", &app.series_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Series index", &app.series_index_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Publisher", &app.publisher_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Tags", &app.tags_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Rating (0-10)", &app.rating_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Languages", &app.languages_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Identifiers", &app.identifiers_input)),
                )
                .child(
                    div()
                        .w(px(310.))
                        .child(field_input("Published", &app.pubdate_input)),
                ),
        )
        .child(field(
            "Comments / description",
            Textarea::new(&app.comments_input).w_full().h(px(150.)),
            muted,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("save-metadata")
                        .primary()
                        .label("Save metadata")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_metadata(window, cx)),
                        ),
                )
                .child(
                    Button::new("discard-metadata")
                        .label("Discard")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
                ),
        )
        .into_any_element()
}

fn devices_panel(app: &CalibreApp, _id: i64, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;

    let device_buttons: Vec<AnyElement> = app
        .devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            let device = device.clone();
            Button::new(("send-to-device", index))
                .label(format!("Send to {}", device.name))
                .on_click(cx.listener(move |this, _, _, cx| this.send_selected(device.clone(), cx)))
                .into_any_element()
        })
        .collect();

    v_flex()
        .w_full()
        .pt_4()
        .gap_2()
        .border_t_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Label::new("Send to device").font_weight(FontWeight::MEDIUM))
                .child(
                    Button::new("scan-devices")
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .label("Scan")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_devices(cx))),
                ),
        )
        .child(if app.devices.is_empty() {
            div()
                .text_sm()
                .text_color(muted)
                .child("No e-reader detected. Connect a Kindle, Kobo or USB reader and press Scan.")
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .flex_wrap()
                .children(device_buttons)
                .into_any_element()
        })
        .into_any_element()
}

fn reader_view(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    if app.chapters.is_empty() {
        return empty_state(
            "Nothing to read",
            "Choose a book with readable text and press Read.",
            cx,
        );
    }
    let border = cx.theme().border;
    let surface = cx.theme().secondary;
    let muted = cx.theme().muted_foreground;
    let primary = cx.theme().primary;

    let current = app.chapters.get(app.chapter_index);
    let chapter_title = current
        .map(|chapter| chapter.title.clone())
        .unwrap_or_default();
    let chapter_text = current
        .map(|chapter| chapter.text.clone())
        .unwrap_or_default();
    let total_words: usize = app
        .chapters
        .iter()
        .map(|chapter| chapter.text.split_whitespace().count())
        .sum();

    let chapter_rows: Vec<AnyElement> = app
        .chapters
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            let selected = index == app.chapter_index;
            let title = chapter.title.clone();
            let words = chapter.text.split_whitespace().count();
            h_flex()
                .id(("chapter", index))
                .w_full()
                .px_3()
                .py_2()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .bg(if selected {
                    primary.opacity(0.12)
                } else {
                    surface
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_chapter(index, cx)))
                .child(
                    div()
                        .w(px(26.))
                        .text_xs()
                        .text_color(if selected { primary } else { muted })
                        .child(format!("{:>2}", index + 1)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .child(Label::new(title).text_sm())
                        .child(
                            Label::new(format!("{words} words"))
                                .text_xs()
                                .text_color(muted),
                        ),
                )
                .into_any_element()
        })
        .collect();

    let paragraphs: Vec<AnyElement> = chapter_text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| {
            div()
                .w_full()
                .text_base()
                .child(paragraph.to_string())
                .into_any_element()
        })
        .collect();

    v_flex()
        .flex_1()
        .overflow_hidden()
        .child(
            h_flex()
                .px_4()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(border)
                .bg(surface)
                .child(
                    Button::new("back-to-library")
                        .icon(IconName::ChevronLeft)
                        .label("Library")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Screen::Library;
                            cx.notify();
                        })),
                )
                .child(Label::new(app.reader_title.clone()).font_weight(FontWeight::BOLD))
                .child(div().flex_1())
                .child(
                    Button::new("previous-chapter")
                        .icon(IconName::ChevronLeft)
                        .label("Prev")
                        .on_click(cx.listener(|this, _, _, cx| this.step_chapter(-1, cx))),
                )
                .child(
                    Label::new(format!(
                        "{} / {}",
                        app.chapter_index + 1,
                        app.chapters.len()
                    ))
                    .text_sm()
                    .text_color(muted),
                )
                .child(
                    Button::new("next-chapter")
                        .icon(IconName::ChevronRight)
                        .label("Next")
                        .on_click(cx.listener(|this, _, _, cx| this.step_chapter(1, cx))),
                ),
        )
        .child(
            h_flex()
                .flex_1()
                .overflow_hidden()
                .child(
                    v_flex()
                        .w(px(300.))
                        .h_full()
                        .bg(surface)
                        .border_r_1()
                        .border_color(border)
                        .child(
                            v_flex()
                                .p_3()
                                .gap_1()
                                .child(Label::new("Chapters").font_weight(FontWeight::BOLD))
                                .child(
                                    Label::new(format!(
                                        "{} chapters · {} words",
                                        app.chapters.len(),
                                        total_words
                                    ))
                                    .text_xs()
                                    .text_color(muted),
                                ),
                        )
                        .child(
                            div()
                                .id("chapter-scroll")
                                .flex_1()
                                .overflow_y_scroll()
                                .child(v_flex().px_2().pb_3().gap_1().children(chapter_rows)),
                        ),
                )
                .child(
                    div()
                        .id("reader-scroll")
                        .flex_1()
                        .h_full()
                        .overflow_y_scroll()
                        .p_8()
                        .child(
                            v_flex()
                                .w_full()
                                .max_w(px(820.))
                                .gap_4()
                                .child(
                                    Label::new(chapter_title)
                                        .text_xl()
                                        .font_weight(FontWeight::BOLD),
                                )
                                .child(div().w(px(48.)).h(px(2.)).bg(primary))
                                .children(paragraphs),
                        ),
                ),
        )
        .into_any_element()
}

fn crop_panel(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let border = cx.theme().border;
    let surface = cx.theme().secondary;
    let muted = cx.theme().muted_foreground;
    let primary = cx.theme().primary;

    let (original_width, original_height) = match app.crop_source.as_ref() {
        Some(source) => fit_dims(source.width() as f32, source.height() as f32, 280., 420.),
        None => (280., 420.),
    };
    let (crop_left, crop_top, crop_width, crop_height) =
        app.crop_percentages(cx).unwrap_or((0.0, 0.0, 100.0, 100.0));
    let bounds_cell = Rc::new(Cell::new((0.0f32, 0.0f32, 0.0f32, 0.0f32)));
    let original = match &app.cover_path {
        Some(path) => {
            let bounds_for_mouse = bounds_cell.clone();
            div()
                .relative()
                .w(px(original_width))
                .h(px(original_height))
                .rounded(px(6.))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        let bounds = bounds_for_mouse.get();
                        this.start_crop_drag(event.position, bounds, cx);
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    if event.dragging() {
                        this.drag_crop(event.position, window, cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseUpEvent, _, cx| this.end_crop_drag(cx)),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseUpEvent, _, cx| this.end_crop_drag(cx)),
                )
                .child(img(path.clone()).absolute().size_full())
                .child(
                    canvas(
                        {
                            let bounds_cell = bounds_cell.clone();
                            move |bounds, _, _| {
                                bounds_cell.set((
                                    bounds.origin.x.as_f32(),
                                    bounds.origin.y.as_f32(),
                                    bounds.size.width.as_f32(),
                                    bounds.size.height.as_f32(),
                                ))
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(crop_left / 100.0))
                        .top(relative(crop_top / 100.0))
                        .w(relative(crop_width / 100.0))
                        .h(relative(crop_height / 100.0))
                        .border_1()
                        .border_color(primary)
                        .child(
                            div()
                                .absolute()
                                .top(-px(4.))
                                .left(-px(4.))
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .bg(primary),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(-px(4.))
                                .right(-px(4.))
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .bg(primary),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom(-px(4.))
                                .left(-px(4.))
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .bg(primary),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom(-px(4.))
                                .right(-px(4.))
                                .w(px(8.))
                                .h(px(8.))
                                .rounded(px(4.))
                                .bg(primary),
                        ),
                )
                .into_any_element()
        }
        None => div()
            .w(px(original_width))
            .h(px(original_height))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(surface)
            .into_any_element(),
    };
    let (preview_width, preview_height) = app.crop_view_dims(cx);
    let preview = match &app.crop_preview_path {
        Some(path) => img(path.clone())
            .w(px(preview_width))
            .h(px(preview_height))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .into_any_element(),
        None => v_flex()
            .w(px(preview_width))
            .h(px(preview_height))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(surface)
            .items_center()
            .justify_center()
            .child(Label::new("Press Preview crop").text_xs().text_color(muted))
            .into_any_element(),
    };
    let source_dims = app
        .crop_source
        .as_ref()
        .map(|source| format!("{} x {} px", source.width(), source.height()))
        .unwrap_or_default();

    v_flex()
        .id("crop-scroll")
        .flex_1()
        .h_full()
        .overflow_y_scroll()
        .p_6()
        .gap_5()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("cancel-crop-top")
                        .icon(IconName::ChevronLeft)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_crop(cx))),
                )
                .child(
                    Label::new("Crop cover")
                        .text_xl()
                        .font_weight(FontWeight::BOLD),
                )
                .child(div().flex_1())
                .child(Label::new(source_dims).text_xs().text_color(muted)),
        )
        .child(
            h_flex()
                .gap_6()
                .items_start()
                .child(
                    v_flex()
                        .gap_2()
                        .child(Label::new("Crop selection").text_xs().text_color(muted))
                        .child(original)
                        .child(
                            Label::new("Drag the box or its corners to crop.")
                                .text_xs()
                                .text_color(muted),
                        ),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_2()
                                .child(
                                    Label::new("Preview (updates on release)")
                                        .text_xs()
                                        .text_color(muted),
                                )
                                .child(preview),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("crop-preset-2-3")
                                        .label("2:3")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_crop_preset(Some(2.0 / 3.0), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("crop-preset-square")
                                        .label("1:1")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_crop_preset(Some(1.0), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("crop-preset-3-4")
                                        .label("3:4")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_crop_preset(Some(0.75), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("crop-preset-original")
                                        .label("Original")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_crop_preset(None, window, cx)
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_3()
                                .flex_wrap()
                                .child(
                                    div().w(px(140.)).child(field(
                                        "Left %",
                                        Input::new(&app.crop_left).w_full(),
                                        muted,
                                    )),
                                )
                                .child(
                                    div().w(px(140.)).child(field(
                                        "Top %",
                                        Input::new(&app.crop_top).w_full(),
                                        muted,
                                    )),
                                )
                                .child(
                                    div().w(px(140.)).child(field(
                                        "Width %",
                                        Input::new(&app.crop_width).w_full(),
                                        muted,
                                    )),
                                )
                                .child(
                                    div().w(px(140.)).child(field(
                                        "Height %",
                                        Input::new(&app.crop_height).w_full(),
                                        muted,
                                    )),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("preview-crop")
                                        .label("Preview crop")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            match this.update_crop_preview(cx) {
                                                Ok(()) => {
                                                    this.status =
                                                        "Crop preview updated.".to_string()
                                                }
                                                Err(err) => this.status = err,
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("save-crop")
                                        .primary()
                                        .label("Save cropped cover")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_crop(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("cancel-crop")
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_crop(cx)
                                        })),
                                ),
                        ),
                ),
        )
        .child(
            Label::new(
                "Percentages are relative to the original cover. 2:3 is the usual ebook cover ratio.",
            )
            .text_xs()
            .text_color(muted),
        )
        .into_any_element()
}

fn epub_editor_view(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let border = cx.theme().border;
    let surface = cx.theme().secondary;
    let muted = cx.theme().muted_foreground;
    let primary = cx.theme().primary;

    if app.epub_files.is_empty() {
        return empty_state(
            "No editable EPUB files",
            "Close the editor and try another book.",
            cx,
        );
    }
    let Some(selected) = app.epub_selected else {
        return empty_state("No file selected", "Choose a file on the left.", cx);
    };
    let Some(current) = app.epub_files.get(selected) else {
        return empty_state("No file selected", "Choose a file on the left.", cx);
    };
    let book_title = app
        .metadata
        .as_ref()
        .map(|metadata| metadata.title.clone())
        .unwrap_or_else(|| "EPUB".to_string());

    let rows: Vec<AnyElement> = app
        .epub_files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let is_selected = app.epub_selected == Some(index);
            let relative = file.relative.clone();
            h_flex()
                .id(("epub-file", index))
                .w_full()
                .px_3()
                .py_2()
                .rounded(px(5.))
                .cursor_pointer()
                .bg(if is_selected {
                    primary.opacity(0.12)
                } else {
                    surface
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| this.load_epub_file(index, window, cx)),
                )
                .child(Label::new(relative).text_xs())
                .into_any_element()
        })
        .collect();

    let dirty_label = if app.epub_dirty {
        "Unsaved changes"
    } else {
        "Saved"
    };

    v_flex()
        .flex_1()
        .overflow_hidden()
        .child(
            h_flex()
                .px_4()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(border)
                .bg(surface)
                .child(
                    Button::new("close-epub-editor")
                        .icon(IconName::ChevronLeft)
                        .label("Library")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.close_epub_editor(window, cx)),
                        ),
                )
                .child(Label::new("EPUB editor").font_weight(FontWeight::BOLD))
                .child(Label::new(book_title).text_sm().text_color(muted))
                .child(div().flex_1())
                .child(
                    Label::new(dirty_label)
                        .text_xs()
                        .text_color(if app.epub_dirty { primary } else { muted }),
                )
                .child(
                    Button::new("save-epub-file")
                        .label("Save file")
                        .on_click(cx.listener(|this, _, _, cx| this.save_epub_action(cx))),
                )
                .child(
                    Button::new("repack-epub")
                        .primary()
                        .label("Repack & replace")
                        .on_click(cx.listener(|this, _, window, cx| this.repack_epub(window, cx))),
                ),
        )
        .child(
            h_flex()
                .flex_1()
                .overflow_hidden()
                .child(
                    v_flex()
                        .w(px(330.))
                        .h_full()
                        .bg(surface)
                        .border_r_1()
                        .border_color(border)
                        .child(
                            v_flex()
                                .p_3()
                                .gap_1()
                                .child(Label::new("Editable files").font_weight(FontWeight::MEDIUM))
                                .child(
                                    Label::new(format!("{} file(s)", app.epub_files.len()))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                        )
                        .child(
                            div()
                                .id("epub-file-scroll")
                                .flex_1()
                                .overflow_y_scroll()
                                .child(v_flex().px_2().pb_3().gap_1().children(rows)),
                        ),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .min_h(px(0.))
                        .p_4()
                        .gap_2()
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    Label::new(current.relative.clone())
                                        .font_weight(FontWeight::MEDIUM),
                                )
                                .child(
                                    Label::new(format_size(current.size))
                                        .text_xs()
                                        .text_color(muted),
                                ),
                        )
                        .child(Textarea::new(&app.epub_editor).w_full().h(relative(1.))),
                ),
        )
        .into_any_element()
}

fn cover_element(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let border = cx.theme().border;
    let surface = cx.theme().secondary;
    let muted = cx.theme().muted_foreground;

    match &app.cover_path {
        Some(path) => img(path.clone())
            .w(px(132.))
            .h(px(198.))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .into_any_element(),
        None => v_flex()
            .w(px(132.))
            .h(px(198.))
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(surface)
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::BookOpen).large().text_color(muted))
            .into_any_element(),
    }
}

fn metadata_row(label: &str, value: String, muted: Hsla) -> AnyElement {
    h_flex()
        .w_full()
        .gap_4()
        .items_start()
        .child(
            div()
                .w(px(110.))
                .text_sm()
                .text_color(muted)
                .child(label.to_string()),
        )
        .child(div().flex_1().text_sm().child(if value.trim().is_empty() {
            "—".to_string()
        } else {
            value
        }))
        .into_any_element()
}

fn field(label: &str, control: impl IntoElement, muted: Hsla) -> AnyElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(Label::new(label.to_string()).text_xs().text_color(muted))
        .child(control)
        .into_any_element()
}

fn badge(text: String, background: Hsla, foreground: Hsla) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded(px(5.))
        .bg(background)
        .text_color(foreground)
        .text_xs()
        .child(text)
        .into_any_element()
}

fn empty_state(title: &str, subtitle: &str, cx: &mut Context<CalibreApp>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .flex_1()
        .h_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_8()
        .child(Icon::new(IconName::BookOpen).large().text_color(muted))
        .child(
            Label::new(title.to_string())
                .text_xl()
                .font_weight(FontWeight::BOLD),
        )
        .child(
            div()
                .max_w(px(460.))
                .text_center()
                .text_sm()
                .text_color(muted)
                .child(subtitle.to_string()),
        )
        .into_any_element()
}

fn status_bar(app: &CalibreApp, cx: &mut Context<CalibreApp>) -> AnyElement {
    let surface = cx.theme().secondary;
    let border = cx.theme().border;
    let muted = cx.theme().muted_foreground;
    h_flex()
        .h(px(26.))
        .px_4()
        .items_center()
        .bg(surface)
        .border_t_1()
        .border_color(border)
        .text_xs()
        .text_color(muted)
        .child(app.status.clone())
        .into_any_element()
}

fn text_input(
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()))
}

fn set_input(
    input: &Entity<InputState>,
    value: String,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn text_area(
    placeholder: &str,
    rows: usize,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder.to_string())
            .rows(rows)
    })
}

fn set_textarea(
    input: &Entity<TextareaState>,
    value: String,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn crop_pixels(
    source_width: u32,
    source_height: u32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
) -> (u32, u32, u32, u32) {
    let max_x = source_width.saturating_sub(1);
    let max_y = source_height.saturating_sub(1);
    let x = (((left / 100.0) * source_width as f32).round() as u32).min(max_x);
    let y = (((top / 100.0) * source_height as f32).round() as u32).min(max_y);
    let max_width = source_width.saturating_sub(x).max(1);
    let max_height = source_height.saturating_sub(y).max(1);
    let width = (((width / 100.0) * source_width as f32).round() as u32).clamp(1, max_width);
    let height = (((height / 100.0) * source_height as f32).round() as u32).clamp(1, max_height);
    (x, y, width, height)
}

fn fit_dims(width: f32, height: f32, max_width: f32, max_height: f32) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 {
        return (max_width, max_height);
    }
    let scale = (max_width / width).min(max_height / height);
    (width * scale, height * scale)
}

fn scan_epub_files(root: &Path) -> Vec<EpubFile> {
    let mut files = Vec::new();
    collect_epub_files(root, root, &mut files);
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    files
}

fn collect_epub_files(root: &Path, directory: &Path, files: &mut Vec<EpubFile>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_epub_files(root, &path, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "xhtml" | "html" | "htm" | "css" | "opf" | "ncx" | "xml" | "svg" | "txt" | "js"
        ) {
            continue;
        }
        files.push(EpubFile {
            relative: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string(),
            size: entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
            path,
        });
    }
}

fn temp_stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn split_commas(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_identifiers(input: &str) -> Result<BTreeMap<String, String>, String> {
    let mut identifiers = BTreeMap::new();
    for item in input.split([',', ';', '\n']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (kind, value) = item
            .split_once('=')
            .or_else(|| item.split_once(':'))
            .ok_or_else(|| format!("Identifier must be type=value: {item}"))?;
        let kind = kind.trim().to_ascii_lowercase();
        let value = value.trim();
        if kind.is_empty() || value.is_empty() {
            return Err(format!("Identifier must be type=value: {item}"));
        }
        identifiers.insert(kind, value.to_string());
    }
    Ok(identifiers)
}

fn default_library_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Calibre Library")
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
