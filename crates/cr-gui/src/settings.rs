//! Persistent user preferences, stored as JSON in the platform config folder.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use cr_core::library::{BookSort, Collision, DuplicatePolicy, ExportOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupLibrary {
    #[default]
    LastUsed,
    Default,
    Ask,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenExternalBook {
    #[default]
    Import,
    Read,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDuplicates {
    #[default]
    Merge,
    KeepBoth,
    Skip,
}

impl From<ImportDuplicates> for DuplicatePolicy {
    fn from(value: ImportDuplicates) -> Self {
        match value {
            ImportDuplicates::Merge => Self::Merge,
            ImportDuplicates::KeepBoth => Self::Add,
            ImportDuplicates::Skip => Self::Skip,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportLayout {
    #[default]
    BookFolders,
    AuthorFolders,
    Flat,
}

impl ExportLayout {
    pub fn template(self) -> &'static str {
        match self {
            Self::BookFolders => "{author_sort}/{title} ({id})/{title} - {author}",
            Self::AuthorFolders => "{author_sort}/{title} - {author} ({id})",
            Self::Flat => "{title} - {author} ({id})",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportCollision {
    #[default]
    Rename,
    Skip,
    Replace,
}

impl From<ExportCollision> for Collision {
    fn from(value: ExportCollision) -> Self {
        match value {
            ExportCollision::Rename => Self::Rename,
            ExportCollision::Skip => Self::Skip,
            ExportCollision::Replace => Self::Overwrite,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderSpacing {
    Compact,
    #[default]
    Normal,
    Airy,
}

impl ReaderSpacing {
    pub fn factor(self) -> f32 {
        match self {
            Self::Compact => 1.35,
            Self::Normal => 1.65,
            Self::Airy => 1.95,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderWidth {
    Narrow,
    #[default]
    Standard,
    Wide,
}

impl ReaderWidth {
    pub fn ems(self) -> f32 {
        match self {
            Self::Narrow => 32.,
            Self::Standard => 40.,
            Self::Wide => 52.,
        }
    }
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
    pub startup_library: StartupLibrary,
    pub restore_window: bool,
    pub auto_detect_devices: bool,
    pub import_duplicates: ImportDuplicates,
    pub open_external_book: OpenExternalBook,
    pub export_layout: ExportLayout,
    pub export_collision: ExportCollision,
    pub export_cover: bool,
    pub export_metadata: bool,
    pub reveal_export: bool,
    pub reader_spacing: ReaderSpacing,
    pub reader_width: ReaderWidth,
    pub reader_contents: bool,
    pub reader_resume: bool,
    pub publisher_styles: bool,
    pub openrouter_model: String,
    pub ai_presets: Vec<cr_core::ai::PromptPreset>,
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
            startup_library: StartupLibrary::LastUsed,
            restore_window: true,
            auto_detect_devices: true,
            import_duplicates: ImportDuplicates::Merge,
            open_external_book: OpenExternalBook::Import,
            export_layout: ExportLayout::BookFolders,
            export_collision: ExportCollision::Rename,
            export_cover: false,
            export_metadata: false,
            reveal_export: true,
            reader_spacing: ReaderSpacing::Normal,
            reader_width: ReaderWidth::Standard,
            reader_contents: true,
            reader_resume: true,
            publisher_styles: true,
            openrouter_model: cr_core::ai::DEFAULT_MODEL.into(),
            ai_presets: Vec::new(),
        }
    }
}

impl Settings {
    /// Loads settings, falling back to defaults when the file is missing or unreadable.
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match Self::read_from(&path) {
            Ok(settings) => settings,
            Err(err) => {
                eprintln!("calibre-rust: Could not load {}: {err}", path.display());
                Self::default()
            }
        }
    }

    fn read_from(path: &Path) -> io::Result<Self> {
        let mut fields = Self::read_fields(path)?;
        let mut settings: Self = match serde_json::from_value(Value::Object(fields.clone())) {
            Ok(settings) => settings,
            Err(_) => {
                // Settings fields are independent. Decode each on its own
                // before falling back to its default, keeping valid neighbors.
                fields.retain(|key, value| {
                    let field = Map::from_iter([(key.clone(), value.clone())]);
                    serde_json::from_value::<Self>(Value::Object(field)).is_ok()
                });
                serde_json::from_value(Value::Object(fields))?
            }
        };
        settings.reader_font_size = settings
            .reader_font_size
            .clamp(MIN_READER_FONT_SIZE, MAX_READER_FONT_SIZE);
        settings.recent_libraries.truncate(MAX_RECENT_LIBRARIES);
        Ok(settings)
    }

    fn read_fields(path: &Path) -> io::Result<Map<String, Value>> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(err) => return Err(err),
        };
        match serde_json::from_reader(io::BufReader::new(file))? {
            Value::Object(fields) => Ok(fields),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected a JSON object",
            )),
        }
    }

    pub fn save(&self, previous: &Self, reset_preferences: bool) -> Result<(), String> {
        let path = settings_path().ok_or("No configuration folder is available.")?;
        self.write_changes(&path, previous, reset_preferences)
    }

    fn write_changes(
        &self,
        path: &Path,
        previous: &Self,
        reset_preferences: bool,
    ) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or("The settings path has no parent folder.")?;
        let write = || -> Result<(), Box<dyn std::error::Error>> {
            let previous = serde_json::to_value(previous)?;
            let Value::Object(mut changes) = serde_json::to_value(self)? else {
                unreachable!("Settings serialize as a JSON object");
            };
            changes.retain(|key, value| {
                previous.get(key) != Some(value)
                    || reset_preferences
                        && !matches!(key.as_str(), "last_library" | "recent_libraries" | "window")
            });
            if changes.is_empty() {
                return Ok(());
            }
            std::fs::create_dir_all(parent)?;
            // Lock a separate file so replacing settings.json leaves the lock
            // valid across processes. Read the latest snapshot while holding it.
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(parent.join("settings.lock"))?;
            lock.lock()?;
            let mut merged = match Self::read_fields(path) {
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    Map::new()
                }
                settings => settings?,
            };
            merged.extend(changes);
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer_pretty(&mut file, &merged)?;
            file.write_all(b"\n")?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        };
        write().map_err(|err| format!("Could not save {}: {err}", path.display()))
    }

    pub fn export_options(&self) -> ExportOptions {
        ExportOptions {
            template: self.export_layout.template().into(),
            collision: self.export_collision.into(),
            cover: self.export_cover,
            opf: self.export_metadata,
            ..Default::default()
        }
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
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = absolute_env_path("CALIBRE_RUST_CONFIG_DIR") {
        return Some(dir);
    }
    let home = absolute_env_path("HOME");
    if cfg!(target_os = "macos") {
        return home.map(|home| home.join("Library/Application Support/calibre-rust"));
    }
    if cfg!(target_os = "windows") {
        return absolute_env_path("APPDATA").map(|dir| dir.join("calibre-rust"));
    }
    absolute_env_path("XDG_CONFIG_HOME")
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|dir| dir.join("calibre-rust"))
}

fn absolute_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn existing_settings_keep_values_and_get_new_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"theme":"dark","view":"grid","reader_font_size":75}"#,
        )
        .unwrap();
        let settings = Settings::read_from(&path).unwrap();
        assert_eq!(settings.theme, ThemePreference::Dark);
        assert_eq!(settings.view, ViewMode::Grid);
        assert_eq!(settings.reader_font_size, MAX_READER_FONT_SIZE);
        assert_eq!(settings.startup_library, StartupLibrary::LastUsed);
        assert_eq!(settings.import_duplicates, ImportDuplicates::Merge);
        assert!(settings.publisher_styles);
    }

    #[core::prelude::v1::test]
    fn settings_replace_previous_file_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile/settings.json");
        let previous = Settings::default();
        let settings = Settings {
            export_cover: true,
            export_layout: ExportLayout::Flat,
            export_collision: ExportCollision::Skip,
            import_duplicates: ImportDuplicates::Skip,
            open_external_book: OpenExternalBook::Read,
            reveal_export: false,
            reader_width: ReaderWidth::Wide,
            ..Default::default()
        };
        settings.write_changes(&path, &previous, false).unwrap();
        let loaded = Settings::read_from(&path).unwrap();
        let export = loaded.export_options();
        assert!(export.cover);
        assert_eq!(export.template, "{title} - {author} ({id})");
        assert_eq!(export.collision, Collision::Skip);
        assert_eq!(loaded.import_duplicates, ImportDuplicates::Skip);
        assert_eq!(loaded.open_external_book, OpenExternalBook::Read);
        assert!(!loaded.reveal_export);
        assert_eq!(loaded.reader_width, ReaderWidth::Wide);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            2
        );
    }

    #[core::prelude::v1::test]
    fn stale_window_save_preserves_preferences_from_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let initial = Settings::read_from(&path).unwrap();
        let preferences = Settings {
            theme: ThemePreference::Dark,
            reveal_export: false,
            ..initial.clone()
        };
        preferences.write_changes(&path, &initial, false).unwrap();

        let window = Some(SavedBounds {
            x: 10.0,
            y: 20.0,
            width: 1000.0,
            height: 700.0,
            maximized: false,
        });
        let stale = Settings {
            window,
            ..initial.clone()
        };
        stale.write_changes(&path, &initial, false).unwrap();
        let loaded = Settings::read_from(&path).unwrap();
        assert_eq!(loaded.theme, ThemePreference::Dark);
        assert!(!loaded.reveal_export);
        assert_eq!(loaded.window, window);

        initial.write_changes(&path, &initial, true).unwrap();
        let reset = Settings::read_from(&path).unwrap();
        assert_eq!(reset.theme, ThemePreference::System);
        assert!(reset.reveal_export);
        assert_eq!(reset.window, window);
    }

    #[core::prelude::v1::test]
    fn preference_changes_recover_an_invalid_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let defaults = Settings::default();
        let updated = Settings {
            theme: ThemePreference::Dark,
            ..defaults.clone()
        };
        for broken in [r#"{"theme":"sepia"}"#, r#"{"theme":"#] {
            std::fs::write(&path, broken).unwrap();
            updated.write_changes(&path, &defaults, false).unwrap();
            assert_eq!(
                Settings::read_from(&path).unwrap().theme,
                ThemePreference::Dark
            );
        }
    }

    #[core::prelude::v1::test]
    fn routine_saves_keep_valid_fields_beside_an_invalid_preference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"theme":"sepia","view":"grid","recent_libraries":["Books"]}"#,
        )
        .unwrap();
        let initial = Settings::read_from(&path).unwrap();
        assert_eq!(initial.theme, ThemePreference::System);
        let updated = Settings {
            export_cover: true,
            ..initial.clone()
        };
        updated.write_changes(&path, &initial, false).unwrap();
        let saved = Settings::read_from(&path).unwrap();
        assert_eq!(saved.view, ViewMode::Grid);
        assert_eq!(saved.recent_libraries, [PathBuf::from("Books")]);
        assert!(saved.export_cover);
        assert_eq!(Settings::read_fields(&path).unwrap()["theme"], "sepia");
    }
}
