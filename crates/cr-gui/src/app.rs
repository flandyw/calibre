//! The calibre-rust main window: state, library operations and action handlers.
//! Rendering lives in [`crate::ui`].

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cr_core::device::{self, Device, SendOptions};
use cr_core::formats::editing::{EpubDir, Severity, EDITABLE_FORMATS};
use cr_core::formats::reading::{Book, Hit, Locator};
use cr_core::jobs::{self, Canceller, JobContext, Progress};
use cr_core::library::{
    AddOutcome, Annotation, AnnotationKind, BookSort, BookUpdate, DuplicatePolicy, ExportOptions,
    ImportOptions, Library, ListOptions, SaveOutcome,
};
use cr_core::metadata::{htmlize_comments, parse_user_date, BookSummary, Metadata};
use cr_core::text::{self, split_authors};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use image::DynamicImage;

use crate::actions::*;
use crate::files;
use crate::settings::{
    SavedBounds, Settings, ThemePreference, ViewMode, DEFAULT_READER_FONT_SIZE,
    MAX_READER_FONT_SIZE, MIN_READER_FONT_SIZE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Library,
    Reader,
    Epub,
    Crop,
}

/// A sidebar entry that narrows the book list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shelf {
    All,
    RecentlyAdded,
    Author(String),
    Series(String),
    Tag(String),
    Format(String),
}

impl Shelf {
    pub fn title(&self) -> String {
        match self {
            Shelf::All => "All Books".to_string(),
            Shelf::RecentlyAdded => "Recently Added".to_string(),
            Shelf::Author(name) | Shelf::Series(name) | Shelf::Tag(name) | Shelf::Format(name) => {
                name.clone()
            }
        }
    }

