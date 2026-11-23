//! The tools screen (bulk edit, trash, search tools, conversion, devices,
//! content server, maintenance) and the book editor's search and contents
//! panels: state and operations. Rendering lives in [`crate::ui::tools`]
//! and [`crate::ui::epub`].

use std::collections::HashSet;
use std::path::PathBuf;

use cr_core::device::{self, DeviceBook, DeviceReading};
use cr_core::formats::editing::{EpubDir, Match, SearchOptions, TocEntry};
use cr_core::formats::Profile;
use cr_core::library::{
    BulkEdit, ConvertOutcome, Library, LibraryConvertOptions, Problem, SearchReplace, TextField,
    TrashEntry,
};
use cr_core::server::{Server, ServerOptions, Stopper};
use gpui_kit::component::input::InputState;
use gpui_kit::*;

use crate::app::{
    capitalize, scan_epub_files, set_input, set_textarea, split_commas, text_input, CalibreApp,
    Screen, Tone,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPane {
    BulkEdit,
    Trash,
    Search,
    Convert,
    Devices,
    Server,
    Maintenance,
}

impl ToolPane {
    pub const ALL: [Self; 7] = [
        Self::BulkEdit,
        Self::Convert,
        Self::Search,
        Self::Devices,
        Self::Server,
        Self::Trash,
        Self::Maintenance,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::BulkEdit => "Edit Metadata in Bulk",
            Self::Trash => "Trash",
            Self::Search => "Search Tools",
            Self::Convert => "Convert Books",
            Self::Devices => "Devices",
            Self::Server => "Content Server",
            Self::Maintenance => "Library Maintenance",
        }
    }
}

/// What the book editor's side panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorPanel {
    Files,
    Search,
    Contents,
}

pub const TEXT_FIELDS: [(&str, TextField); 6] = [
    ("Title", TextField::Title),
    ("Authors", TextField::Authors),
    ("Publisher", TextField::Publisher),
    ("Series", TextField::Series),
    ("Tags", TextField::Tags),
    ("Comments", TextField::Comments),
];

pub const PROFILES: [(&str, Profile); 4] = [
    ("Generic", Profile::Generic),
    ("Kindle", Profile::Kindle),
    ("Kobo", Profile::Kobo),
    ("Tablet", Profile::Tablet),
];

/// Fields the search builder offers, as `(label, name in a search)`.
pub const SEARCH_FIELDS: [(&str, &str); 13] = [
    ("Title", "title"),
    ("Author", "author"),
    ("Tag", "tag"),
    ("Series", "series"),
    ("Publisher", "publisher"),
    ("Format", "format"),
    ("Language", "language"),
    ("Identifier", "identifier"),
    ("Comments", "comments"),
    ("Rating", "rating"),
    ("Date added", "date"),
    ("Published", "pubdate"),
    ("Size", "size"),
];

/// How a search term compares its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermMatch {
    Contains,
    Equals,
    Regex,
    Less,
    Greater,
    /// The field has any value (`field:true`).
    Present,
    /// The field is empty (`field:false`).
    Missing,
}

impl TermMatch {
    pub const ALL: [(&'static str, Self); 7] = [
        ("contains", Self::Contains),
        ("is exactly", Self::Equals),
        ("matches regex", Self::Regex),
        ("less than", Self::Less),
        ("more than", Self::Greater),
        ("is set", Self::Present),
        ("is empty", Self::Missing),
    ];
}

/// How a new term joins the search so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    And,
    Or,
    Not,
}

/// One term of calibre's search language, e.g. `title:"=War and Peace"`.
pub fn search_term(field: &str, kind: TermMatch, value: &str) -> Result<String, String> {
    let value = value.trim();
    let prefix = match kind {
        TermMatch::Present => return Ok(format!("{field}:true")),
        TermMatch::Missing => return Ok(format!("{field}:false")),
        TermMatch::Contains => "",
        TermMatch::Equals => "=",
        TermMatch::Regex => "~",
        TermMatch::Less => "<",
        TermMatch::Greater => ">",
    };
    if value.is_empty() {
        return Err("Type a value for the term.".to_string());
    }
    let plain = value
        .chars()
        .all(|c| !c.is_whitespace() && !matches!(c, '"' | '(' | ')'));
    if plain {
        Ok(format!("{field}:{prefix}{value}"))
    } else {
        Ok(format!(
            "{field}:\"{prefix}{}\"",
            value.replace('"', "\\\"")
        ))
    }
}

/// Add a term to a search. An existing search with alternatives is grouped,
/// so the new term narrows all of it.
pub fn append_term(query: &str, term: &str, join: Join) -> String {
    let query = query.trim();
    if query.is_empty() {
        return match join {
            Join::Not => format!("not {term}"),
            _ => term.to_string(),
        };
    }
    let alternatives = query
        .split_whitespace()
        .any(|word| word.eq_ignore_ascii_case("or"));
    let grouped = if alternatives && join != Join::Or {
        format!("({query})")
    } else {
        query.to_string()
    };
    match join {
        Join::And => format!("{grouped} {term}"),
        Join::Or => format!("{grouped} or {term}"),
        Join::Not => format!("{grouped} not {term}"),
    }
}

/// The bulk edit form as typed.
#[derive(Debug, Clone, Default)]
pub struct BulkForm {
    pub authors: String,
    pub publisher: String,
    pub series: String,
    pub series_start: String,
    pub rating: String,
    pub languages: String,
    pub add_tags: String,
    pub remove_tags: String,
    pub clear_publisher: bool,
    pub clear_series: bool,
    pub clear_rating: bool,
    pub clear_tags: bool,
    pub find: String,
    pub replace: String,
    pub field: Option<TextField>,
    pub regex: bool,
    pub ignore_case: bool,
}

