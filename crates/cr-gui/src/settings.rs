//! Persistent user preferences, stored as JSON in the platform config folder.

use std::path::{Path, PathBuf};

use cr_core::library::BookSort;
use serde::{Deserialize, Serialize};

const MAX_RECENT_LIBRARIES: usize = 10;
pub const DEFAULT_READER_FONT_SIZE: f32 = 17.0;
pub const MIN_READER_FONT_SIZE: f32 = 12.0;
pub const MAX_READER_FONT_SIZE: f32 = 32.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    List,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub last_library: Option<PathBuf>,
    pub recent_libraries: Vec<PathBuf>,
    pub theme: ThemePreference,
    pub view: ViewMode,
    pub sidebar_visible: bool,
    pub inspector_visible: bool,
    pub sort: String,
    pub descending: bool,
    pub reader_font_size: f32,
    pub window: Option<SavedBounds>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_library: None,
            recent_libraries: Vec::new(),
            theme: ThemePreference::System,
            view: ViewMode::List,
            sidebar_visible: true,
            inspector_visible: true,
            sort: BookSort::Title.as_str().to_string(),
            descending: false,
            reader_font_size: DEFAULT_READER_FONT_SIZE,
            window: None,
        }
    }
}

impl Settings {
    /// Loads settings, falling back to defaults when the file is missing or unreadable.
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.reader_font_size = settings
                    .reader_font_size
                    .clamp(MIN_READER_FONT_SIZE, MAX_READER_FONT_SIZE);
                settings.recent_libraries.truncate(MAX_RECENT_LIBRARIES);
                settings
            }
            Err(err) => {
                eprintln!(
                    "calibre-rust: ignoring unreadable {}: {err}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path().ok_or("No configuration folder is available.")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| format!("Could not encode settings: {err}"))?;
        std::fs::write(&path, text)
            .map_err(|err| format!("Could not save {}: {err}", path.display()))
    }

    pub fn sort(&self) -> BookSort {
        BookSort::parse(&self.sort).unwrap_or(BookSort::Title)
    }

    pub fn set_sort(&mut self, sort: BookSort) {
        self.sort = sort.as_str().to_string();
    }

    /// Records a library as the most recently used one.
    pub fn remember_library(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.recent_libraries.retain(|existing| existing != &path);
        self.recent_libraries.insert(0, path.clone());
        self.recent_libraries.truncate(MAX_RECENT_LIBRARIES);
        self.last_library = Some(path);
    }
}

/// The per-user configuration folder for calibre-rust.
fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|home| home.join("Library/Application Support/calibre-rust"));
    }
    if cfg!(target_os = "windows") {
        return std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("calibre-rust"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|dir| dir.join("calibre-rust"))
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}