    fn matches(&self, book: &BookSummary) -> bool {
        match self {
            Shelf::All | Shelf::RecentlyAdded => true,
            Shelf::Author(name) => book.authors.iter().any(|author| author == name),
            Shelf::Series(name) => book.series.as_deref() == Some(name.as_str()),
            Shelf::Tag(name) => book.tags.iter().any(|tag| tag == name),
            Shelf::Format(name) => book.formats.iter().any(|format| format == name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShelfSection {
    Authors,
    Series,
    Tags,
    Formats,
}

/// Category counts shown in the sidebar, derived from the whole library.
#[derive(Default)]
pub struct Categories {
    pub authors: Vec<(String, usize)>,
    pub series: Vec<(String, usize)>,
    pub tags: Vec<(String, usize)>,
    pub formats: Vec<(String, usize)>,
}

impl Categories {
    fn from_books(books: &[BookSummary]) -> Self {
        let mut authors = BTreeMap::<String, usize>::new();
        let mut series = BTreeMap::<String, usize>::new();
        let mut tags = BTreeMap::<String, usize>::new();
        let mut formats = BTreeMap::<String, usize>::new();
        for book in books {
            for author in &book.authors {
                *authors.entry(author.clone()).or_default() += 1;
            }
            if let Some(name) = &book.series {
                *series.entry(name.clone()).or_default() += 1;
            }
            for tag in &book.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
            for format in &book.formats {
                *formats.entry(format.clone()).or_default() += 1;
            }
        }
        let sorted = |map: BTreeMap<String, usize>| {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by_key(|(name, _)| name.to_lowercase());
            entries
        };
        Self {
            authors: sorted(authors),
            series: sorted(series),
            tags: sorted(tags),
            formats: sorted(formats),
        }
    }

    pub fn entries(&self, section: ShelfSection) -> &[(String, usize)] {
        match section {
            ShelfSection::Authors => &self.authors,
            ShelfSection::Series => &self.series,
            ShelfSection::Tags => &self.tags,
            ShelfSection::Formats => &self.formats,
        }
    }
}

#[derive(Clone)]
pub struct EpubFile {
    pub relative: String,
    pub path: PathBuf,
    pub size: u64,
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

/// What to do once unsaved changes have been saved or discarded.
#[derive(Clone, Copy)]
enum AfterResolve {
    CloseWindow,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Error,
}

const EXPORT_TEMPLATE: &str = "{author_sort}/{title} ({id})/{title} - {author}";
const RECENTLY_ADDED_LIMIT: usize = 50;
/// Formats tried first when opening or dragging a book out of the app.
/// The format of a book the editor opens, if it has one.
pub fn editable_format(formats: &[String]) -> Option<&'static str> {
    EDITABLE_FORMATS
        .iter()
        .find(|format| formats.iter().any(|own| own.eq_ignore_ascii_case(format)))
        .copied()
}

const PREFERRED_FORMATS: &[&str] = &["EPUB", "PDF", "AZW3", "MOBI", "FB2", "TXT"];

/// A long operation running off the UI thread, shown in the status bar.
pub struct RunningJob {
    pub label: String,
    pub progress: Progress,
    pub cancelling: bool,
    canceller: Canceller,
}

pub struct CalibreApp {
    pub settings: Settings,
    pub job: Option<RunningJob>,
    /// Close or quit requested while a job was running; done once it stops.
    close_after_job: Option<AfterResolve>,
    pub library: Option<Library>,
    pub devices: Vec<Device>,
    pub scanning_devices: bool,
    pub sending_to_device: bool,
    /// Every book in the library, for the sidebar categories.
    pub all_books: Vec<BookSummary>,
    pub categories: Categories,
    /// Books currently shown: the shelf and search applied.
    pub books: Vec<BookSummary>,
    pub shelf: Shelf,
    pub expanded_sections: HashSet<ShelfSection>,
    pub selected: Option<i64>,
    /// Every selected book, including `selected` (the one shown in the
    /// inspector). Batch actions apply to all of them.
    pub marked: HashSet<i64>,
    /// Where a Shift-click range starts.
    anchor: Option<usize>,
    pub metadata: Option<Metadata>,
    pub cover_path: Option<PathBuf>,
    pub selected_formats: Vec<String>,
    pub selected_path: Option<String>,
    pub book: Option<Rc<Book>>,
    pub reader_book_id: i64,
    pub section_index: usize,
    /// Decoded once per book; `None` for images that cannot be shown.
    pub reader_images: Vec<Option<std::sync::Arc<Image>>>,
    /// The loaded name of each font family the open book embeds.
    pub reader_fonts: HashMap<String, SharedString>,
    /// Font files already handed to the text system, which keeps them.
    loaded_fonts: HashSet<(usize, u64)>,
    pub reader_search: Entity<InputState>,
    pub reader_hits: Vec<Hit>,
    pub reader_hit: usize,
    pub reader_annotations: Vec<Annotation>,
    /// The block of the current section that actions apply to.
    pub reader_block: Option<usize>,
    /// A block to scroll to once the section is laid out.
    pub reader_jump: Cell<Option<usize>>,
    pub reader_panel: ReaderPanel,
    pub reader_title: String,
    pub chapter_list_visible: bool,
    pub screen: Screen,
    pub editing: bool,
    /// The format of the book open in the editor.
    pub epub_format: String,
    pub status: String,
    pub status_tone: Tone,
    pub query: String,
    pub show_shortcuts: bool,

    // Cover crop state.
    pub crop_book_id: Option<i64>,
    pub crop_source: Option<DynamicImage>,
    pub crop_preview_path: Option<PathBuf>,
    crop_preview_counter: u64,
    crop_drag: Option<CropDrag>,

    // EPUB editor state.
    pub epub_book_id: Option<i64>,
    epub_dir: Option<PathBuf>,
    pub epub_files: Vec<EpubFile>,
    pub epub_selected: Option<usize>,
    pub epub_dirty: bool,
    /// Edits written to the unpacked files but not yet saved to the library.
    pub epub_pending: bool,

    pub focus_handle: FocusHandle,
    pub list_focus: FocusHandle,
    pub reader_focus: FocusHandle,
    pub list_scroll: UniformListScrollHandle,
    pub grid_scroll: ScrollHandle,
    pub reader_scroll: ScrollHandle,
    /// Width of the cover grid from the last layout, for keyboard navigation.
    pub grid_width: Rc<Cell<f32>>,
    last_window_title: String,

    pub search: Entity<InputState>,
    pub title_input: Entity<InputState>,
    pub authors_input: Entity<InputState>,
    pub series_input: Entity<InputState>,
    pub series_index_input: Entity<InputState>,
    pub publisher_input: Entity<InputState>,
    pub tags_input: Entity<InputState>,
    pub rating_input: Entity<InputState>,
    pub languages_input: Entity<InputState>,
    pub identifiers_input: Entity<InputState>,
    pub pubdate_input: Entity<InputState>,
    pub comments_input: Entity<TextareaState>,
    pub epub_editor: Entity<TextareaState>,
    pub crop_left: Entity<InputState>,
    pub crop_top: Entity<InputState>,
    pub crop_width: Entity<InputState>,
    pub crop_height: Entity<InputState>,

    _subscriptions: Vec<Subscription>,
}

/// What the reader's side panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderPanel {
    Contents,
    Notes,
    Search,
}

impl CalibreApp {
    pub fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = text_input("Search", window, cx);
        let title_input = text_input("Title", window, cx);
        let authors_input = text_input("Authors, separated by &", window, cx);
        let series_input = text_input("Series", window, cx);
        let series_index_input = text_input("1", window, cx);
        let publisher_input = text_input("Publisher", window, cx);
        let tags_input = text_input("Tags, comma separated", window, cx);
        let rating_input = text_input("0–10", window, cx);
        let languages_input = text_input("eng, fra", window, cx);
        let identifiers_input = text_input("isbn=…, asin=…", window, cx);
        let pubdate_input = text_input("YYYY-MM-DD", window, cx);
        let comments_input = text_area("Description", 8, window, cx);
        let epub_editor = text_area("EPUB source", 20, window, cx);
        let reader_search = text_input("Find in book", window, cx);
        let crop_left = text_input("Left %", window, cx);
        let crop_top = text_input("Top %", window, cx);
        let crop_width = text_input("Width %", window, cx);
        let crop_height = text_input("Height %", window, cx);

        let subscriptions = vec![
            cx.subscribe_in(&reader_search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                    let query = state.read(cx).value().to_string();
                    let fresh = matches!(event, InputEvent::Change);
                    this.search_book(&query, fresh, cx);
                }
            }),
            cx.subscribe_in(&search, window, |this, state, event, window, cx| {
                match event {
                    InputEvent::Change => {
                        this.query = state.read(cx).value().to_string();
                        this.reload();
                        this.ensure_selection(window, cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        // Return in the search field jumps to the results.
                        window.focus(&this.list_focus, cx);
                        if this.selected.is_none() {
                            this.select_index(0, window, cx);
                        }
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&epub_editor, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::Change) && !this.epub_dirty {
                    this.epub_dirty = true;
                    window.set_window_edited(true);
                    cx.notify();
                }
            }),
            // Follow the OS light/dark appearance when asked to.
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.settings.theme == ThemePreference::System {
                    Theme::sync_system_appearance(Some(window), cx);
                }
            }),
        ];

        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |app, cx| app.window_should_close(window, cx))
                .unwrap_or(true)
        });

        let focus_handle = cx.focus_handle();
        let list_focus = cx.focus_handle();
        let reader_focus = cx.focus_handle();

        let mut app = Self {
            settings,
            job: None,
            close_after_job: None,
            library: None,
            devices: Vec::new(),
            scanning_devices: false,
            sending_to_device: false,
            all_books: Vec::new(),
            categories: Categories::default(),
            books: Vec::new(),
            shelf: Shelf::All,
            expanded_sections: HashSet::new(),
            selected: None,
            marked: HashSet::new(),
            anchor: None,
            metadata: None,
            cover_path: None,
            selected_formats: Vec::new(),
            selected_path: None,
            book: None,
            reader_book_id: 0,
            section_index: 0,
            reader_images: Vec::new(),
            reader_fonts: HashMap::new(),
            loaded_fonts: HashSet::new(),
            reader_search,
            reader_hits: Vec::new(),
            reader_hit: 0,
            reader_annotations: Vec::new(),
            reader_block: None,
            reader_jump: Cell::new(None),
            reader_panel: ReaderPanel::Contents,
            reader_title: String::new(),
            chapter_list_visible: true,
            screen: Screen::Library,
            editing: false,
            epub_format: "EPUB".to_string(),
            status: String::new(),
            status_tone: Tone::Info,
            query: String::new(),
            show_shortcuts: false,
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
            epub_pending: false,
            focus_handle,
            list_focus,
            reader_focus,
            list_scroll: UniformListScrollHandle::new(),
            grid_scroll: ScrollHandle::new(),
            reader_scroll: ScrollHandle::new(),
            grid_width: Rc::new(Cell::new(0.0)),
            last_window_title: String::new(),
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

        app.apply_theme(window, cx);

        let startup_library = app
            .settings
            .last_library
            .clone()
            .filter(|path| files::is_library(path))
            .or_else(|| Some(default_library_path()).filter(|path| files::is_library(path)));
        match startup_library {
            Some(path) => app.open_library(path, window, cx),
            None => {
                app.set_status(
                    "Open a library or create a new one to get started.",
                    Tone::Info,
                );
                app.scan_devices(window, cx);
            }
        }
        window.focus(&app.list_focus, cx);
        app
    }

    // ---------------------------------------------------------------------
    // Shared helpers
    // ---------------------------------------------------------------------

    pub fn set_status(&mut self, message: impl Into<String>, tone: Tone) {
        self.status = message.into();
        self.status_tone = tone;
    }

    /// Shows a message in the status bar and as a toast.
    fn announce(
        &mut self,
        message: impl Into<String>,
        tone: Tone,
        window: &mut Window,
        cx: &mut App,
    ) {
        let message = message.into();
        let note = match tone {
            Tone::Info => Notification::info(message.clone()),
            Tone::Success => Notification::success(message.clone()),
            Tone::Error => Notification::error(message.clone()),
        };
        window.push_notification(note, cx);
        self.set_status(message, tone);
    }

    /// Runs `body` on a background thread, showing its progress in the status
    /// bar, then hands the result to `finish` on the UI thread. One job runs
    /// at a time.
    fn run_job<T: Send + 'static>(
        &mut self,
        label: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        body: impl FnOnce(&JobContext) -> cr_core::Result<T> + Send + 'static,
        finish: impl FnOnce(&mut Self, cr_core::Result<T>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if let Some(job) = &self.job {
            let message = format!("Wait for “{}” to finish first.", job.label);
            self.announce(message, Tone::Info, window, cx);
            cx.notify();
            return;
        }
        let label = label.into();
        let mut handle = match jobs::spawn(label.clone(), body) {
            Ok(handle) => handle,
            Err(err) => {
                finish(self, Err(err), window, cx);
                cx.notify();
                return;
            }
        };
        self.job = Some(RunningJob {
            label,
            progress: Progress::default(),
            cancelling: false,
            canceller: handle.canceller(),
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            while !handle.is_finished() {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let progress = handle.progress().clone();
                let updated = this.update(cx, |this, cx| {
                    if let Some(job) = this.job.as_mut() {
                        if job.progress != progress {
                            job.progress = progress;
                            cx.notify();
                        }
                    }
                });
                if updated.is_err() {
                    handle.cancel();
                    return;
                }
            }
            let result = handle.join();
            this.update_in(cx, |this, window, cx| {
                this.job = None;
                finish(this, result, window, cx);
                cx.notify();
                match this.close_after_job.take() {
                    Some(AfterResolve::CloseWindow) => {
                        if this.window_should_close(window, cx) {
                            window.remove_window();
                        }
                    }
                    Some(AfterResolve::Quit) => this.request_quit(window, cx),
                    None => {}
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel_job(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.job.as_mut() {
            job.canceller.cancel();
            job.cancelling = true;
            cx.notify();
        }
    }

    /// The open library's folder, or a status message when it cannot be changed.
    fn writable_root(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<PathBuf> {
        let Some(library) = &self.library else {
            self.set_status("Open a library first.", Tone::Info);
            cx.notify();
            return None;
        };
        if let Some(reason) = library.read_only_reason() {
            let reason = capitalize(reason);
            self.announce(reason, Tone::Error, window, cx);
            cx.notify();
            return None;
        }
        Some(library.root().to_path_buf())
    }

    /// Moves focus into the main view when nothing in it has focus, so window
    /// actions and shortcuts reach this view.
    pub fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.focus_handle.contains_focused(window, cx) {
            let target = match self.screen {
                Screen::Reader => self.reader_focus.clone(),
                Screen::Library => self.list_focus.clone(),
                Screen::Epub | Screen::Crop => self.focus_handle.clone(),
            };
            window.focus(&target, cx);
        }
        true
    }

    fn save_settings(&mut self) {
        if let Err(err) = self.settings.save() {
            self.set_status(err, Tone::Error);
        }
    }

    /// Persists settings and rebuilds the menu bar to match them.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        self.save_settings();
        cx.set_menus(app_menus(&self.settings));
        cx.notify();
    }

    fn apply_theme(&self, window: &mut Window, cx: &mut App) {
        match self.settings.theme {
            ThemePreference::System => Theme::sync_system_appearance(Some(window), cx),
            ThemePreference::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemePreference::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
    }

    pub fn set_theme(
        &mut self,
        theme: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.theme = theme;
        self.apply_theme(window, cx);
        self.settings_changed(cx);
    }

    /// Keeps the OS window title, proxy icon and "edited" dot in sync.
    pub fn sync_window_chrome(&mut self, window: &mut Window) {
        let library = self.library_name();
        let title = match self.screen {
            Screen::Reader if !self.reader_title.is_empty() => {
                format!("{} — calibre-rust", self.reader_title)
            }
            _ if !library.is_empty() => format!("{library} — calibre-rust"),
            _ => "calibre-rust".to_string(),
        };
        if title != self.last_window_title {
            window.set_window_title(&title);
            window.set_document_path(self.library.as_ref().map(|library| library.root()));
            self.last_window_title = title;
        }
        window.set_window_edited(self.has_unsaved_changes());
    }

    pub fn library_name(&self) -> String {
        self.library
            .as_ref()
            .and_then(|library| library.root().file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.editing || self.epub_dirty || self.epub_pending
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.books.iter().position(|book| book.id == id)
    }

    pub fn selected_title(&self) -> String {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.title.clone())
            .unwrap_or_else(|| "this book".to_string())
    }

    pub fn cover_for(&self, book: &BookSummary) -> Option<PathBuf> {
        if !book.has_cover {
            return None;
        }
        let library = self.library.as_ref()?;
        Some(library.root().join(&book.path).join("cover.jpg"))
    }

    /// The file of the preferred format of a book, used for opening it with
    /// another app or dragging it out of the window.
    pub fn preferred_format_path(&self, id: i64, formats: &[String]) -> Option<PathBuf> {
        let library = self.library.as_ref()?;
        let format = PREFERRED_FORMATS
            .iter()
            .find_map(|preferred| {
                formats
                    .iter()
                    .find(|format| format.eq_ignore_ascii_case(preferred))
            })
            .or_else(|| formats.first())?;
        library.format_path(id, format).ok().flatten()
    }

    // ---------------------------------------------------------------------
    // Library list
    // ---------------------------------------------------------------------

    /// Re-reads every book (for the sidebar) and the visible list.
    fn refresh_all(&mut self) {
        let result = self
            .library
            .as_ref()
            .map(|library| library.list_books(&ListOptions::default()));
        self.all_books = match result {
            Some(Ok(books)) => books,
            Some(Err(err)) => {
                self.set_status(format!("Could not read the library: {err}"), Tone::Error);
                Vec::new()
            }
            None => Vec::new(),
        };
        self.categories = Categories::from_books(&self.all_books);
        if !self.shelf_exists(&self.shelf) {
            self.shelf = Shelf::All;
        }
        self.reload();
    }

    fn shelf_exists(&self, shelf: &Shelf) -> bool {
        let has = |entries: &[(String, usize)], name: &str| {
            entries.iter().any(|(entry, _)| entry == name)
        };
        match shelf {
            Shelf::All | Shelf::RecentlyAdded => true,
            Shelf::Author(name) => has(&self.categories.authors, name),
            Shelf::Series(name) => has(&self.categories.series, name),
            Shelf::Tag(name) => has(&self.categories.tags, name),
            Shelf::Format(name) => has(&self.categories.formats, name),
        }
    }

    /// Re-runs the search and shelf filter for the visible list.
    pub fn reload(&mut self) {
        let query = self.query.trim().to_string();
        let (sort, descending, limit) = match self.shelf {
            Shelf::RecentlyAdded => (BookSort::Date, true, Some(RECENTLY_ADDED_LIMIT)),
            _ => (self.settings.sort(), self.settings.descending, None),
        };
        let options = ListOptions {
            query: if query.is_empty() { None } else { Some(query) },
            sort: Some(sort),
            descending,
            limit,
        };
        let result = self
            .library
            .as_ref()
            .map(|library| library.list_books(&options));
        match result {
            Some(Ok(books)) => {
                let shelf = &self.shelf;
                self.books = books
                    .into_iter()
                    .filter(|book| shelf.matches(book))
                    .collect();
            }
            Some(Err(err)) => {
                self.books.clear();
                self.set_status(format!("Search failed: {err}"), Tone::Error);
            }
            None => self.books.clear(),
        }
    }

    /// Keeps a sensible selection after the list changes.
    fn ensure_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            return;
        }
        if self.selected_index().is_none() {
            match self.books.first().map(|book| book.id) {
                Some(id) => self.load_book(id, window, cx),
                None => self.clear_selection(),
            }
        }
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.marked.clear();
        self.anchor = None;
        self.metadata = None;
        self.cover_path = None;
        self.selected_formats.clear();
        self.selected_path = None;
        self.editing = false;
    }

    pub fn select_shelf(&mut self, shelf: Shelf, window: &mut Window, cx: &mut Context<Self>) {
        self.shelf = shelf;
        self.reload();
        self.ensure_selection(window, cx);
        if let Some(index) = self.selected_index() {
            self.scroll_to(index);
        }
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    pub fn toggle_section(&mut self, section: ShelfSection, cx: &mut Context<Self>) {
        if !self.expanded_sections.remove(&section) {
            self.expanded_sections.insert(section);
        }
        cx.notify();
    }

    /// Sorts by `sort`; choosing the current sort again flips the direction.
    fn set_sort(&mut self, sort: BookSort, cx: &mut Context<Self>) {
        if self.settings.sort() == sort {
            self.settings.descending = !self.settings.descending;
        } else {
            self.settings.set_sort(sort);
            // Newest or best first is the useful default for these.
            self.settings.descending =
                matches!(sort, BookSort::Date | BookSort::Pubdate | BookSort::Rating);
        }
        self.reload();
        if let Some(index) = self.selected_index() {
            self.scroll_to(index);
        }
        self.settings_changed(cx);
    }

    pub fn load_book(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.library.as_ref() else {
            return;
        };
        let metadata = match library.get_metadata(id) {
            Ok(metadata) => metadata,
            Err(err) => {
                self.set_status(format!("Could not load book #{id}: {err}"), Tone::Error);
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
    }

    pub fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.books.get(index).map(|book| book.id) else {
            return;
        };
        if self.selected != Some(id) {
            if self.editing {
                self.set_status(
                    "Save or cancel your metadata changes before choosing another book.",
                    Tone::Info,
                );
                cx.notify();
                return;
            }
            self.load_book(id, window, cx);
        }
        self.scroll_to(index);
        cx.notify();
    }

    /// A click on a book: plain selects only it, Ctrl/Cmd toggles it, Shift
    /// extends the selection from the last plain click.
    pub fn click_index(
        &mut self,
        index: usize,
        toggle: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.books.get(index).map(|book| book.id) else {
            return;
        };
        if extend {
            let anchor = self.anchor.or(self.selected_index()).unwrap_or(index);
            // The list may have shrunk since the anchor was set.
            let anchor = anchor.min(self.books.len() - 1);
            let (from, to) = (anchor.min(index), anchor.max(index));
            self.marked = self.books[from..=to].iter().map(|book| book.id).collect();
            self.anchor = Some(anchor);
        } else if toggle {
            if let Some(current) = self.selected {
                self.marked.insert(current);
            }
            if !self.marked.insert(id) {
                self.marked.remove(&id);
                // Keep the inspector on a book that is still selected.
                if self.selected == Some(id) && !self.editing {
                    let next = self.books.iter().position(|b| self.marked.contains(&b.id));
                    match next {
                        Some(next) => self.select_index(next, window, cx),
                        None => self.clear_selection(),
                    }
                }
                cx.notify();
                return;
            }
            self.anchor = Some(index);
        } else {
            self.marked.clear();
            self.anchor = Some(index);
        }
        self.select_index(index, window, cx);
        if self.selected == Some(id) {
            self.marked.insert(id);
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.marked = self.books.iter().map(|book| book.id).collect();
        cx.notify();
    }

    /// The selected books in list order.
    pub fn selection_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .books
            .iter()
            .map(|book| book.id)
            .filter(|id| self.marked.contains(id) || self.selected == Some(*id))
            .collect();
        if ids.is_empty() {
            ids.extend(self.selected);
        }
        ids
    }

    pub fn is_marked(&self, id: i64) -> bool {
        self.selected == Some(id) || self.marked.contains(&id)
    }

    fn scroll_to(&self, index: usize) {
        match self.settings.view {
            ViewMode::List => self
                .list_scroll
                .scroll_to_item(index, ScrollStrategy::Center),
            ViewMode::Grid => self.grid_scroll.scroll_to_item(index),
        }
    }

    fn grid_columns(&self) -> usize {
        let width = self.grid_width.get() - 2.0 * crate::ui::GRID_PADDING;
        let cell = crate::ui::GRID_CARD_WIDTH + crate::ui::GRID_GAP;
        (((width + crate::ui::GRID_GAP) / cell).floor().max(1.0)) as usize
    }

    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.books.is_empty() {
            return;
        }
        let last = self.books.len() as isize - 1;
        let next = match self.selected_index() {
            Some(index) => (index as isize + delta).clamp(0, last),
            None => 0,
        };
        self.click_index(next as usize, false, false, window, cx);
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

    // ---------------------------------------------------------------------
    // Opening libraries
    // ---------------------------------------------------------------------

    fn install_library(
        &mut self,
        library: Library,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discard_epub_editor();
        self.clear_crop();
        self.library = Some(library);
        self.screen = Screen::Library;
        self.shelf = Shelf::All;
        self.query.clear();
        set_input(&self.search, String::new(), window, cx);
        self.clear_selection();
        self.refresh_all();
        self.ensure_selection(window, cx);
        if let Some(index) = self.selected_index() {
            self.scroll_to(index);
        }

        self.settings.remember_library(path);
        cx.add_recent_document(path);
        self.settings_changed(cx);
        self.sync_window_chrome(window);
        window.focus(&self.list_focus, cx);
        let read_only = self
            .library
            .as_ref()
            .and_then(|library| library.read_only_reason().map(capitalize));
        match read_only {
            Some(reason) => self.announce(reason, Tone::Error, window, cx),
            None => self.set_status(
                format!(
                    "Opened {} — {} book(s).",
                    path.display(),
                    self.all_books.len()
                ),
                Tone::Info,
            ),
        }
        self.scan_devices(window, cx);
        cx.notify();
    }

    pub fn open_library(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_unsaved_changes() {
            self.set_status(
                "Save or cancel your changes before switching libraries.",
                Tone::Info,
            );
            cx.notify();
            return;
        }
        match Library::open(&path) {
            Ok(library) => self.install_library(library, &path, window, cx),
            Err(err) => {
                self.settings
                    .recent_libraries
                    .retain(|recent| recent != &path);
                self.settings_changed(cx);
                self.announce(
                    format!("Could not open {}: {err}", path.display()),
                    Tone::Error,
                    window,
                    cx,
                );
                cx.notify();
            }
        }
    }

    fn create_library_at(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if files::is_library(&path) {
            self.open_library(path, window, cx);
            return;
        }
        match Library::create(&path) {
            Ok(library) => {
                self.install_library(library, &path, window, cx);
                self.announce(
                    format!("Created a new library in {}.", path.display()),
                    Tone::Success,
                    window,
                    cx,
                );
            }
            Err(err) => {
                self.announce(
                    format!("Could not create {}: {err}", path.display()),
                    Tone::Error,
                    window,
                    cx,
                );
                cx.notify();
            }
        }
    }

    pub fn prompt_open_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Library".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            if files::is_library(&path) {
                this.update_in(cx, |this, window, cx| this.open_library(path, window, cx))
                    .ok();
                return;
            }
            // Offer to create a library in a folder that has none.
            let answer = this.update_in(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    &format!("“{}” is not a calibre library.", display_name(&path)),
                    Some("Create a new, empty library in this folder?"),
                    &["Create Library", "Cancel"],
                    cx,
                )
            });
            if let Ok(answer) = answer {
                if let Ok(0) = answer.await {
                    this.update_in(cx, |this, window, cx| {
                        this.create_library_at(path, window, cx)
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    pub fn prompt_new_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default = default_library_path();
        let home = default
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let suggested = if default.exists() {
            "New Calibre Library"
        } else {
            "Calibre Library"
        };
        let receiver = cx.prompt_for_new_path(&home, Some(suggested));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    this.create_library_at(path, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Handles files and folders from drag and drop or the OS ("Open With",
    /// the Dock icon): libraries are opened and ebooks are added.
    pub fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let dropped = files::classify(paths);
        let had_library = dropped.library.is_some();
        if let Some(library) = dropped.library {
            self.open_library(library, window, cx);
        }
        if dropped.books.is_empty() {
            if !had_library {
                self.announce(
                    "Drop ebook files or a calibre library folder.",
                    Tone::Info,
                    window,
                    cx,
                );
            }
            cx.notify();
            return;
        }
        if self.library.is_none() {
            self.announce(
                "Open a library before adding books.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        self.add_paths(dropped.books, window, cx);
    }

    // ---------------------------------------------------------------------
    // Adding, exporting and removing books
    // ---------------------------------------------------------------------

    pub fn prompt_add_books(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.is_none() {
            self.prompt_open_library(window, cx);
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Add to Library".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    let books = files::classify(paths).books;
                    if books.is_empty() {
                        this.announce("No ebook files were selected.", Tone::Info, window, cx);
                        cx.notify();
                    } else {
                        this.add_paths(books, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            self.announce(
                "Save or cancel your metadata changes before adding books.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let label = match paths.len() {
            1 => "Adding 1 book".to_string(),
            count => format!("Adding {count} books"),
        };
        self.run_job(
            label,
            window,
            cx,
            move |context| {
                // Another format of a book already here joins that record.
                let options = ImportOptions {
                    duplicates: DuplicatePolicy::Merge,
                    ..Default::default()
                };
                Ok(Library::open(&root)?.import_books(&paths, &options, context))
            },
            |this, result, window, cx| {
                let results = match result {
                    Ok(results) => results,
                    Err(err) => {
                        this.announce(format!("Add failed: {err}"), Tone::Error, window, cx);
                        return;
                    }
                };
                let last_id = results
                    .iter()
                    .rev()
                    .find_map(|item| item.result.as_ref().ok().and_then(AddOutcome::book_id));
                let count = |wanted: fn(&AddOutcome) -> bool| {
                    results
                        .iter()
                        .filter(|item| item.result.as_ref().is_ok_and(wanted))
                        .count()
                };
                let added = count(|outcome| matches!(outcome, AddOutcome::Added(_)));
                let merged =
                    count(|outcome| matches!(outcome, AddOutcome::Merged { added: true, .. }));
                let present =
                    count(|outcome| matches!(outcome, AddOutcome::Merged { added: false, .. }));
                let mut summary = match added {
                    1 => "Added 1 book".to_string(),
                    _ => format!("Added {added} books"),
                };
                if merged > 0 {
                    summary.push_str(&format!("; {merged} added as formats of existing books"));
                }
                if present > 0 {
                    summary.push_str(&format!("; {present} already in the library"));
                }
                let errors: Vec<String> = results
                    .iter()
                    .filter_map(|item| {
                        let err = item.result.as_ref().err()?;
                        Some(format!("{}: {err}", display_name(&item.item)))
                    })
                    .collect();
                // Show the new books: clear filters that might hide them.
                this.shelf = Shelf::All;
                this.query.clear();
                set_input(&this.search, String::new(), window, cx);
                this.refresh_all();
                if let Some(id) = last_id {
                    this.load_book(id, window, cx);
                    if let Some(index) = this.selected_index() {
                        this.scroll_to(index);
                    }
                }
                let (message, tone) = match (added + merged + present, errors.last()) {
                    (0, Some(err)) => (format!("No books added. {err}"), Tone::Error),
                    (_, Some(err)) => (
                        format!("{summary}; {} failed. Last error: {err}", errors.len()),
                        Tone::Error,
                    ),
                    (0, None) => ("Adding books was cancelled.".to_string(), Tone::Info),
                    (_, None) => (format!("{summary}."), Tone::Success),
                };
                this.announce(message, tone, window, cx);
            },
        );
    }

    fn prompt_export(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            self.set_status("There are no books to export.", Tone::Info);
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Export Here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(destination) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| {
                        this.export_books(ids, destination, window, cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn export_books(
        &mut self,
        ids: Vec<i64>,
        destination: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.library.as_ref().map(|l| l.root().to_path_buf()) else {
            self.set_status("Open a library first.", Tone::Info);
            cx.notify();
            return;
        };
        let book_count = ids.len();
        let target = destination.clone();
        self.run_job(
            format!("Exporting {book_count} book(s)"),
            window,
            cx,
            move |context| {
                let library = Library::open(&root)?;
                let options = ExportOptions {
                    template: EXPORT_TEMPLATE.to_string(),
                    ..Default::default()
                };
                Ok(library.export_with(&ids, &target, &options, context))
            },
            move |this, result, window, cx| {
                let results = match result {
                    Ok(results) => results,
                    Err(err) => {
                        this.announce(format!("Export failed: {err}"), Tone::Error, window, cx);
                        return;
                    }
                };
                let files: usize = results
                    .iter()
                    .filter_map(|item| item.result.as_ref().ok().map(Vec::len))
                    .sum();
                let errors: Vec<String> = results
                    .iter()
                    .filter_map(|item| {
                        let err = item.result.as_ref().err()?;
                        Some(format!("book {}: {err}", item.item))
                    })
                    .collect();
                match (files, errors.last()) {
                    (_, Some(err)) => this.announce(
                        format!(
                            "Exported {files} file(s); {} book(s) failed. Last error: {err}",
                            errors.len()
                        ),
                        Tone::Error,
                        window,
                        cx,
                    ),
                    (0, None) => this.announce(
                        "Nothing exported: these books have no format files.",
                        Tone::Info,
                        window,
                        cx,
                    ),
                    (_, None) => this.announce(
                        format!(
                            "Exported {files} file(s) from {} book(s) to {}.",
                            results.len(),
                            destination.display()
                        ),
                        Tone::Success,
                        window,
                        cx,
                    ),
                }
                if files > 0 {
                    cx.reveal_path(&destination);
                }
            },
        );
    }

    fn confirm_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selection_ids();
        if ids.is_empty() {
            return;
        }
        let question = match ids.len() {
            1 => format!("Remove “{}” from the library?", self.selected_title()),
            count => format!("Remove {count} books from the library?"),
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            &question,
            Some(
                "Removed books are kept in the library's trash folder and can be \
                 restored with `cr trash restore`.",
            ),
            &["Remove", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(0) = receiver.await {
                this.update_in(cx, |this, window, cx| this.remove_books(ids, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn remove_books(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.selected_index();
        let title = self.selected_title();
        let Some(library) = self.library.as_mut() else {
            self.set_status("Open a library first.", Tone::Info);
            cx.notify();
            return;
        };
        let mut removed = 0usize;
        let mut errors = Vec::new();
        for &id in &ids {
            match library.trash_book(id) {
                Ok(()) => removed += 1,
                Err(err) => errors.push(format!("book {id}: {err}")),
            }
        }
        if removed > 0 {
            self.clear_selection();
            self.refresh_all();
            // Select the neighbour so the keyboard keeps working.
            if let Some(index) = index {
                let next = index.min(self.books.len().saturating_sub(1));
                self.click_index(next, false, false, window, cx);
            }
        }
        match (removed, errors.last()) {
            (_, Some(err)) => self.announce(
                format!(
                    "Removed {removed} book(s); {} failed. Last error: {err}",
                    errors.len()
                ),
                Tone::Error,
                window,
                cx,
            ),
            (1, None) if ids.len() == 1 => {
                self.announce(format!("Removed “{title}”."), Tone::Success, window, cx)
            }
            (_, None) => self.announce(
                format!("Removed {removed} books."),
                Tone::Success,
                window,
                cx,
            ),
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // OS integration for the selected book
    // ---------------------------------------------------------------------

    fn open_selected_externally(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        match self.preferred_format_path(id, &self.selected_formats) {
            Some(path) => {
                cx.open_with_system(&path);
                self.set_status(
                    format!("Opened {} in its default app.", display_name(&path)),
                    Tone::Info,
                );
            }
            None => self.announce("This book has no files to open.", Tone::Info, window, cx),
        }
        cx.notify();
    }

    pub fn open_format_externally(&mut self, format: &str, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let path = self
            .library
            .as_ref()
            .and_then(|library| library.format_path(id, format).ok().flatten());
        match path {
            Some(path) => {
                cx.open_with_system(&path);
                self.set_status(format!("Opened {}.", display_name(&path)), Tone::Info);
            }
            None => self.set_status(format!("No {format} file was found."), Tone::Error),
        }
        cx.notify();
    }

    pub fn save_conversion(
        &mut self,
        source: &str,
        extension: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((library, id)) = self.library.as_ref().zip(self.selected) else {
            return;
        };
        let input = match library.format_path(id, source) {
            Ok(Some(path)) => path,
            Ok(None) => {
                self.announce(
                    format!("No {source} file was found."),
                    Tone::Error,
                    window,
                    cx,
                );
                return;
            }
            Err(err) => {
                self.announce(err.to_string(), Tone::Error, window, cx);
                return;
            }
        };
        let default = default_library_path();
        let home = default.parent().unwrap_or_else(|| Path::new("."));
        let suggested = format!(
            "{}.{extension}",
            text::sanitize_component(&self.selected_title())
        );
        let receiver = cx.prompt_for_new_path(home, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut output))) = receiver.await else {
                return;
            };
            if output.extension().is_none() {
                output.set_extension(extension);
            }
            if !output
                .extension()
                .and_then(|suffix| suffix.to_str())
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
            {
                this.update_in(cx, |this, window, cx| {
                    this.announce(
                        format!("Choose a .{extension} filename."),
                        Tone::Error,
                        window,
                        cx,
                    );
                })
                .ok();
                return;
            }
            let result = cx
                .background_spawn(async move {
                    cr_core::formats::convert(&input, &output).map(|()| output)
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(output) => this.announce(
                    format!("Saved {}.", display_name(&output)),
                    Tone::Success,
                    window,
                    cx,
                ),
                Err(err) => {
                    this.announce(format!("Conversion failed: {err}"), Tone::Error, window, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn reveal_selected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let file = self.preferred_format_path(id, &self.selected_formats);
        let folder = self.library.as_ref().and_then(|library| {
            self.selected_path
                .as_ref()
                .map(|relative| library.root().join(relative))
        });
        match file.or(folder) {
            Some(path) => cx.reveal_path(&path),
            None => self.set_status("This book has no files on disk.", Tone::Error),
        }
        cx.notify();
    }

    fn copy_book_info(&mut self, cx: &mut Context<Self>) {
        let Some(metadata) = &self.metadata else {
            return;
        };
        let text = if metadata.authors.is_empty() {
            metadata.title.clone()
        } else {
            format!("{} — {}", metadata.title, metadata.authors.join(" & "))
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status("Copied title and author.", Tone::Info);
        cx.notify();
    }

    pub fn set_cover_from_file(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.selected else {
            return;
        };
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(err) => {
                self.announce(
                    format!("Could not read cover: {err}"),
                    Tone::Error,
                    window,
                    cx,
                );
                cx.notify();
                return;
            }
        };
        if let Err(err) = image::load_from_memory(&data) {
            self.announce(
                format!("Unsupported cover image: {err}"),
                Tone::Error,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        let result = self
            .library
            .as_mut()
            .map(|library| library.set_cover(id, Some(&data)));
        match result {
            Some(Ok(())) => {
                self.load_book(id, window, cx);
                self.refresh_all();
                self.announce("Cover updated.", Tone::Success, window, cx);
            }
            Some(Err(err)) => self.announce(
                format!("Cover update failed: {err}"),
                Tone::Error,
                window,
                cx,
            ),
            None => self.set_status("Open a library first.", Tone::Info),
        }
        cx.notify();
    }

    fn prompt_cover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() || self.editing {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use as Cover".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update_in(cx, |this, window, cx| {
                        this.set_cover_from_file(&path, window, cx)
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Devices
    // ---------------------------------------------------------------------

    /// Scans for e-readers off the UI thread: USB (MTP) discovery can block.
    pub fn scan_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scanning_devices {
            return;
        }
        self.scanning_devices = true;
        cx.notify();
        let scan = cx.background_spawn(async move { device::discover() });
        cx.spawn_in(window, async move |this, cx| {
            let result = scan.await;
            this.update_in(cx, |this, _, cx| {
                this.scanning_devices = false;
                match result {
                    Ok(devices) => this.devices = devices,
                    Err(err) => this.set_status(format!("Device scan failed: {err}"), Tone::Error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rescan_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_status("Looking for e-readers…", Tone::Info);
        self.scan_devices(window, cx);
    }

    pub fn send_selected(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending_to_device {
            return;
        }
        let ids = self.selection_ids();
        if ids.is_empty() {
            return;
        }
        let Some(device) = self.devices.get(index).cloned() else {
            self.announce(
                "No e-reader is connected. Connect one in USB file-transfer mode and rescan.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        };
        let Some(library) = &self.library else {
            self.set_status("Open a library first.", Tone::Info);
            cx.notify();
            return;
        };
        // The transfer runs on a background thread with its own connection.
        let path = library.root().to_path_buf();
        let device_name = device.name.clone();
        self.sending_to_device = true;
        self.set_status(format!("Sending to {device_name}…"), Tone::Info);
        cx.notify();
        let transfer = cx.background_spawn(async move {
            let library = Library::open(&path)?;
            device::send(&library, &device, &ids, &SendOptions::default())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = transfer.await;
            this.update_in(cx, |this, window, cx| {
                this.sending_to_device = false;
                match result {
                    Ok(report) => match (report.sent.first(), report.skipped.first()) {
                        (Some(_), _) if report.sent.len() + report.skipped.len() > 1 => this
                            .announce(
                                format!(
                                    "Sent {} book(s) to {device_name}; {} skipped.",
                                    report.sent.len(),
                                    report.skipped.len()
                                ),
                                if report.skipped.is_empty() {
                                    Tone::Success
                                } else {
                                    Tone::Info
                                },
                                window,
                                cx,
                            ),
                        (Some(sent), _) => this.announce(
                            format!(
                                "Sent “{}” ({}) to {device_name}.",
                                sent.title,
                                format_size(sent.bytes)
                            ),
                            Tone::Success,
                            window,
                            cx,
                        ),
                        (_, Some((_, reason))) => {
                            this.announce(format!("Nothing sent: {reason}"), Tone::Info, window, cx)
                        }
                        _ => this.set_status("Nothing sent.", Tone::Info),
                    },
                    Err(err) => {
                        this.announce(format!("Send failed: {err}"), Tone::Error, window, cx)
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Metadata editing
    // ---------------------------------------------------------------------

    pub fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Library || self.editing {
            return;
        }
        if let Some(metadata) = self.metadata.clone() {
            self.fill_form(&metadata, window, cx);
            self.editing = true;
            if !self.settings.inspector_visible {
                self.settings.inspector_visible = true;
                self.settings_changed(cx);
            }
            self.title_input
                .update(cx, |input, cx| input.focus(window, cx));
            self.set_status("Editing metadata. ⌘S saves, Esc cancels.", Tone::Info);
            window.set_window_edited(true);
            cx.notify();
        }
    }

    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(metadata) = self.metadata.clone() {
            self.fill_form(&metadata, window, cx);
        }
        self.editing = false;
        self.set_status("Changes discarded.", Tone::Info);
        window.set_window_edited(self.has_unsaved_changes());
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    /// Saves the metadata form. Returns whether the save succeeded.
    pub fn save_metadata(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let update = match self.read_form(cx) {
            Ok(update) => update,
            Err(err) => {
                self.announce(err, Tone::Error, window, cx);
                cx.notify();
                return false;
            }
        };
        let Some(library) = self.library.as_mut() else {
            self.set_status("Open a library first.", Tone::Info);
            cx.notify();
            return false;
        };
        let saved = match library.update_book(id, &update) {
            Ok(()) => {
                self.load_book(id, window, cx);
                self.refresh_all();
                if let Some(index) = self.selected_index() {
                    self.scroll_to(index);
                }
                self.set_status("Metadata saved.", Tone::Success);
                window.focus(&self.list_focus, cx);
                true
            }
            Err(err) => {
                self.announce(format!("Save failed: {err}"), Tone::Error, window, cx);
                false
            }
        };
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
        saved
    }

    fn read_form(&self, cx: &App) -> Result<BookUpdate, String> {
        let get = |input: &Entity<InputState>| input.read(cx).value().to_string();

        let title = get(&self.title_input).trim().to_string();
        if title.is_empty() {
            return Err("Title cannot be empty.".to_string());
        }
        let authors = split_authors(&get(&self.authors_input));
        let series = get(&self.series_input).trim().to_string();
        let series_index = text::parse_series_index(&get(&self.series_index_input))
            .ok_or("Series index must be a number.")?;
        let publisher = get(&self.publisher_input).trim().to_string();
        let tags = split_commas(&get(&self.tags_input));
        let rating_raw = get(&self.rating_input).trim().to_string();
        let rating = if rating_raw.is_empty() {
            None
        } else {
            Some(
                rating_raw
                    .parse::<u8>()
                    .map_err(|_| "Rating must be a number from 0 to 10.")?
                    .min(10),
            )
        };
        let languages = split_commas(&get(&self.languages_input));
        let identifiers = parse_identifiers(&get(&self.identifiers_input))?;
        let pubdate_raw = get(&self.pubdate_input).trim().to_string();
        let pubdate = if pubdate_raw.is_empty() {
            None
        } else {
            Some(
                parse_user_date(&pubdate_raw)
                    .ok_or("Publication date must look like YYYY-MM-DD.")?,
            )
        };
        let comments_raw = self.comments_input.read(cx).value().to_string();
        let comments = if comments_raw.trim().is_empty() {
            None
        } else {
            Some(htmlize_comments(&comments_raw))
        };

        Ok(BookUpdate {
            title: Some(title),
            authors: Some(authors),
            series: Some(Some(series).filter(|series| !series.is_empty())),
            series_index: Some(series_index),
            publisher: Some(Some(publisher).filter(|publisher| !publisher.is_empty())),
            tags: Some(tags),
            rating: Some(rating),
            comments: Some(comments),
            identifiers: Some(identifiers),
            languages: Some(languages),
            pubdate: Some(pubdate),
            ..Default::default()
        })
    }

    // ---------------------------------------------------------------------
    // Reader
    // ---------------------------------------------------------------------

    pub fn start_reading(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if self.editing {
            return;
        }
        let Some(library) = self.library.as_ref() else {
            self.set_status("Open a library first.", Tone::Info);
            return;
        };
        let book = match library.open_book(id, None) {
            Ok(book)
                if book
                    .sections
                    .iter()
                    .any(|section| !section.blocks.is_empty()) =>
            {
                book
            }
            Ok(_) => {
                self.announce("No readable text in this file.", Tone::Info, window, cx);
                return;
            }
            Err(err) => {
                self.announce(
                    format!("Cannot read this book: {err}"),
                    Tone::Error,
                    window,
                    cx,
                );
                return;
            }
        };
        // Annotations follow their text if the file changed since.
        let mut problem = None;
        if library.read_only_reason().is_none() {
            if let Err(err) = library.relocate_annotations(id, &book) {
                problem = Some(format!("Could not update your notes: {err}"));
            }
        }
        self.reader_annotations = library.annotations(Some(id)).unwrap_or_else(|err| {
            problem = Some(format!("Could not load your notes: {err}"));
            Vec::new()
        });
        let position = library.reading_position(id, &book).unwrap_or_else(|err| {
            problem = Some(format!("Could not load your place: {err}"));
            None
        });
        self.reader_images = book
            .images
            .iter()
            .map(|image| {
                let format = match image.data.as_slice() {
                    [0xff, 0xd8, ..] => ImageFormat::Jpeg,
                    [0x89, b'P', ..] => ImageFormat::Png,
                    [b'G', b'I', b'F', ..] => ImageFormat::Gif,
                    [b'R', b'I', b'F', b'F', ..] => ImageFormat::Webp,
                    _ if image.name.to_ascii_lowercase().ends_with(".svg") => ImageFormat::Svg,
                    _ => return None,
                };
                Some(std::sync::Arc::new(Image::from_bytes(
                    format,
                    image.data.clone(),
                )))
            })
            .collect();
        self.reader_fonts.clear();
        let mut plain: HashSet<&str> = HashSet::new();
        for font in &book.fonts {
            let Some(name) = cr_core::formats::css::font_family_name(&font.data) else {
                continue;
            };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            font.data.hash(&mut hasher);
            if self.loaded_fonts.insert((font.data.len(), hasher.finish()))
                && cx
                    .text_system()
                    .add_fonts(vec![font.data.clone().into()])
                    .is_err()
            {
                continue;
            }
            // The upright face names the family when faces disagree.
            let upright = !font.bold && !font.italic;
            if upright || !plain.contains(font.family.as_str()) {
                self.reader_fonts.insert(font.family.clone(), name.into());
            }
            if upright {
                plain.insert(&font.family);
            }
        }
        self.reader_title = book.title.clone();
        let sections = book.sections.len();
        self.book = Some(Rc::new(book));
        self.reader_book_id = id;
        self.reader_hits.clear();
        self.reader_hit = 0;
        self.screen = Screen::Reader;
        self.go_to(
            position
                .map(|position| position.locator)
                .unwrap_or_default(),
            cx,
        );
        window.focus(&self.reader_focus, cx);
        match problem {
            Some(problem) => self.set_status(problem, Tone::Error),
            None => self.set_status(
                match position {
                    Some(position) => format!("Resumed at {:.0}%.", position.fraction * 100.0),
                    None => format!("Reading {sections} section(s)."),
                },
                Tone::Info,
            ),
        }
        self.sync_window_chrome(window);
        cx.notify();
    }

    /// The block at the top of the reader, as a place in the book.
    pub fn reader_locator(&self) -> Locator {
        let blocks = self
            .book
            .as_ref()
            .and_then(|book| book.sections.get(self.section_index))
            .map_or(0, |section| section.blocks.len());
        Locator {
            section: self.section_index,
            block: self.reader_scroll.top_item().min(blocks.saturating_sub(1)),
            offset: 0,
        }
    }

    fn save_reading_position(&mut self) {
        if self.screen == Screen::Reader {
            self.save_reading_position_at(self.reader_locator());
        }
    }

    fn save_reading_position_at(&mut self, locator: Locator) {
        let (Some(library), Some(book)) = (self.library.as_ref(), self.book.as_ref()) else {
            return;
        };
        if library.read_only_reason().is_some() {
            return;
        }
        if let Err(err) = library.set_reading_position(self.reader_book_id, book, locator) {
            self.set_status(format!("Could not save your place: {err}"), Tone::Error);
        }
    }

    pub fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_reading_position();
        self.book = None;
        self.reader_images.clear();
        self.reader_fonts.clear();
        self.reader_annotations.clear();
        self.reader_hits.clear();
        self.reader_block = None;
        self.screen = Screen::Library;
        self.reader_title.clear();
        self.sync_window_chrome(window);
        window.focus(&self.list_focus, cx);
        if let Some(index) = self.selected_index() {
            self.scroll_to(index);
        }
        cx.notify();
    }

    /// Show a place in the book.
    pub fn go_to(&mut self, locator: Locator, cx: &mut Context<Self>) {
        let Some(book) = self.book.as_ref() else {
            return;
        };
        if locator.section >= book.sections.len() {
            return;
        }
        if locator.section != self.section_index {
            self.reader_block = None;
        }
        self.section_index = locator.section;
        self.reader_scroll.set_offset(point(px(0.), px(0.)));
        self.reader_jump
            .set((locator.block > 0).then_some(locator.block));
        self.save_reading_position_at(locator);
        cx.notify();
    }

    pub fn step_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        let next = self.section_index as isize + delta;
        if next >= 0 {
            self.go_to(
                Locator {
                    section: next as usize,
                    ..Locator::default()
                },
                cx,
            );
        }
    }

    /// Follow a link: inside the book, or in the browser.
    pub fn follow_link(&mut self, href: &str, cx: &mut Context<Self>) {
        let target = self.book.as_ref().and_then(|book| book.resolve_link(href));
        match target {
            Some(locator) => self.go_to(locator, cx),
            None if href.starts_with("http://")
                || href.starts_with("https://")
                || href.starts_with("mailto:") =>
            {
                cx.open_url(href)
            }
            None => {}
        }
    }

    /// Search the book; Return in the field moves to the next match.
    fn search_book(&mut self, query: &str, fresh: bool, cx: &mut Context<Self>) {
        let Some(book) = self.book.clone() else {
            return;
        };
        if fresh {
            self.reader_hits = book.search(query, 500);
            self.reader_hit = 0;
            self.reader_panel = ReaderPanel::Search;
            cx.notify();
        } else {
            self.step_hit(1, cx);
        }
    }

    pub fn step_hit(&mut self, delta: isize, cx: &mut Context<Self>) {
        let total = self.reader_hits.len() as isize;
        if total > 0 {
            let next = (self.reader_hit as isize + delta).rem_euclid(total) as usize;
            self.show_hit(next, cx);
        }
    }

    pub fn show_hit(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(hit) = self.reader_hits.get(index) {
            let start = hit.start;
            self.reader_hit = index;
            self.go_to(start, cx);
            self.reader_block = Some(start.block);
        }
    }

    pub fn select_reader_block(&mut self, block: usize, cx: &mut Context<Self>) {
        self.reader_block = (self.reader_block != Some(block)).then_some(block);
        cx.notify();
    }

    /// Bookmark or highlight the selected block (the block at the top of the
    /// page when none is selected). An existing one of that kind is removed.
    pub fn annotate(&mut self, highlight: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(library), Some(book)) = (self.library.as_ref(), self.book.clone()) else {
            return;
        };
        if library.read_only_reason().is_some() {
            self.announce("This library is read-only.", Tone::Info, window, cx);
            return;
        }
        let mut start = self.reader_locator();
        if let Some(block) = self.reader_block {
            start.block = block;
        }
        let kind = if highlight {
            AnnotationKind::Highlight
        } else {
            AnnotationKind::Bookmark
        };
        let existing = self
            .reader_annotations
            .iter()
            .find(|a| {
                a.kind == kind && a.start.section == start.section && a.start.block == start.block
            })
            .map(|a| a.id);
        let result = match existing {
            Some(id) => library.remove_annotation(id),
            None => {
                let end = highlight.then(|| book.block(start).map_or(0, |block| block.text.len()));
                library
                    .add_annotation(self.reader_book_id, &book, start, end, "")
                    .map(|_| ())
            }
        };
        self.reader_annotations = library
            .annotations(Some(self.reader_book_id))
            .unwrap_or_default();
        if let Err(err) = result {
            self.announce(format!("Could not save: {err}"), Tone::Error, window, cx);
        }
        cx.notify();
    }

    pub fn remove_annotation(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Some(library) = self.library.as_ref() {
            let result = library.remove_annotation(id);
            self.reader_annotations = library
                .annotations(Some(self.reader_book_id))
                .unwrap_or_default();
            if let Err(err) = result {
                self.set_status(format!("Could not remove it: {err}"), Tone::Error);
            }
        }
        cx.notify();
    }

    fn change_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = if delta == 0.0 {
            DEFAULT_READER_FONT_SIZE
        } else {
            (self.settings.reader_font_size + delta)
                .clamp(MIN_READER_FONT_SIZE, MAX_READER_FONT_SIZE)
        };
        if size != self.settings.reader_font_size {
            self.settings.reader_font_size = size;
            self.save_settings();
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Cover cropping
    // ---------------------------------------------------------------------

    pub fn begin_cover_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if self.editing || self.screen != Screen::Library {
            return;
        }
        let Some(path) = self.cover_path.clone() else {
            self.announce(
                "This book has no cover to crop. Choose a cover first.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        };
        let loaded = std::fs::read(&path)
            .map_err(|err| format!("Could not read cover: {err}"))
            .and_then(|bytes| {
                image::load_from_memory(&bytes)
                    .map_err(|err| format!("Unsupported cover image: {err}"))
            });
        let source = match loaded {
            Ok(source) => source,
            Err(err) => {
                self.announce(err, Tone::Error, window, cx);
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
        self.screen = Screen::Crop;
        window.focus(&self.focus_handle, cx);
        match self.update_crop_preview(cx) {
            Ok(()) => self.set_status(
                "Drag the frame or pick a preset, then save the cover.",
                Tone::Info,
            ),
            Err(err) => self.set_status(err, Tone::Error),
        }
        cx.notify();
    }

    pub fn update_crop_preview(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let data = {
            let Some(source) = self.crop_source.as_ref() else {
                return Err("No cover is loaded for cropping.".to_string());
            };
            let (left, top, width, height) = self.crop_percentages(cx)?;
            let (x, y, w, h) =
                crop_pixels(source.width(), source.height(), left, top, width, height);
            encode_jpeg(&source.crop_imm(x, y, w, h), 90)?
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

    pub fn crop_percentages(&self, cx: &App) -> Result<(f32, f32, f32, f32), String> {
        let value = |input: &Entity<InputState>, name: &str| -> Result<f32, String> {
            input
                .read(cx)
                .value()
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{name} must be a number from 0 to 100."))
        };
        let left = value(&self.crop_left, "Left")?;
        let top = value(&self.crop_top, "Top")?;
        let width = value(&self.crop_width, "Width")?;
        let height = value(&self.crop_height, "Height")?;
        if !(left.is_finite() && top.is_finite() && width.is_finite() && height.is_finite()) {
            return Err("Crop values must be finite numbers.".to_string());
        }
        let left = left.clamp(0.0, 99.0);
        let top = top.clamp(0.0, 99.0);
        let width = width.clamp(1.0, 100.0 - left);
        let height = height.clamp(1.0, 100.0 - top);
        Ok((left, top, width, height))
    }

    pub fn crop_view_dims(&self, cx: &App) -> (f32, f32) {
        let Some(source) = self.crop_source.as_ref() else {
            return (220., 330.);
        };
        let (_, _, width, height) = self
            .crop_percentages(cx)
            .unwrap_or((0.0, 0.0, 100.0, 100.0));
        fit_dims(
            source.width() as f32 * width / 100.0,
            source.height() as f32 * height / 100.0,
            260.,
            400.,
        )
    }

    pub fn start_crop_drag(
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
                self.set_status(err, Tone::Error);
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

    pub fn drag_crop(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.crop_drag else {
            return;
        };
        let delta_x = (position.x.as_f32() - drag.start_x) / drag.display_width * 100.0;
        let delta_y = (position.y.as_f32() - drag.start_y) / drag.display_height * 100.0;
        let (mut left, mut top, mut width, mut height) =
            (drag.left, drag.top, drag.width, drag.height);
        let right = drag.left + drag.width;
        let bottom = drag.top + drag.height;
        let moves_left = matches!(
            drag.mode,
            CropDragMode::Left | CropDragMode::TopLeft | CropDragMode::BottomLeft
        );
        let moves_right = matches!(
            drag.mode,
            CropDragMode::Right | CropDragMode::TopRight | CropDragMode::BottomRight
        );
        let moves_top = matches!(
            drag.mode,
            CropDragMode::Top | CropDragMode::TopLeft | CropDragMode::TopRight
        );
        let moves_bottom = matches!(
            drag.mode,
            CropDragMode::Bottom | CropDragMode::BottomLeft | CropDragMode::BottomRight
        );

        if drag.mode == CropDragMode::Move {
            left = (drag.left + delta_x).clamp(0.0, 100.0 - drag.width);
            top = (drag.top + delta_y).clamp(0.0, 100.0 - drag.height);
        }
        if moves_left {
            left = (drag.left + delta_x).clamp(0.0, right - 1.0);
            width = right - left;
        }
        if moves_right {
            width = (drag.width + delta_x).clamp(1.0, 100.0 - drag.left);
        }
        if moves_top {
            top = (drag.top + delta_y).clamp(0.0, bottom - 1.0);
            height = bottom - top;
        }
        if moves_bottom {
            height = (drag.height + delta_y).clamp(1.0, 100.0 - drag.top);
        }

        set_input(&self.crop_left, format!("{left:.2}"), window, cx);
        set_input(&self.crop_top, format!("{top:.2}"), window, cx);
        set_input(&self.crop_width, format!("{width:.2}"), window, cx);
        set_input(&self.crop_height, format!("{height:.2}"), window, cx);
        cx.notify();
    }

    pub fn end_crop_drag(&mut self, cx: &mut Context<Self>) {
        if self.crop_drag.take().is_none() {
            return;
        }
        if let Err(err) = self.update_crop_preview(cx) {
            self.set_status(err, Tone::Error);
        }
        cx.notify();
    }

    pub fn apply_crop_preset(
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
        if let Err(err) = self.update_crop_preview(cx) {
            self.set_status(err, Tone::Error);
        }
        cx.notify();
    }

    pub fn apply_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.crop_book_id.or(self.selected) else {
            return;
        };
        let encoded = match self.crop_source.as_ref() {
            None => Err("No cover is loaded for cropping.".to_string()),
            Some(source) => self
                .crop_percentages(cx)
                .and_then(|(left, top, width, height)| {
                    let (x, y, w, h) =
                        crop_pixels(source.width(), source.height(), left, top, width, height);
                    encode_jpeg(&source.crop_imm(x, y, w, h), 92)
                }),
        };
        let data = match encoded {
            Ok(data) => data,
            Err(err) => {
                self.announce(err, Tone::Error, window, cx);
                cx.notify();
                return;
            }
        };
        let result = self
            .library
            .as_mut()
            .map(|library| library.set_cover(id, Some(&data)));
        match result {
            Some(Ok(())) => {
                self.clear_crop();
                self.screen = Screen::Library;
                self.load_book(id, window, cx);
                self.refresh_all();
                window.focus(&self.list_focus, cx);
                self.announce("Cover cropped and saved.", Tone::Success, window, cx);
            }
            Some(Err(err)) => {
                self.announce(format!("Cover crop failed: {err}"), Tone::Error, window, cx)
            }
            None => self.set_status("Open a library first.", Tone::Info),
        }
        cx.notify();
    }

    pub fn cancel_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_crop();
        self.screen = Screen::Library;
        window.focus(&self.list_focus, cx);
        self.set_status("Cover crop cancelled.", Tone::Info);
        cx.notify();
    }

    fn clear_crop(&mut self) {
        if let Some(path) = self.crop_preview_path.take() {
            let _ = std::fs::remove_file(path);
        }
        self.crop_book_id = None;
        self.crop_source = None;
        self.crop_drag = None;
        if self.screen == Screen::Crop {
            self.screen = Screen::Library;
        }
    }

    // ---------------------------------------------------------------------
    // EPUB editor
    // ---------------------------------------------------------------------

    pub fn begin_epub_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if self.editing || self.screen != Screen::Library {
            return;
        }
        let Some(format) = editable_format(&self.selected_formats) else {
            self.announce(
                "This book has no EPUB, KEPUB or AZW3 file to edit.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        };
        let dir = std::env::temp_dir().join(format!("calibre-rust-edit-{id}-{}", temp_stamp()));
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.announce(
                format!("Could not create editor folder: {err}"),
                Tone::Error,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        let unpacked = self
            .library
            .as_ref()
            .map(|library| library.unpack_book(id, format, &dir));
        if let Some(Err(err)) = unpacked {
            let _ = std::fs::remove_dir_all(&dir);
            self.announce(
                format!("Could not unpack the {format} file: {err}"),
                Tone::Error,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        let files = scan_epub_files(&dir);
        if files.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            self.announce(
                "This book has no editable text files.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        self.epub_book_id = Some(id);
        self.epub_format = format.to_string();
        self.epub_dir = Some(dir);
        self.epub_files = files;
        self.epub_selected = None;
        self.epub_dirty = false;
        self.epub_pending = false;
        self.screen = Screen::Epub;
        window.focus(&self.focus_handle, cx);
        // Start on the first content document rather than META-INF.
        let first = self
            .epub_files
            .iter()
            .position(|file| file.relative.ends_with(".xhtml") || file.relative.ends_with(".html"))
            .unwrap_or(0);
        self.load_epub_file(first, window, cx);
        cx.notify();
    }

    pub fn load_epub_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.epub_selected == Some(index) {
            return;
        }
        if let Err(err) = self.save_current_epub_file(cx) {
            self.set_status(err, Tone::Error);
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
                self.set_status(format!("Editing {}.", file.relative), Tone::Info);
            }
            Err(err) => self.set_status(
                format!("Could not read {}: {err}", file.relative),
                Tone::Error,
            ),
        }
        window.set_window_edited(self.has_unsaved_changes());
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
        std::fs::write(&file.path, &text)
            .map_err(|err| format!("Could not save {}: {err}", file.relative))?;
        if let Some(entry) = self.epub_files.get_mut(index) {
            entry.size = text.len() as u64;
        }
        self.epub_dirty = false;
        self.epub_pending = true;
        Ok(())
    }

    pub fn save_epub_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .epub_selected
            .and_then(|index| self.epub_files.get(index))
            .map(|file| file.relative.clone())
            .unwrap_or_default();
        match self.save_current_epub_file(cx) {
            Ok(()) => self.set_status(
                format!("Saved {name}. Repack to write the changes into the library."),
                Tone::Success,
            ),
            Err(err) => self.set_status(err, Tone::Error),
        }
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
    }

    /// Repacks the EPUB into the library. Returns whether it succeeded.
    pub fn repack_epub(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(id), Some(dir)) = (self.epub_book_id, self.epub_dir.clone()) else {
            return false;
        };
        if let Err(err) = self.save_current_epub_file(cx) {
            self.announce(err, Tone::Error, window, cx);
            cx.notify();
            return false;
        }
        // Checked first, written atomically, and the first save keeps the
        // old file as ORIGINAL_<FORMAT>.
        let format = self.epub_format.clone();
        let outcome = match self.library.as_mut() {
            Some(library) => library
                .save_edited_format(id, &format, &dir, false)
                .map_err(|err| format!("Could not save the {format} file: {err}")),
            None => Err("Open a library first.".to_string()),
        };
        let ok = match outcome {
            Ok(SaveOutcome::Invalid { errors }) => {
                let first = errors
                    .first()
                    .map(|issue| format!("{}: {}", issue.file, issue.message))
                    .unwrap_or_default();
                self.announce(
                    format!("Not saved: {} error(s). {first}", errors.len()),
                    Tone::Error,
                    window,
                    cx,
                );
                false
            }
            Ok(saved) => {
                self.discard_epub_editor();
                self.load_book(id, window, cx);
                self.refresh_all();
                window.focus(&self.list_focus, cx);
                let message = match saved {
                    SaveOutcome::Saved { changes } => format!(
                        "Saved {} changed file(s); the original is kept as ORIGINAL_{format}.",
                        changes.len()
                    ),
                    _ => "Nothing changed; the library copy was left as it is.".to_string(),
                };
                self.announce(message, Tone::Success, window, cx);
                true
            }
            Err(err) => {
                self.announce(err, Tone::Error, window, cx);
                false
            }
        };
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
        ok
    }

    /// Check the book being edited and report what is wrong with it.
    pub fn check_epub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.epub_dir.clone() else {
            return;
        };
        // Writing the file for the check does not put it in the library, so
        // the editor still has unsaved work afterwards.
        let dirty = self.epub_dirty;
        let written = self.save_current_epub_file(cx);
        self.epub_dirty = dirty;
        if let Err(err) = written {
            self.announce(err, Tone::Error, window, cx);
            return;
        }
        match EpubDir::open(&dir) {
            Ok(book) => {
                let issues = book.check();
                let errors = issues
                    .iter()
                    .filter(|issue| issue.severity == Severity::Error)
                    .count();
                match issues.first() {
                    None => self.announce("No problems found.", Tone::Success, window, cx),
                    Some(first) => {
                        let line = first
                            .line
                            .map(|line| format!(":{line}"))
                            .unwrap_or_default();
                        self.announce(
                            format!(
                                "{errors} error(s), {} warning(s). First: {}{line}: {}",
                                issues.len() - errors,
                                first.file,
                                first.message
                            ),
                            if errors > 0 { Tone::Error } else { Tone::Info },
                            window,
                            cx,
                        );
                    }
                }
            }
            Err(err) => self.announce(format!("Cannot check: {err}"), Tone::Error, window, cx),
        }
        cx.notify();
    }

    pub fn close_epub_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.epub_dirty && !self.epub_pending {
            self.discard_epub_editor();
            self.set_status("Closed the book editor.", Tone::Info);
            window.focus(&self.list_focus, cx);
            cx.notify();
            return;
        }
        let receiver = window.prompt(
            PromptLevel::Warning,
            "Save changes to the book before closing?",
            Some("Your edits are only written to the library when the book is saved."),
            &["Repack & Close", "Discard Changes", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = receiver.await;
            this.update_in(cx, |this, window, cx| match answer {
                Ok(0) => {
                    this.repack_epub(window, cx);
                }
                Ok(1) => {
                    this.discard_epub_editor();
                    window.set_window_edited(this.has_unsaved_changes());
                    window.focus(&this.list_focus, cx);
                    this.set_status("Discarded the edits.", Tone::Info);
                    cx.notify();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn discard_epub_editor(&mut self) {
        if let Some(dir) = self.epub_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.epub_book_id = None;
        self.epub_files.clear();
        self.epub_selected = None;
        self.epub_dirty = false;
        self.epub_pending = false;
        if self.screen == Screen::Epub {
            self.screen = Screen::Library;
        }
    }

    // ---------------------------------------------------------------------
    // Closing and quitting
    // ---------------------------------------------------------------------

    /// Lets a running job stop at a safe point before the app exits, so no
    /// half-added book is left on disk.
    fn defer_until_job_stops(&mut self, then: AfterResolve, cx: &mut Context<Self>) -> bool {
        if self.job.is_none() {
            return false;
        }
        self.close_after_job = Some(then);
        self.cancel_job(cx);
        true
    }

    fn window_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.defer_until_job_stops(AfterResolve::CloseWindow, cx) {
            return false;
        }
        if self.has_unsaved_changes() {
            self.resolve_unsaved(AfterResolve::CloseWindow, window, cx);
            return false;
        }
        self.save_reading_position();
        self.persist_window_state(window);
        self.cleanup_temp_files();
        true
    }

    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.defer_until_job_stops(AfterResolve::Quit, cx) {
            return;
        }
        if self.has_unsaved_changes() {
            self.resolve_unsaved(AfterResolve::Quit, window, cx);
            return;
        }
        self.persist_window_state(window);
        self.cleanup_temp_files();
        cx.quit();
    }

    fn resolve_unsaved(&mut self, then: AfterResolve, window: &mut Window, cx: &mut Context<Self>) {
        let (message, detail) = if self.epub_book_id.is_some() {
            (
                "Repack your EPUB changes before closing?",
                "Edits that are not repacked are lost when calibre-rust closes.",
            )
        } else {
            (
                "Save metadata changes before closing?",
                "Your changes to this book will be lost if you don’t save them.",
            )
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            message,
            Some(detail),
            &["Save", "Don’t Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = receiver.await;
            this.update_in(cx, |this, window, cx| {
                let proceed = match answer {
                    Ok(0) => {
                        let mut ok = true;
                        if this.epub_book_id.is_some() {
                            ok = this.repack_epub(window, cx);
                        }
                        if ok && this.editing {
                            ok = this.save_metadata(window, cx);
                        }
                        ok
                    }
                    Ok(1) => {
                        this.discard_epub_editor();
                        this.editing = false;
                        true
                    }
                    _ => false,
                };
                if !proceed {
                    return;
                }
                this.persist_window_state(window);
                this.cleanup_temp_files();
                match then {
                    AfterResolve::CloseWindow => window.remove_window(),
                    AfterResolve::Quit => cx.quit(),
                }
            })
            .ok();
        })
        .detach();
    }

    fn persist_window_state(&mut self, window: &Window) {
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, false),
            WindowBounds::Maximized(bounds) => (bounds, true),
        };
        self.settings.window = Some(SavedBounds {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
            maximized,
        });
        self.save_settings();
    }

    /// Last chance to save state when the OS quits the app directly (Dock
    /// "Quit", logout, AppleScript), bypassing the Quit action.
    pub fn prepare_for_quit(&mut self, window: &Window) {
        self.save_reading_position();
        self.persist_window_state(window);
        self.cleanup_temp_files();
    }

    fn cleanup_temp_files(&mut self) {
        self.clear_crop();
        self.discard_epub_editor();
    }

    // ---------------------------------------------------------------------
    // Action dispatch
    // ---------------------------------------------------------------------

    /// Registers every window-level action handler on the root element.
    pub fn register_actions(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &Quit, window, cx| this.request_quit(window, cx)))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                if this.window_should_close(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|this, _: &About, window, cx| this.show_about(window, cx)))
            .on_action(cx.listener(|this, _: &ShowShortcuts, _, cx| {
                this.show_shortcuts = !this.show_shortcuts;
                cx.notify();
            }))
            // File
            .on_action(
                cx.listener(|this, _: &AddBooks, window, cx| this.prompt_add_books(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenLibrary, window, cx| {
                    this.prompt_open_library(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NewLibrary, window, cx| this.prompt_new_library(window, cx)),
            )
            .on_action(cx.listener(|this, action: &OpenRecentLibrary, window, cx| {
                if let Some(path) = this.settings.recent_libraries.get(action.0).cloned() {
                    this.open_library(path, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ClearRecentLibraries, _, cx| {
                this.settings.recent_libraries.clear();
                this.settings_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &ExportSelected, window, cx| {
                let ids = this.selection_ids();
                this.prompt_export(ids, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ExportAll, window, cx| {
                let ids = this.books.iter().map(|book| book.id).collect();
                this.prompt_export(ids, window, cx);
            }))
            // Edit
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if this.screen != Screen::Library || this.library.is_none() {
                    return;
                }
                this.search.update(cx, |input, cx| input.focus(window, cx));
            }))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(
                cx.listener(|this, _: &SaveChanges, window, cx| match this.screen {
                    Screen::Epub => this.save_epub_action(window, cx),
                    Screen::Crop => this.apply_crop(window, cx),
                    Screen::Library if this.editing => {
                        this.save_metadata(window, cx);
                    }
                    _ => {}
                }),
            )
            // View
            .on_action(cx.listener(|this, _: &ShowAsList, _, cx| this.set_view(ViewMode::List, cx)))
            .on_action(cx.listener(|this, _: &ShowAsGrid, _, cx| this.set_view(ViewMode::Grid, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.settings.sidebar_visible = !this.settings.sidebar_visible;
                this.settings_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDetailsPane, _, cx| {
                this.settings.inspector_visible = !this.settings.inspector_visible;
                this.settings_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &ThemeSystem, window, cx| {
                this.set_theme(ThemePreference::System, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ThemeLight, window, cx| {
                this.set_theme(ThemePreference::Light, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ThemeDark, window, cx| {
                this.set_theme(ThemePreference::Dark, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SortByTitle, _, cx| this.set_sort(BookSort::Title, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SortByAuthor, _, cx| this.set_sort(BookSort::Author, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SortByDateAdded, _, cx| this.set_sort(BookSort::Date, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SortByPublished, _, cx| {
                    this.set_sort(BookSort::Pubdate, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SortBySeries, _, cx| this.set_sort(BookSort::Series, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SortByRating, _, cx| this.set_sort(BookSort::Rating, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSortDirection, _, cx| {
                let sort = this.settings.sort();
                this.set_sort(sort, cx);
            }))
            // Book
            .on_action(cx.listener(|this, _: &ReadBook, window, cx| {
                if this.screen == Screen::Library {
                    this.start_reading(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &EditMetadata, window, cx| this.begin_edit(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ChangeCover, window, cx| this.prompt_cover(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CropCover, window, cx| this.begin_cover_crop(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &EditEpub, window, cx| this.begin_epub_edit(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CheckEpub, window, cx| this.check_epub(window, cx)))
            .on_action(cx.listener(|this, _: &RepackEpub, window, cx| {
                this.repack_epub(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenInDefaultApp, window, cx| {
                this.open_selected_externally(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RevealInFileManager, _, cx| this.reveal_selected(cx)))
            .on_action(cx.listener(|this, _: &CopyBookInfo, _, cx| this.copy_book_info(cx)))
            .on_action(cx.listener(|this, _: &RemoveBook, window, cx| {
                if this.screen == Screen::Library && !this.editing {
                    this.confirm_remove(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SendToFirstDevice, window, cx| {
                this.send_selected(0, window, cx)
            }))
            .on_action(cx.listener(|this, action: &SendToDevice, window, cx| {
                this.send_selected(action.0, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RescanDevices, window, cx| this.rescan_devices(window, cx)),
            )
            // List navigation
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                let step = this.vertical_step();
                this.move_selection(-step, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                let step = this.vertical_step();
                this.move_selection(step, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, window, cx| {
                if this.settings.view == ViewMode::Grid {
                    this.move_selection(-1, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectRight, window, cx| {
                if this.settings.view == ViewMode::Grid {
                    this.move_selection(1, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, window, cx| {
                this.click_index(0, false, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                if this.screen == Screen::Library {
                    this.select_all(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectLast, window, cx| {
                let last = this.books.len().saturating_sub(1);
                this.click_index(last, false, false, window, cx);
            }))
            // Reader
            .on_action(cx.listener(|this, _: &PreviousChapter, _, cx| this.step_chapter(-1, cx)))
            .on_action(cx.listener(|this, _: &NextChapter, _, cx| this.step_chapter(1, cx)))
            .on_action(
                cx.listener(|this, _: &IncreaseFontSize, _, cx| this.change_font_size(1.0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &DecreaseFontSize, _, cx| this.change_font_size(-1.0, cx)),
            )
            .on_action(cx.listener(|this, _: &ResetFontSize, _, cx| this.change_font_size(0.0, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleBookmark, window, cx| {
                    this.annotate(false, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ToggleHighlight, window, cx| {
                    this.annotate(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FindInBook, window, cx| {
                this.chapter_list_visible = true;
                this.reader_panel = ReaderPanel::Search;
                let handle = this.reader_search.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextMatch, _, cx| this.step_hit(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousMatch, _, cx| this.step_hit(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleChapterList, _, cx| {
                this.chapter_list_visible = !this.chapter_list_visible;
                cx.notify();
            }))
    }

    fn vertical_step(&self) -> isize {
        match self.settings.view {
            ViewMode::List => 1,
            ViewMode::Grid => self.grid_columns() as isize,
        }
    }

    fn set_view(&mut self, view: ViewMode, cx: &mut Context<Self>) {
        if self.settings.view != view {
            self.settings.view = view;
            if let Some(index) = self.selected_index() {
                self.scroll_to(index);
            }
            self.settings_changed(cx);
        }
    }

    /// Escape backs out of whatever is in progress, innermost first.
    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_shortcuts {
            self.show_shortcuts = false;
            cx.notify();
            return;
        }
        match self.screen {
            Screen::Crop => self.cancel_crop(window, cx),
            Screen::Epub => self.close_epub_editor(window, cx),
            Screen::Reader => self.close_reader(window, cx),
            Screen::Library if self.editing => self.cancel_edit(window, cx),
            Screen::Library if !self.query.is_empty() => {
                self.query.clear();
                set_input(&self.search, String::new(), window, cx);
                self.reload();
                if let Some(index) = self.selected_index() {
                    self.scroll_to(index);
                }
                window.focus(&self.list_focus, cx);
                cx.notify();
            }
            Screen::Library => window.focus(&self.list_focus, cx),
        }
    }

    fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detail = format!(
            "Version {}\nA small, fast ebook library manager that works with calibre libraries.",
            env!("CARGO_PKG_VERSION")
        );
        let receiver = window.prompt(
            PromptLevel::Info,
            "calibre-rust",
            Some(&detail),
            &["OK", "Website"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if let Ok(1) = receiver.await {
                cx.update(|cx| cx.open_url(PROJECT_URL));
            }
        })
        .detach();
    }
}

// -------------------------------------------------------------------------
// Free helpers
// -------------------------------------------------------------------------

fn text_input(
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<CalibreApp>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()))
}

pub fn set_input(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
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

fn set_textarea(input: &Entity<TextareaState>, value: String, window: &mut Window, cx: &mut App) {
    input.update(cx, |state, cx| state.set_value(value, window, cx));
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality)
        .encode_image(&image.to_rgb8())
        .map_err(|err| format!("Could not encode the cover: {err}"))?;
    Ok(data)
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

pub fn fit_dims(width: f32, height: f32, max_width: f32, max_height: f32) -> (f32, f32) {
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
        let editable = file_type.is_file()
            && files::has_extension(
                &path,
                &[
                    "xhtml", "html", "htm", "css", "opf", "ncx", "xml", "svg", "txt", "js",
                ],
            );
        if !editable {
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

pub fn default_library_path() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Calibre Library")
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

pub fn format_size(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn crop_pixels_stay_inside_the_image() {
        assert_eq!(
            crop_pixels(100, 200, 0.0, 0.0, 100.0, 100.0),
            (0, 0, 100, 200)
        );
        assert_eq!(
            crop_pixels(100, 200, 50.0, 50.0, 100.0, 100.0),
            (50, 100, 50, 100)
        );
        assert_eq!(crop_pixels(10, 10, 99.0, 99.0, 1.0, 1.0), (9, 9, 1, 1));
    }

    #[core::prelude::v1::test]
    fn identifiers_parse_both_separators() {
        let parsed = parse_identifiers("ISBN=123, asin:B00X").unwrap();
        assert_eq!(parsed.get("isbn").map(String::as_str), Some("123"));
        assert_eq!(parsed.get("asin").map(String::as_str), Some("B00X"));
        assert!(parse_identifiers("nope").is_err());
    }

    #[core::prelude::v1::test]
    fn categories_count_and_sort_case_insensitively() {
        let book = |authors: &[&str], tags: &[&str]| BookSummary {
            id: 1,
            title: "t".into(),
            authors: authors.iter().map(|a| a.to_string()).collect(),
            author_sort: String::new(),
            publisher: None,
            series: None,
            series_index: 1.0,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            rating: None,
            formats: vec!["EPUB".into()],
            has_cover: false,
            timestamp: None,
            pubdate: None,
            path: String::new(),
        };
        let categories =
            Categories::from_books(&[book(&["b", "A"], &["x"]), book(&["A"], &["x", "Y"])]);
        assert_eq!(categories.authors, vec![("A".into(), 2), ("b".into(), 1)]);
        assert_eq!(categories.tags, vec![("x".into(), 2), ("Y".into(), 1)]);
        assert_eq!(categories.formats, vec![("EPUB".into(), 2)]);
    }
}