/// The edit a filled-in form describes. Empty inputs leave a field alone.
pub fn bulk_edit(form: &BulkForm) -> Result<BulkEdit, String> {
    let text = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    let choice = |value: &str, clear: bool, name: &str| match (text(value), clear) {
        (Some(_), true) => Err(format!("Either set the {name} or clear it, not both.")),
        (Some(value), false) => Ok(Some(Some(value))),
        (None, true) => Ok(Some(None)),
        (None, false) => Ok(None),
    };
    let rating = match (text(&form.rating), form.clear_rating) {
        (Some(_), true) => return Err("Either set the rating or clear it, not both.".to_string()),
        (Some(value), false) => Some(Some(
            value
                .parse::<u8>()
                .ok()
                .filter(|rating| *rating <= 10)
                .ok_or("Rating must be a number from 0 to 10.")?,
        )),
        (None, true) => Some(None),
        (None, false) => None,
    };
    let series_start = match text(&form.series_start) {
        Some(value) => Some(
            value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.)
                .ok_or("Series numbering must start at a number, such as 1.")?,
        ),
        None => None,
    };
    let replace = match (text(&form.find), form.field) {
        (Some(pattern), Some(field)) => {
            if form.regex {
                regex::Regex::new(&pattern).map_err(|err| format!("Bad pattern: {err}"))?;
            }
            Some(SearchReplace {
                field,
                pattern,
                replacement: form.replace.clone(),
                regex: form.regex,
                ignore_case: form.ignore_case,
            })
        }
        (Some(_), None) => return Err("Choose the field to search and replace in.".to_string()),
        (None, _) => None,
    };
    let edit = BulkEdit {
        authors: text(&form.authors).map(|authors| cr_core::text::split_authors(&authors)),
        publisher: choice(&form.publisher, form.clear_publisher, "publisher")?,
        series: choice(&form.series, form.clear_series, "series")?,
        series_start,
        rating,
        languages: text(&form.languages).map(|languages| split_commas(&languages)),
        clear_tags: form.clear_tags,
        remove_tags: split_commas(&form.remove_tags),
        add_tags: split_commas(&form.add_tags),
        copy: None,
        replace,
    };
    if edit == BulkEdit::default() {
        return Err("Nothing to change: fill in at least one field.".to_string());
    }
    Ok(edit)
}

/// Server settings from the form, and a warning when the library would be
/// open to other machines without a password.
pub fn server_options(
    host: &str,
    port: &str,
    user: &str,
    password: &str,
) -> Result<(ServerOptions, Option<String>), String> {
    let host = match host.trim() {
        "" => "127.0.0.1",
        host => host,
    };
    let port = match port.trim() {
        "" => 8080,
        port => port
            .parse::<u16>()
            .map_err(|_| "The port must be a number from 0 to 65535.".to_string())?,
    };
    let user = user.trim();
    let credentials = match (user.is_empty(), password.is_empty()) {
        (true, true) => None,
        (false, false) => Some((user.to_string(), password.to_string())),
        (false, true) => return Err("Set a password for that user.".to_string()),
        (true, false) => return Err("Set a user name for that password.".to_string()),
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let local = bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    let warning = (credentials.is_none() && !local).then(|| {
        "Anyone who can reach this address can read and download the library. Set a user and password.".to_string()
    });
    let bind = if bare.contains(':') {
        format!("[{bare}]:{port}")
    } else {
        format!("{bare}:{port}")
    };
    Ok((ServerOptions { bind, credentials }, warning))
}

/// The content server started from the tools screen.
pub struct RunningServer {
    pub url: String,
    pub warning: Option<String>,
    stopper: Stopper,
}

pub struct Tools {
    pub pane: ToolPane,

    pub bulk_authors: Entity<InputState>,
    pub bulk_publisher: Entity<InputState>,
    pub bulk_series: Entity<InputState>,
    pub bulk_series_start: Entity<InputState>,
    pub bulk_rating: Entity<InputState>,
    pub bulk_languages: Entity<InputState>,
    pub bulk_add_tags: Entity<InputState>,
    pub bulk_remove_tags: Entity<InputState>,
    pub bulk_find: Entity<InputState>,
    pub bulk_replace: Entity<InputState>,
    pub bulk_clear_publisher: bool,
    pub bulk_clear_series: bool,
    pub bulk_clear_rating: bool,
    pub bulk_clear_tags: bool,
    pub bulk_field: Option<TextField>,
    pub bulk_regex: bool,
    pub bulk_ignore_case: bool,

    pub trash: Vec<TrashEntry>,

    pub saved_searches: Vec<(String, String)>,
    pub virtual_libraries: Vec<(String, String)>,
    pub search_name: Entity<InputState>,
    pub term_value: Entity<InputState>,
    pub term_field: usize,
    pub term_match: TermMatch,

    pub convert_target: &'static str,
    pub convert_profile: Profile,
    pub convert_overwrite: bool,
    pub convert_no_split: bool,

    pub device_index: usize,
    pub device_books: Vec<DeviceBook>,
    pub device_marked: HashSet<String>,
    pub device_reading: Vec<DeviceReading>,
    /// Why the device's reading status is not shown.
    pub device_reading_note: Option<String>,
    pub device_loading: bool,
    pub device_loaded: bool,

    pub server_host: Entity<InputState>,
    pub server_port: Entity<InputState>,
    pub server_user: Entity<InputState>,
    pub server_password: Entity<InputState>,
    pub server: Option<RunningServer>,

    /// The last library check; `None` before one has run.
    pub problems: Option<Vec<Problem>>,

    pub editor_panel: EditorPanel,
    pub editor_find: Entity<InputState>,
    pub editor_replace: Entity<InputState>,
    pub editor_regex: bool,
    pub editor_case: bool,
    /// The matches of the last search; `None` before one has run.
    pub editor_matches: Option<Vec<Match>>,
    pub editor_toc: Vec<TocEntry>,
    pub editor_toc_selected: Option<usize>,
    pub editor_toc_title: Entity<InputState>,
    /// The contents list differs from the one in the book.
    pub editor_toc_dirty: bool,
}

impl Tools {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        let server_password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password")
                .masked(true)
        });
        Self {
            pane: ToolPane::BulkEdit,
            bulk_authors: text_input("Authors, separated by &", window, cx),
            bulk_publisher: text_input("Publisher", window, cx),
            bulk_series: text_input("Series", window, cx),
            bulk_series_start: text_input("Number books from, e.g. 1", window, cx),
            bulk_rating: text_input("0–10", window, cx),
            bulk_languages: text_input("eng, fra", window, cx),
            bulk_add_tags: text_input("Tags to add, comma separated", window, cx),
            bulk_remove_tags: text_input("Tags to remove, comma separated", window, cx),
            bulk_find: text_input("Find", window, cx),
            bulk_replace: text_input("Replace with", window, cx),
            bulk_clear_publisher: false,
            bulk_clear_series: false,
            bulk_clear_rating: false,
            bulk_clear_tags: false,
            bulk_field: None,
            bulk_regex: false,
            bulk_ignore_case: false,
            trash: Vec::new(),
            saved_searches: Vec::new(),
            virtual_libraries: Vec::new(),
            search_name: text_input("Name", window, cx),
            term_value: text_input("Value", window, cx),
            term_field: 0,
            term_match: TermMatch::Contains,
            convert_target: "EPUB",
            convert_profile: Profile::Generic,
            convert_overwrite: false,
            convert_no_split: false,
            device_index: 0,
            device_books: Vec::new(),
            device_marked: HashSet::new(),
            device_reading: Vec::new(),
            device_reading_note: None,
            device_loading: false,
            device_loaded: false,
            server_host: text_input("127.0.0.1", window, cx),
            server_port: text_input("8080", window, cx),
            server_user: text_input("User (optional)", window, cx),
            server_password,
            server: None,
            problems: None,
            editor_panel: EditorPanel::Files,
            editor_find: text_input("Find in book", window, cx),
            editor_replace: text_input("Replace with", window, cx),
            editor_regex: false,
            editor_case: false,
            editor_matches: None,
            editor_toc: Vec::new(),
            editor_toc_selected: None,
            editor_toc_title: text_input("Entry title", window, cx),
            editor_toc_dirty: false,
        }
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

fn books(count: usize) -> String {
    match count {
        1 => "1 book".to_string(),
        count => format!("{count} books"),
    }
}

impl CalibreApp {
    // ---------------------------------------------------------------------
    // Navigation
    // ---------------------------------------------------------------------

    pub fn open_tools(&mut self, pane: ToolPane, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.screen, Screen::Library | Screen::Tools) {
            return;
        }
        if self.editing {
            self.announce(
                "Save or cancel your metadata changes first.",
                Tone::Info,
                window,
                cx,
            );
            cx.notify();
            return;
        }
        if self.library.is_none() {
            self.announce("Open a library first.", Tone::Info, window, cx);
            cx.notify();
            return;
        }
        self.screen = Screen::Tools;
        self.show_tool(pane, window, cx);
        window.focus(&self.focus_handle, cx);
    }

    pub fn show_tool(&mut self, pane: ToolPane, window: &mut Window, cx: &mut Context<Self>) {
        self.tools.pane = pane;
        match pane {
            ToolPane::Trash => self.load_trash(),
            ToolPane::Search => self.load_searches(),
            ToolPane::Devices if !self.tools.device_loaded && !self.devices.is_empty() => {
                self.load_device(self.tools.device_index, window, cx)
            }
            _ => {}
        }
        cx.notify();
    }

    pub fn close_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen == Screen::Tools {
            self.screen = Screen::Library;
            window.focus(&self.list_focus, cx);
            cx.notify();
        }
    }

    // ---------------------------------------------------------------------
    // Bulk metadata edit
    // ---------------------------------------------------------------------

    pub fn bulk_form(&self, cx: &App) -> BulkForm {
        let tools = &self.tools;
        BulkForm {
            authors: value(&tools.bulk_authors, cx),
            publisher: value(&tools.bulk_publisher, cx),
            series: value(&tools.bulk_series, cx),
            series_start: value(&tools.bulk_series_start, cx),
            rating: value(&tools.bulk_rating, cx),
            languages: value(&tools.bulk_languages, cx),
            add_tags: value(&tools.bulk_add_tags, cx),
            remove_tags: value(&tools.bulk_remove_tags, cx),
            clear_publisher: tools.bulk_clear_publisher,
            clear_series: tools.bulk_clear_series,
            clear_rating: tools.bulk_clear_rating,
            clear_tags: tools.bulk_clear_tags,
            find: value(&tools.bulk_find, cx),
            replace: value(&tools.bulk_replace, cx),
            field: tools.bulk_field,
            regex: tools.bulk_regex,
            ignore_case: tools.bulk_ignore_case,
        }
    }

    fn clear_bulk_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tools = &mut self.tools;
        for input in [
            &tools.bulk_authors,
            &tools.bulk_publisher,
            &tools.bulk_series,
            &tools.bulk_series_start,
            &tools.bulk_rating,
            &tools.bulk_languages,
            &tools.bulk_add_tags,
            &tools.bulk_remove_tags,
            &tools.bulk_find,
            &tools.bulk_replace,
        ] {
            set_input(input, String::new(), window, cx);
        }
        tools.bulk_clear_publisher = false;
        tools.bulk_clear_series = false;
        tools.bulk_clear_rating = false;
        tools.bulk_clear_tags = false;
    }

    pub fn apply_bulk_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selection_ids();
        if ids.is_empty() {
            self.announce("Select the books to edit first.", Tone::Info, window, cx);
            cx.notify();
            return;
        }
        let edit = match bulk_edit(&self.bulk_form(cx)) {
            Ok(edit) => edit,
            Err(message) => {
                self.announce(message, Tone::Error, window, cx);
                cx.notify();
                return;
            }
        };
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        self.run_job(
            format!("Editing {}", books(ids.len())),
            window,
            cx,
            move |context| Ok(Library::open(&root)?.bulk_update(&ids, &edit, context)),
            |this, result, window, cx| {
                let results = match result {
                    Ok(results) => results,
                    Err(err) => {
                        this.announce(format!("Edit failed: {err}"), Tone::Error, window, cx);
                        return;
                    }
                };
                let changed = results
                    .iter()
                    .filter(|item| matches!(item.result, Ok(true)))
                    .count();
                let unchanged = results
                    .iter()
                    .filter(|item| matches!(item.result, Ok(false)))
                    .count();
                let failed: Vec<String> = results
                    .iter()
                    .filter_map(|item| {
                        let err = item.result.as_ref().err()?;
                        Some(format!("book {}: {err}", item.item))
                    })
                    .collect();
                this.refresh_all();
                if let Some(id) = this.selected {
                    this.load_book(id, window, cx);
                }
                let mut message = format!("Changed {}", books(changed));
                if unchanged > 0 {
                    message.push_str(&format!("; {unchanged} already as asked"));
                }
                match failed.last() {
                    Some(err) => this.announce(
                        format!("{message}; {} failed. Last error: {err}", failed.len()),
                        Tone::Error,
                        window,
                        cx,
                    ),
                    None => {
                        this.clear_bulk_form(window, cx);
                        this.announce(format!("{message}."), Tone::Success, window, cx)
                    }
                }
            },
        );
    }

    // ---------------------------------------------------------------------
    // Trash
    // ---------------------------------------------------------------------

    pub fn load_trash(&mut self) {
        let listed = self.library.as_ref().map(|library| library.list_trash());
        match listed {
            Some(Ok(entries)) => self.tools.trash = entries,
            Some(Err(err)) => {
                self.tools.trash.clear();
                self.set_status(format!("Could not read the trash: {err}"), Tone::Error);
            }
            None => self.tools.trash.clear(),
        }
    }

    pub fn restore_trashed(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.tools.trash.get(index).cloned() else {
            return;
        };
        let Some(library) = self.library.as_mut() else {
            return;
        };
        let result = if entry.is_book {
            library.restore_book(entry.id).map(|restored| restored.id)
        } else {
            entry
                .formats
                .iter()
                .try_for_each(|format| library.restore_format(entry.id, format))
                .map(|()| entry.id)
        };
        match result {
            Ok(id) => {
                self.refresh_all();
                self.load_book(id, window, cx);
                self.announce(
                    format!("Restored “{}”.", entry.title),
                    Tone::Success,
                    window,
                    cx,
                );
            }
            Err(err) => self.announce(
                format!("Could not restore “{}”: {err}", entry.title),
                Tone::Error,
                window,
                cx,
            ),
        }
        self.load_trash();
        cx.notify();
    }

    /// Asks, then deletes one trashed entry for good, or the whole trash.
    pub fn confirm_purge(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let entry = index.and_then(|index| self.tools.trash.get(index).cloned());
        let (question, id) = match (&entry, index) {
            (Some(entry), _) => (
                format!("Delete “{}” permanently?", entry.title),
                Some(entry.id),
            ),
            (None, Some(_)) => return,
            (None, None) if self.tools.trash.is_empty() => return,
            (None, None) => (
                format!(
                    "Delete all {} item(s) in the trash permanently?",
                    self.tools.trash.len()
                ),
                None,
            ),
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            &question,
            Some("This cannot be undone."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(0) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    let purged = this.library.as_mut().map(|library| library.purge_trash(id));
                    match purged {
                        Some(Ok(count)) => this.announce(
                            format!("Deleted {count} item(s) from the trash."),
                            Tone::Success,
                            window,
                            cx,
                        ),
                        Some(Err(err)) => this.announce(
                            format!("Could not delete: {err}"),
                            Tone::Error,
                            window,
                            cx,
                        ),
                        None => {}
                    }
                    this.load_trash();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Search tools
    // ---------------------------------------------------------------------

    pub fn load_searches(&mut self) {
        let Some(library) = &self.library else {
            return;
        };
        let saved = library.saved_searches();
        let virtual_libraries = library.virtual_libraries();
        match (saved, virtual_libraries) {
            (Ok(saved), Ok(virtual_libraries)) => {
                self.tools.saved_searches = saved.into_iter().collect();
                self.tools.virtual_libraries = virtual_libraries.into_iter().collect();
            }
            (Err(err), _) | (_, Err(err)) => {
                self.set_status(format!("Could not read saved searches: {err}"), Tone::Error)
            }
        }
    }

    /// Puts a search in the library's search box and runs it.
    pub fn set_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query = query.clone();
        set_input(&self.search, query, window, cx);
        self.reload();
        cx.notify();
    }

    pub fn add_search_term(&mut self, join: Join, window: &mut Window, cx: &mut Context<Self>) {
        let (_, field) = SEARCH_FIELDS[self.tools.term_field.min(SEARCH_FIELDS.len() - 1)];
        let typed = value(&self.tools.term_value, cx);
        match search_term(field, self.tools.term_match, &typed) {
            Ok(term) => {
                let query = append_term(&self.query, &term, join);
                set_input(&self.tools.term_value, String::new(), window, cx);
                self.set_query(query, window, cx);
                self.set_status(
                    format!("{} found.", books(self.books.len())),
                    Tone::Info,
                );
            }
            Err(message) => self.announce(message, Tone::Info, window, cx),
        }
        cx.notify();
    }

    /// Runs a search and goes back to the book list.
    pub fn show_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_query(query, window, cx);
        self.close_tools(window, cx);
    }

    /// Saves the current search under the typed name, as a saved search or
    /// as a virtual library.
    pub fn save_search(&mut self, virtual_library: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = value(&self.tools.search_name, cx).trim().to_string();
        let query = self.query.trim().to_string();
        let problem = if name.is_empty() {
            Some("Type a name to save the search under.")
        } else if query.is_empty() {
            Some("There is no search to save: build or type one first.")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.announce(problem, Tone::Info, window, cx);
            cx.notify();
            return;
        }
        let Some(library) = self.library.as_mut() else {
            return;
        };
        let saved = if virtual_library {
            library.set_virtual_library(&name, Some(&query))
        } else {
            library.set_saved_search(&name, Some(&query))
        };
        match saved {
            Ok(()) => {
                set_input(&self.tools.search_name, String::new(), window, cx);
                let kind = if virtual_library {
                    "virtual library"
                } else {
                    "saved search"
                };
                self.announce(format!("Saved the {kind} “{name}”."), Tone::Success, window, cx);
            }
            Err(err) => self.announce(format!("Could not save: {err}"), Tone::Error, window, cx),
        }
        self.load_searches();
        cx.notify();
    }

    pub fn delete_search(
        &mut self,
        name: String,
        virtual_library: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self.library.as_mut() else {
            return;
        };
        let deleted = if virtual_library {
            library.set_virtual_library(&name, None)
        } else {
            library.set_saved_search(&name, None)
        };
        match deleted {
            Ok(()) => self.set_status(format!("Deleted “{name}”."), Tone::Info),
            Err(err) => self.announce(format!("Could not delete: {err}"), Tone::Error, window, cx),
        }
        self.load_searches();
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Conversion
    // ---------------------------------------------------------------------

    pub fn convert_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.selection_ids();
        if ids.is_empty() {
            self.announce("Select the books to convert first.", Tone::Info, window, cx);
            cx.notify();
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let target = self.tools.convert_target;
        let options = LibraryConvertOptions {
            from: None,
            overwrite: self.tools.convert_overwrite,
            no_split: self.tools.convert_no_split,
            profile: self.tools.convert_profile,
        };
        self.run_job(
            format!("Converting {} to {target}", books(ids.len())),
            window,
            cx,
            move |context| Ok(Library::open(&root)?.convert_books(&ids, target, &options, context)),
            move |this, result, window, cx| {
                let results = match result {
                    Ok(results) => results,
                    Err(err) => {
                        this.announce(format!("Conversion failed: {err}"), Tone::Error, window, cx);
                        return;
                    }
                };
                let converted = results
                    .iter()
                    .filter(|item| matches!(item.result, Ok(ConvertOutcome::Converted { .. })))
                    .count();
                let skipped = results
                    .iter()
                    .filter(|item| matches!(item.result, Ok(ConvertOutcome::Skipped)))
                    .count();
                let failed: Vec<String> = results
                    .iter()
                    .filter_map(|item| {
                        let err = item.result.as_ref().err()?;
                        Some(format!("book {}: {err}", item.item))
                    })
                    .collect();
                this.refresh_all();
                if let Some(id) = this.selected {
                    this.load_book(id, window, cx);
                }
                let mut message = format!("Converted {} to {target}", books(converted));
                if skipped > 0 {
                    message.push_str(&format!("; {skipped} already had {target}"));
                }
                match failed.last() {
                    Some(err) => this.announce(
                        format!("{message}; {} failed. Last error: {err}", failed.len()),
                        Tone::Error,
                        window,
                        cx,
                    ),
                    None => this.announce(format!("{message}."), Tone::Success, window, cx),
                }
            },
        );
    }

    // ---------------------------------------------------------------------
    // Devices
    // ---------------------------------------------------------------------

    /// Reads what is on a device, off the UI thread.
    pub fn load_device(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device) = self.devices.get(index).cloned() else {
            self.tools.device_books.clear();
            self.tools.device_reading.clear();
            self.tools.device_loaded = false;
            cx.notify();
            return;
        };
        if self.tools.device_loading {
            return;
        }
        let root = self.library.as_ref().map(|library| library.root().to_path_buf());
        self.tools.device_index = index;
        self.tools.device_loading = true;
        self.tools.device_marked.clear();
        cx.notify();
        let read = cx.background_spawn(async move {
            let library = root.map(|root| Library::open(&root)).transpose()?;
            let books = device::books(library.as_ref(), &device)?;
            Ok::<_, cr_core::Error>((books, device::reading(&device)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            this.update_in(cx, |this, window, cx| {
                this.tools.device_loading = false;
                match result {
                    Ok((books, reading)) => {
                        this.tools.device_books = books;
                        this.tools.device_loaded = true;
                        match reading {
                            Ok(reading) => {
                                this.tools.device_reading = reading;
                                this.tools.device_reading_note = None;
                            }
                            Err(err) => {
                                this.tools.device_reading.clear();
                                this.tools.device_reading_note = Some(capitalize(&err.to_string()));
                            }
                        }
                    }
                    Err(err) => {
                        this.tools.device_books.clear();
                        this.tools.device_reading.clear();
                        this.tools.device_loaded = false;
                        this.announce(
                            format!("Could not read the device: {err}"),
                            Tone::Error,
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn toggle_device_book(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.tools.device_marked.remove(&path) {
            self.tools.device_marked.insert(path);
        }
        cx.notify();
    }

    /// Asks, then deletes the ticked books from the device.
    pub fn confirm_device_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.tools.device_index;
        let Some(device) = self.devices.get(index).cloned() else {
            return;
        };
        // In the order shown, and only what is still listed.
        let paths: Vec<String> = self
            .tools
            .device_books
            .iter()
            .filter(|book| self.tools.device_marked.contains(&book.path))
            .map(|book| book.path.clone())
            .collect();
        if paths.is_empty() {
            self.announce("Tick the books to remove first.", Tone::Info, window, cx);
            cx.notify();
            return;
        }
        let receiver = window.prompt(
            PromptLevel::Warning,
            &format!("Delete {} from {}?", books(paths.len()), device.name),
            Some("The files are deleted from the device. The library is not changed."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(0) = receiver.await else {
                return;
            };
            let removal =
                cx.background_spawn(async move { device::remove(&device, &paths) });
            let result = removal.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(removed) => this.announce(
                        format!("Deleted {} from the device.", books(removed.len())),
                        Tone::Success,
                        window,
                        cx,
                    ),
                    Err(err) => this.announce(
                        format!("Could not delete from the device: {err}"),
                        Tone::Error,
                        window,
                        cx,
                    ),
                }
                this.load_device(index, window, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Content server
    // ---------------------------------------------------------------------

    pub fn start_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tools.server.is_some() {
            return;
        }
        let Some(root) = self.library.as_ref().map(|library| library.root().to_path_buf()) else {
            self.announce("Open a library first.", Tone::Info, window, cx);
            return;
        };
        let tools = &self.tools;
        let options = server_options(
            &value(&tools.server_host, cx),
            &value(&tools.server_port, cx),
            &value(&tools.server_user, cx),
            &value(&tools.server_password, cx),
        );
        let started = options.and_then(|(options, warning)| {
            let server = Server::bind(&root, &options).map_err(|err| capitalize(&err.to_string()))?;
            let address = server.local_addr().map_err(|err| err.to_string())?;
            Ok((server, address, warning))
        });
        match started {
            Ok((server, address, warning)) => {
                let host = if address.ip().is_unspecified() {
                    "localhost".to_string()
                } else if address.is_ipv6() {
                    format!("[{}]", address.ip())
                } else {
                    address.ip().to_string()
                };
                let url = format!("http://{host}:{}", address.port());
                let stopper = server.stopper();
                // Each request is answered on its own thread; this one only
                // accepts connections until the stopper is used.
                std::thread::spawn(move || server.run());
                self.announce(format!("Serving the library at {url}."), Tone::Success, window, cx);
                self.tools.server = Some(RunningServer {
                    url,
                    warning,
                    stopper,
                });
            }
            Err(message) => self.announce(message, Tone::Error, window, cx),
        }
        cx.notify();
    }

    /// Stops the content server if it is running.
    pub fn stop_server(&mut self) -> bool {
        match self.tools.server.take() {
            Some(server) => {
                server.stopper.stop();
                true
            }
            None => false,
        }
    }

    // ---------------------------------------------------------------------
    // Maintenance
    // ---------------------------------------------------------------------

    pub fn check_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.library.as_ref().map(|library| library.root().to_path_buf()) else {
            self.announce("Open a library first.", Tone::Info, window, cx);
            return;
        };
        self.run_job(
            "Checking the library",
            window,
            cx,
            move |_| Library::open(&root)?.check(),
            |this, result, window, cx| match result {
                Ok(problems) => {
                    let (message, tone) = match problems.len() {
                        0 => ("The library is consistent.".to_string(), Tone::Success),
                        count => (format!("Found {count} problem(s)."), Tone::Info),
                    };
                    this.tools.problems = Some(problems);
                    this.announce(message, tone, window, cx);
                }
                Err(err) => this.announce(format!("Check failed: {err}"), Tone::Error, window, cx),
            },
        );
    }

    pub fn repair_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let problems: Vec<Problem> = self
            .tools
            .problems
            .iter()
            .flatten()
            .filter(|problem| problem.repairable())
            .cloned()
            .collect();
        if problems.is_empty() {
            self.announce("There is nothing to repair.", Tone::Info, window, cx);
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        self.run_job(
            format!("Repairing {} problem(s)", problems.len()),
            window,
            cx,
            move |_| {
                let mut library = Library::open(&root)?;
                let repaired = library.repair(&problems)?;
                Ok((repaired, library.check()?))
            },
            |this, result, window, cx| match result {
                Ok((repaired, left)) => {
                    this.refresh_all();
                    let message = match left.len() {
                        0 => format!("Repaired {repaired} problem(s); the library is consistent."),
                        count => format!(
                            "Repaired {repaired} problem(s); {count} need attention by hand."
                        ),
                    };
                    this.tools.problems = Some(left);
                    this.announce(message, Tone::Success, window, cx);
                }
                Err(err) => this.announce(format!("Repair failed: {err}"), Tone::Error, window, cx),
            },
        );
    }

    /// Asks, then rebuilds `metadata.db` from the books' `metadata.opf` files.
    pub fn confirm_rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            self.announce("Wait for the running job to finish first.", Tone::Info, window, cx);
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            "Rebuild the library database?",
            Some(
                "The database is rebuilt from the metadata.opf file in each book folder. \
                 The current database is kept as a backup. Custom columns, saved searches \
                 and virtual libraries are not restored.",
            ),
            &["Rebuild", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(0) = receiver.await {
                this.update_in(cx, |this, window, cx| this.rebuild_library(root, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn rebuild_library(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        // Nothing may hold the database while it is replaced.
        self.stop_server();
        self.library = None;
        self.tools.problems = None;
        let reopen = root.clone();
        self.run_job(
            "Rebuilding the database",
            window,
            cx,
            move |_| Library::rebuild(&root).map(|(_, report)| report),
            move |this, result, window, cx| {
                this.open_library(reopen, window, cx);
                match result {
                    Ok(report) => {
                        let mut message = format!("Rebuilt the database: {}", books(report.restored));
                        if let Some(backup) = &report.backup {
                            message.push_str(&format!(
                                "; the old one is kept as {}",
                                crate::app::display_name(backup)
                            ));
                        }
                        match report.failed.first() {
                            Some((folder, reason)) => this.announce(
                                format!(
                                    "{message}; {} folder(s) could not be read. First: {folder}: {reason}",
                                    report.failed.len()
                                ),
                                Tone::Error,
                                window,
                                cx,
                            ),
                            None => this.announce(format!("{message}."), Tone::Success, window, cx),
                        }
                    }
                    Err(err) => {
                        this.announce(format!("Rebuild failed: {err}"), Tone::Error, window, cx)
                    }
                }
            },
        );
    }

    // ---------------------------------------------------------------------
    // Book editor: search and replace
    // ---------------------------------------------------------------------

    pub fn show_editor_panel(&mut self, panel: EditorPanel, cx: &mut Context<Self>) {
        self.tools.editor_panel = panel;
        if panel == EditorPanel::Contents && !self.tools.editor_toc_dirty {
            self.load_editor_toc();
        }
        cx.notify();
    }

    /// Clears what the editor panels remember about the last book.
    pub fn reset_editor_tools(&mut self) {
        let tools = &mut self.tools;
        tools.editor_panel = EditorPanel::Files;
        tools.editor_matches = None;
        tools.editor_toc.clear();
        tools.editor_toc_selected = None;
        tools.editor_toc_dirty = false;
    }

    fn editor_search_options(&self) -> SearchOptions {
        SearchOptions {
            regex: self.tools.editor_regex,
            case_sensitive: self.tools.editor_case,
            file: None,
        }
    }

    /// Writes the open file so a whole-book operation sees the text on
    /// screen, and opens the unpacked book.
    fn editor_book(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<EpubDir> {
        let dir = self.epub_dir.clone()?;
        if let Err(err) = self.save_current_epub_file(cx) {
            self.announce(err, Tone::Error, window, cx);
            return None;
        }
        match EpubDir::open(&dir) {
            Ok(book) => Some(book),
            Err(err) => {
                self.announce(format!("Cannot read the book: {err}"), Tone::Error, window, cx);
                None
            }
        }
    }

    pub fn editor_find_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = value(&self.tools.editor_find, cx);
        if query.is_empty() {
            self.tools.editor_matches = None;
            cx.notify();
            return;
        }
        let options = self.editor_search_options();
        let Some(book) = self.editor_book(window, cx) else {
            return;
        };
        match book.search(&query, &options) {
            Ok(matches) => {
                let files: HashSet<&str> = matches.iter().map(|hit| hit.file.as_str()).collect();
                self.set_status(
                    format!("{} match(es) in {} file(s).", matches.len(), files.len()),
                    Tone::Info,
                );
                self.tools.editor_matches = Some(matches);
            }
            Err(err) => self.announce(capitalize(&err.to_string()), Tone::Error, window, cx),
        }
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
    }

    pub fn editor_replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = value(&self.tools.editor_find, cx);
        if query.is_empty() {
            self.announce("Type what to find first.", Tone::Info, window, cx);
            return;
        }
        let replacement = value(&self.tools.editor_replace, cx);
        let options = self.editor_search_options();
        let Some(mut book) = self.editor_book(window, cx) else {
            return;
        };
        match book.replace(&query, &replacement, &options) {
            Ok(changed) => {
                let total: usize = changed.iter().map(|(_, count)| count).sum();
                if total > 0 {
                    self.epub_pending = true;
                    self.reload_editor_files(window, cx);
                }
                self.tools.editor_matches = None;
                self.announce(
                    format!("Replaced {total} match(es) in {} file(s).", changed.len()),
                    if total > 0 { Tone::Success } else { Tone::Info },
                    window,
                    cx,
                );
            }
            Err(err) => self.announce(capitalize(&err.to_string()), Tone::Error, window, cx),
        }
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
    }

    /// After the files changed on disk: list them again and show the open
    /// one as it is now.
    fn reload_editor_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.epub_dir.clone() else {
            return;
        };
        let open = self
            .epub_selected
            .and_then(|index| self.epub_files.get(index))
            .map(|file| file.relative.clone());
        self.epub_files = scan_epub_files(&dir);
        self.epub_selected = open.and_then(|open| {
            self.epub_files
                .iter()
                .position(|file| file.relative == open)
        });
        self.epub_dirty = false;
        let text = self
            .epub_selected
            .and_then(|index| self.epub_files.get(index))
            .and_then(|file| std::fs::read_to_string(&file.path).ok());
        match text {
            Some(text) => set_textarea(&self.epub_editor, text, window, cx),
            None => {
                self.epub_selected = None;
                if !self.epub_files.is_empty() {
                    self.load_epub_file(0, window, cx);
                }
            }
        }
        // Setting the text reports a change; the file is as on disk.
        self.epub_dirty = false;
    }

    /// Opens the file a search match is in.
    pub fn open_editor_match(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self
            .tools
            .editor_matches
            .as_ref()
            .and_then(|matches| matches.get(index))
            .cloned()
        else {
            return;
        };
        self.open_editor_path(&hit.file, window, cx);
        self.set_status(format!("{}, line {}.", hit.file, hit.line), Tone::Info);
        cx.notify();
    }

    fn open_editor_path(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.split('#').next().unwrap_or(path);
        let found = self
            .epub_files
            .iter()
            .position(|file| file.relative.replace('\\', "/") == path);
        if let Some(index) = found {
            self.load_epub_file(index, window, cx);
        }
    }

    // ---------------------------------------------------------------------
    // Book editor: table of contents
    // ---------------------------------------------------------------------

    fn load_editor_toc(&mut self) {
        let Some(dir) = self.epub_dir.clone() else {
            return;
        };
        match EpubDir::open(&dir).and_then(|book| book.toc()) {
            Ok(entries) => {
                self.tools.editor_toc = entries;
                self.tools.editor_toc_selected = None;
                self.tools.editor_toc_dirty = false;
            }
            Err(err) => self.set_status(format!("Could not read the contents: {err}"), Tone::Error),
        }
    }

    /// Rebuilds the table of contents from the book's headings.
    pub fn generate_editor_toc(&mut self, depth: u8, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut book) = self.editor_book(window, cx) else {
            return;
        };
        match book.generate_toc(depth) {
            Ok(count) => {
                self.epub_pending = true;
                self.reload_editor_files(window, cx);
                self.load_editor_toc();
                self.announce(
                    format!("Built a table of contents with {count} entries from the headings."),
                    Tone::Success,
                    window,
                    cx,
                );
            }
            Err(err) => self.announce(
                format!("Could not build the contents: {err}"),
                Tone::Error,
                window,
                cx,
            ),
        }
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
    }

    pub fn select_toc_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.tools.editor_toc.get(index).cloned() else {
            return;
        };
        self.tools.editor_toc_selected = Some(index);
        set_input(&self.tools.editor_toc_title, entry.title, window, cx);
        self.open_editor_path(&entry.target, window, cx);
        cx.notify();
    }

    /// Changes the selected entry of the contents list; nothing is written
    /// until [`Self::save_editor_toc`].
    pub fn edit_toc_entry(&mut self, change: TocChange, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tools.editor_toc_selected else {
            self.announce("Select an entry first.", Tone::Info, window, cx);
            return;
        };
        let title = value(&self.tools.editor_toc_title, cx);
        let (selected, changed) = change_toc(&mut self.tools.editor_toc, index, change, &title);
        self.tools.editor_toc_selected = selected;
        self.tools.editor_toc_dirty |= changed;
        if selected.is_none() {
            set_input(&self.tools.editor_toc_title, String::new(), window, cx);
        }
        cx.notify();
    }

    pub fn save_editor_toc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.tools.editor_toc.clone();
        let Some(mut book) = self.editor_book(window, cx) else {
            return;
        };
        match book.set_toc(&entries) {
            Ok(()) => {
                self.epub_pending = true;
                self.reload_editor_files(window, cx);
                self.tools.editor_toc_dirty = false;
                self.announce(
                    format!("Wrote a table of contents with {} entries.", entries.len()),
                    Tone::Success,
                    window,
                    cx,
                );
            }
            Err(err) => self.announce(
                format!("Could not write the contents: {err}"),
                Tone::Error,
                window,
                cx,
            ),
        }
        window.set_window_edited(self.has_unsaved_changes());
        cx.notify();
    }
}

/// An edit to one entry of a table of contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TocChange {
    Rename,
    Remove,
    Up,
    Down,
    Indent,
    Outdent,
}

/// Applies a change to entry `index`. Returns where the entry is afterwards
/// and whether anything changed.
pub fn change_toc(
    entries: &mut Vec<TocEntry>,
    index: usize,
    change: TocChange,
    title: &str,
) -> (Option<usize>, bool) {
    if index >= entries.len() {
        return (None, false);
    }
    match change {
        TocChange::Rename => {
            let title = title.trim();
            if title.is_empty() || entries[index].title == title {
                return (Some(index), false);
            }
            entries[index].title = title.to_string();
            (Some(index), true)
        }
        TocChange::Remove => {
            entries.remove(index);
            (None, true)
        }
        TocChange::Up if index > 0 => {
            entries.swap(index, index - 1);
            (Some(index - 1), true)
        }
        TocChange::Down if index + 1 < entries.len() => {
            entries.swap(index, index + 1);
            (Some(index + 1), true)
        }
        // An entry can be at most one level below the one before it.
        TocChange::Indent
            if index > 0 && entries[index].level <= entries[index - 1].level =>
        {
            entries[index].level += 1;
            (Some(index), true)
        }
        TocChange::Outdent if entries[index].level > 1 => {
            entries[index].level -= 1;
            (Some(index), true)
        }
        _ => (Some(index), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_terms_follow_the_search_language() {
        let term = |kind, value| search_term("title", kind, value);
        assert_eq!(term(TermMatch::Contains, " dune ").unwrap(), "title:dune");
        assert_eq!(
            term(TermMatch::Equals, "War and Peace").unwrap(),
            "title:\"=War and Peace\""
        );
        assert_eq!(term(TermMatch::Regex, "^The").unwrap(), "title:~^The");
        assert_eq!(
            term(TermMatch::Contains, "say \"hi\"").unwrap(),
            "title:\"say \\\"hi\\\"\""
        );
        assert_eq!(
            search_term("rating", TermMatch::Greater, "3").unwrap(),
            "rating:>3"
        );
        assert_eq!(search_term("size", TermMatch::Less, "1M").unwrap(), "size:<1M");
        assert_eq!(search_term("series", TermMatch::Present, "").unwrap(), "series:true");
        assert_eq!(search_term("tag", TermMatch::Missing, "x").unwrap(), "tag:false");
        assert!(term(TermMatch::Contains, "  ").is_err());
    }

    #[test]
    fn terms_join_a_search_without_changing_its_meaning() {
        assert_eq!(append_term("", "tag:sf", Join::And), "tag:sf");
        assert_eq!(append_term(" ", "tag:sf", Join::Not), "not tag:sf");
        assert_eq!(append_term("a", "tag:sf", Join::And), "a tag:sf");
        assert_eq!(append_term("a", "tag:sf", Join::Or), "a or tag:sf");
        assert_eq!(append_term("a", "tag:sf", Join::Not), "a not tag:sf");
        // Alternatives are grouped before they are narrowed.
        assert_eq!(append_term("a OR b", "tag:sf", Join::And), "(a OR b) tag:sf");
        assert_eq!(append_term("a or b", "tag:sf", Join::Not), "(a or b) not tag:sf");
        assert_eq!(append_term("a or b", "tag:sf", Join::Or), "a or b or tag:sf");
    }

    #[test]
    fn a_bulk_form_becomes_an_edit() {
        assert!(bulk_edit(&BulkForm::default()).is_err());
        let form = BulkForm {
            authors: "Ann Lee & Bo Chen".into(),
            series: " Saga ".into(),
            series_start: "1".into(),
            rating: "8".into(),
            languages: "eng, fra".into(),
            add_tags: "sf, space".into(),
            remove_tags: "todo".into(),
            clear_publisher: true,
            find: "Vol.".into(),
            replace: "Volume".into(),
            field: Some(TextField::Title),
            ignore_case: true,
            ..Default::default()
        };
        let edit = bulk_edit(&form).unwrap();
        assert_eq!(edit.authors, Some(vec!["Ann Lee".into(), "Bo Chen".into()]));
        assert_eq!(edit.publisher, Some(None));
        assert_eq!(edit.series, Some(Some("Saga".into())));
        assert_eq!(edit.series_start, Some(1.));
        assert_eq!(edit.rating, Some(Some(8)));
        assert_eq!(edit.languages, Some(vec!["eng".into(), "fra".into()]));
        assert_eq!(edit.add_tags, ["sf", "space"]);
        assert_eq!(edit.remove_tags, ["todo"]);
        assert!(!edit.clear_tags);
        let replace = edit.replace.unwrap();
        assert_eq!((replace.pattern.as_str(), replace.replacement.as_str()), ("Vol.", "Volume"));
        assert!(replace.ignore_case && !replace.regex);

        let only = |form: BulkForm| bulk_edit(&form);
        // Untouched fields stay untouched.
        let edit = only(BulkForm {
            clear_tags: true,
            ..Default::default()
        })
        .unwrap();
        assert!(edit.clear_tags && edit.publisher.is_none() && edit.rating.is_none());
        assert_eq!(
            only(BulkForm {
                clear_rating: true,
                ..Default::default()
            })
            .unwrap()
            .rating,
            Some(None)
        );
        for bad in [
            BulkForm {
                rating: "11".into(),
                ..Default::default()
            },
            BulkForm {
                rating: "3".into(),
                clear_rating: true,
                ..Default::default()
            },
            BulkForm {
                series: "Saga".into(),
                clear_series: true,
                ..Default::default()
            },
            BulkForm {
                series_start: "one".into(),
                ..Default::default()
            },
            BulkForm {
                find: "x".into(),
                ..Default::default()
            },
            BulkForm {
                find: "(".into(),
                field: Some(TextField::Title),
                regex: true,
                ..Default::default()
            },
        ] {
            assert!(only(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_settings_are_checked() {
        let (options, warning) = server_options("", "", "", "").unwrap();
        assert_eq!(options.bind, "127.0.0.1:8080");
        assert!(options.credentials.is_none() && warning.is_none());

        let (options, warning) = server_options("0.0.0.0", "9000", "", "").unwrap();
        assert_eq!(options.bind, "0.0.0.0:9000");
        assert!(warning.is_some());

        let (options, warning) = server_options("0.0.0.0", "0", " ann ", "s3 cret").unwrap();
        assert_eq!(options.credentials, Some(("ann".into(), "s3 cret".into())));
        assert!(warning.is_none());

        let (options, warning) = server_options("::1", "8081", "", "").unwrap();
        assert_eq!(options.bind, "[::1]:8081");
        assert!(warning.is_none());
        assert!(server_options("localhost", "80", "", "").unwrap().1.is_none());

        assert!(server_options("", "70000", "", "").is_err());
        assert!(server_options("", "http", "", "").is_err());
        assert!(server_options("", "", "ann", "").is_err());
        assert!(server_options("", "", "", "secret").is_err());
    }

    #[test]
    fn contents_entries_are_edited_in_place() {
        let entry = |title: &str, level| TocEntry {
            title: title.into(),
            level,
            target: format!("{title}.xhtml"),
        };
        let titles = |entries: &[TocEntry]| -> Vec<String> {
            entries
                .iter()
                .map(|entry| format!("{}{}", "-".repeat(entry.level as usize - 1), entry.title))
                .collect()
        };
        let mut entries = vec![entry("One", 1), entry("Two", 1), entry("Three", 1)];

        assert_eq!(change_toc(&mut entries, 1, TocChange::Rename, " 2 "), (Some(1), true));
        assert_eq!(change_toc(&mut entries, 1, TocChange::Rename, "2"), (Some(1), false));
        assert_eq!(change_toc(&mut entries, 1, TocChange::Rename, " "), (Some(1), false));
        assert_eq!(change_toc(&mut entries, 1, TocChange::Indent, ""), (Some(1), true));
        // No deeper than one below the entry before, and not the first.
        assert_eq!(change_toc(&mut entries, 1, TocChange::Indent, ""), (Some(1), false));
        assert_eq!(change_toc(&mut entries, 0, TocChange::Indent, ""), (Some(0), false));
        assert_eq!(titles(&entries), ["One", "-2", "Three"]);

        assert_eq!(change_toc(&mut entries, 2, TocChange::Up, ""), (Some(1), true));
        assert_eq!(change_toc(&mut entries, 0, TocChange::Up, ""), (Some(0), false));
        assert_eq!(change_toc(&mut entries, 2, TocChange::Down, ""), (Some(2), false));
        assert_eq!(titles(&entries), ["One", "Three", "-2"]);

        assert_eq!(change_toc(&mut entries, 2, TocChange::Outdent, ""), (Some(2), true));
        assert_eq!(change_toc(&mut entries, 2, TocChange::Outdent, ""), (Some(2), false));
        assert_eq!(change_toc(&mut entries, 1, TocChange::Remove, ""), (None, true));
        assert_eq!(titles(&entries), ["One", "2"]);
        assert_eq!(change_toc(&mut entries, 5, TocChange::Remove, ""), (None, false));
    }
}
