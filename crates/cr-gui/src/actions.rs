//! Actions, keyboard shortcuts and the native application/dock menus.

use cr_core::library::BookSort;
use gpui_kit::component::input;
use gpui_kit::*;

use crate::settings::{Settings, ThemePreference, ViewMode};

actions!(
    calibre,
    [
        // Application
        Quit,
        About,
        OpenPreferences,
        ClosePreferences,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        ShowShortcuts,
        OpenProjectPage,
        // Window
        CloseWindow,
        Minimize,
        Zoom,
        ToggleFullScreen,
        // File
        AddBooks,
        OpenBookFile,
        OpenLibrary,
        NewLibrary,
        ExportSelected,
        ExportAll,
        ClearRecentLibraries,
        // Edit
        FocusSearch,
        Cancel,
        SaveChanges,
        // View
        ShowAsList,
        ShowAsGrid,
        ToggleSidebar,
        ToggleDetailsPane,
        ThemeSystem,
        ThemeLight,
        ThemeDark,
        SortByTitle,
        SortByAuthor,
        SortByDateAdded,
        SortByPublished,
        SortBySeries,
        SortByRating,
        ToggleSortDirection,
        // Book
        ReadBook,
        AttachFormats,
        EditMetadata,
        AskAi,
        DownloadMetadata,
        ChangeCover,
        CropCover,
        EditEpub,
        RepackEpub,
        CheckEpub,
        OpenInDefaultApp,
        RevealInFileManager,
        CopyBookInfo,
        RemoveBook,
        SelectAll,
        SendToFirstDevice,
        RescanDevices,
        // Tools
        OpenBulkEdit,
        OpenConvert,
        OpenSearchTools,
        OpenFullText,
        OpenDevices,
        OpenServer,
        OpenTrash,
        OpenMaintenance,
        OpenTransfer,
        OpenCatalog,
        OpenCustomColumns,
        // List navigation
        SelectPrevious,
        SelectNext,
        SelectLeft,
        SelectRight,
        SelectFirst,
        SelectLast,
        // Reader
        PreviousChapter,
        NextChapter,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        ToggleChapterList,
        ToggleBookmark,
        ToggleHighlight,
        FindInBook,
        NextMatch,
        PreviousMatch,
    ]
);

/// Opens the library at the given index of the recent libraries list.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = calibre, no_json)]
pub struct OpenRecentLibrary(pub usize);

/// Sends the selected book to the device at the given index.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = calibre, no_json)]
pub struct SendToDevice(pub usize);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FormatCommand {
    Read,
    Edit,
    Open,
    Reveal,
    CopyPath,
    SaveCopy,
    Remove,
}

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = calibre, no_json)]
pub struct BookFormatAction(pub FormatCommand, pub String);

/// Key context of the main window. Its bindings work everywhere in the window
/// unless a focused text field claims the same keystroke.
pub const APP_CONTEXT: &str = "CalibreApp";
/// Key context of the book list or cover grid.
pub const LIST_CONTEXT: &str = "BookList";
/// Key context of the reader.
pub const READER_CONTEXT: &str = "Reader";

/// Expands to a keystroke using the platform's primary modifier:
/// Command on macOS and Control elsewhere.
macro_rules! primary {
    ($keys:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $keys)
        } else {
            concat!("ctrl-", $keys)
        }
    };
}

pub fn bind_keys(cx: &mut App) {
    let app = Some(APP_CONTEXT);
    let list = Some(LIST_CONTEXT);
    let reader = Some(READER_CONTEXT);

    let mut bindings = vec![
        KeyBinding::new(primary!(","), OpenPreferences, None),
        // File
        KeyBinding::new(primary!("o"), AddBooks, app),
        KeyBinding::new(primary!("alt-shift-o"), OpenBookFile, app),
        KeyBinding::new(primary!("shift-o"), OpenLibrary, app),
        KeyBinding::new(primary!("shift-n"), NewLibrary, app),
        KeyBinding::new(primary!("shift-e"), ExportSelected, app),
        KeyBinding::new(primary!("w"), CloseWindow, app),
        // Edit
        KeyBinding::new(primary!("f"), FocusSearch, app),
        KeyBinding::new(primary!("s"), SaveChanges, app),
        KeyBinding::new("escape", Cancel, app),
        // View
        KeyBinding::new(primary!("1"), ShowAsList, app),
        KeyBinding::new(primary!("2"), ShowAsGrid, app),
        KeyBinding::new(primary!("alt-s"), ToggleSidebar, app),
        KeyBinding::new(primary!("alt-i"), ToggleDetailsPane, app),
        KeyBinding::new(primary!("/"), ShowShortcuts, app),
        // Book
        KeyBinding::new(primary!("enter"), ReadBook, app),
        KeyBinding::new(primary!("shift-a"), AttachFormats, app),
        KeyBinding::new(primary!("i"), EditMetadata, app),
        KeyBinding::new(primary!("alt-o"), OpenInDefaultApp, app),
        KeyBinding::new(primary!("shift-r"), RevealInFileManager, app),
        KeyBinding::new(primary!("shift-c"), CopyBookInfo, app),
        KeyBinding::new(primary!("backspace"), RemoveBook, app),
        KeyBinding::new(primary!("d"), SendToFirstDevice, app),
        KeyBinding::new(primary!("shift-d"), RescanDevices, app),
        KeyBinding::new(primary!("shift-m"), OpenBulkEdit, app),
        KeyBinding::new(primary!("shift-f"), OpenSearchTools, app),
        KeyBinding::new(primary!("alt-f"), OpenFullText, app),
        // Book list
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("k", SelectPrevious, list),
        KeyBinding::new("j", SelectNext, list),
        KeyBinding::new("left", SelectLeft, list),
        KeyBinding::new("right", SelectRight, list),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new(primary!("up"), SelectFirst, list),
        KeyBinding::new(primary!("down"), SelectLast, list),
        KeyBinding::new("enter", ReadBook, list),
        KeyBinding::new("delete", RemoveBook, list),
        KeyBinding::new(primary!("a"), SelectAll, list),
        // Reader
        KeyBinding::new("left", PreviousChapter, reader),
        KeyBinding::new("right", NextChapter, reader),
        KeyBinding::new("pageup", PreviousChapter, reader),
        KeyBinding::new("pagedown", NextChapter, reader),
        KeyBinding::new(primary!("["), PreviousChapter, reader),
        KeyBinding::new(primary!("]"), NextChapter, reader),
        KeyBinding::new(primary!("="), IncreaseFontSize, reader),
        KeyBinding::new(primary!("+"), IncreaseFontSize, reader),
        KeyBinding::new(primary!("-"), DecreaseFontSize, reader),
        KeyBinding::new(primary!("0"), ResetFontSize, reader),
        KeyBinding::new(primary!("alt-s"), ToggleChapterList, reader),
        KeyBinding::new(primary!("d"), ToggleBookmark, reader),
        KeyBinding::new(primary!("h"), ToggleHighlight, reader),
        KeyBinding::new(primary!("f"), FindInBook, reader),
        KeyBinding::new(primary!("g"), NextMatch, reader),
        KeyBinding::new(primary!("shift-g"), PreviousMatch, reader),
    ];

    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("cmd-alt-h", HideOtherApps, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("f11", ToggleFullScreen, None),
            KeyBinding::new("f1", ShowShortcuts, None),
        ]);
    }
    cx.bind_keys(bindings);
}

/// Builds the application menu bar. Rebuilt whenever a checked item or the
/// recent libraries list changes.
pub fn app_menus(settings: &Settings) -> Vec<Menu> {
    let recent: Vec<MenuItem> = if settings.recent_libraries.is_empty() {
        vec![MenuItem::action("No Recent Libraries", OpenRecentLibrary(usize::MAX)).disabled(true)]
    } else {
        settings
            .recent_libraries
            .iter()
            .enumerate()
            .map(|(index, path)| {
                MenuItem::action(path.display().to_string(), OpenRecentLibrary(index))
            })
            .chain([
                MenuItem::separator(),
                MenuItem::action("Clear Menu", ClearRecentLibraries),
            ])
            .collect()
    };

    let sort = settings.sort();
    let sort_item =
        |name: &'static str, which: BookSort, action: Box<dyn Action>| MenuItem::Action {
            name: name.into(),
            action,
            os_action: None,
            checked: sort == which,
            disabled: false,
        };

    let mut menus = Vec::new();

    if cfg!(target_os = "macos") {
        menus.push(Menu::new("calibre-rust").items([
            MenuItem::action("About calibre-rust", About),
            MenuItem::separator(),
            MenuItem::action("Preferences…", OpenPreferences),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide calibre-rust", HideApp),
            MenuItem::action("Hide Others", HideOtherApps),
            MenuItem::action("Show All", ShowAllApps),
            MenuItem::separator(),
            MenuItem::action("Quit calibre-rust", Quit),
        ]));
    }

    let mut file = vec![
        MenuItem::action("Add Books…", AddBooks),
        MenuItem::action("Open Book File…", OpenBookFile),
        MenuItem::separator(),
        MenuItem::action("Open Library…", OpenLibrary),
        MenuItem::submenu(Menu::new("Open Recent").items(recent)),
        MenuItem::action("New Library…", NewLibrary),
        MenuItem::separator(),
        MenuItem::action("Export Selected Book…", ExportSelected),
        MenuItem::action("Export All Shown Books…", ExportAll),
        MenuItem::separator(),
        MenuItem::action("Close Window", CloseWindow),
    ];
    if !cfg!(target_os = "macos") {
        file.push(MenuItem::separator());
        file.push(MenuItem::action("Quit", Quit));
    }
    menus.push(Menu::new("File").items(file));

    let mut edit = vec![
        MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
        MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
        MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
        MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
        MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        MenuItem::separator(),
        MenuItem::action("Find", FocusSearch),
        MenuItem::action("Save", SaveChanges),
    ];
    if !cfg!(target_os = "macos") {
        edit.push(MenuItem::separator());
        edit.push(MenuItem::action("Preferences…", OpenPreferences));
    }
    menus.push(Menu::new("Edit").items(edit));

    menus.push(
        Menu::new("View").items([
            MenuItem::action("as List", ShowAsList).checked(settings.view == ViewMode::List),
            MenuItem::action("as Covers", ShowAsGrid).checked(settings.view == ViewMode::Grid),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Sort By").items([
                sort_item("Title", BookSort::Title, Box::new(SortByTitle)),
                sort_item("Author", BookSort::Author, Box::new(SortByAuthor)),
                sort_item("Date Added", BookSort::Date, Box::new(SortByDateAdded)),
                sort_item("Published", BookSort::Pubdate, Box::new(SortByPublished)),
                sort_item("Series", BookSort::Series, Box::new(SortBySeries)),
                sort_item("Rating", BookSort::Rating, Box::new(SortByRating)),
                MenuItem::separator(),
                MenuItem::action("Descending", ToggleSortDirection).checked(settings.descending),
            ])),
            MenuItem::separator(),
            MenuItem::action(
                if settings.sidebar_visible {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                },
                ToggleSidebar,
            ),
            MenuItem::action(
                if settings.inspector_visible {
                    "Hide Inspector"
                } else {
                    "Show Inspector"
                },
                ToggleDetailsPane,
            ),
            MenuItem::separator(),
            MenuItem::submenu(
                Menu::new("Appearance").items([
                    MenuItem::action("Match System", ThemeSystem)
                        .checked(settings.theme == ThemePreference::System),
                    MenuItem::action("Light", ThemeLight)
                        .checked(settings.theme == ThemePreference::Light),
                    MenuItem::action("Dark", ThemeDark)
                        .checked(settings.theme == ThemePreference::Dark),
                ]),
            ),
            MenuItem::separator(),
            MenuItem::action("Bigger Text", IncreaseFontSize),
            MenuItem::action("Smaller Text", DecreaseFontSize),
            MenuItem::action("Actual Size", ResetFontSize),
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", ToggleFullScreen),
        ]),
    );

    menus.push(Menu::new("Book").items([
        MenuItem::action("Read", ReadBook),
        MenuItem::action("Edit Metadata…", EditMetadata),
        MenuItem::action("Ask AI…", AskAi),
        MenuItem::action("Download Metadata…", DownloadMetadata),
        MenuItem::action("Attach Formats…", AttachFormats),
        MenuItem::separator(),
        MenuItem::action("Change Cover…", ChangeCover),
        MenuItem::action("Crop Cover…", CropCover),
        MenuItem::action("Edit Book…", EditEpub),
        MenuItem::separator(),
        MenuItem::action("Open in Default App", OpenInDefaultApp),
        MenuItem::action(reveal_label(), RevealInFileManager),
        MenuItem::action("Copy Title and Author", CopyBookInfo),
        MenuItem::separator(),
        MenuItem::action("Send to Device", SendToFirstDevice),
        MenuItem::action("Rescan Devices", RescanDevices),
        MenuItem::separator(),
        MenuItem::action("Remove Book…", RemoveBook),
    ]));

    menus.push(Menu::new("Tools").items(tool_items()));

    let mut window = Vec::new();
    if cfg!(target_os = "macos") {
        window.extend([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]);
    } else {
        window.push(MenuItem::action("Toggle Full Screen", ToggleFullScreen));
    }
    menus.push(Menu::new("Window").items(window));

    menus.push(Menu::new("Help").items([
        MenuItem::action("Keyboard Shortcuts", ShowShortcuts),
        MenuItem::action("calibre-rust Website", OpenProjectPage),
    ]));

    menus
}

/// The tools screen's panes, as `(menu label, action that opens it)`.
pub fn tool_actions() -> [(&'static str, Box<dyn Action>); 11] {
    [
        ("Edit Metadata in Bulk…", Box::new(OpenBulkEdit)),
        ("Convert Books…", Box::new(OpenConvert)),
        ("Search Tools…", Box::new(OpenSearchTools)),
        ("Full-text Search…", Box::new(OpenFullText)),
        ("Devices…", Box::new(OpenDevices)),
        ("Content Server…", Box::new(OpenServer)),
        ("Trash…", Box::new(OpenTrash)),
        ("Library Maintenance…", Box::new(OpenMaintenance)),
        ("Transfer Books…", Box::new(OpenTransfer)),
        ("Export Catalog…", Box::new(OpenCatalog)),
        ("Custom Columns…", Box::new(OpenCustomColumns)),
    ]
}

fn tool_items() -> Vec<MenuItem> {
    tool_actions()
        .into_iter()
        .map(|(name, action)| MenuItem::Action {
            name: name.into(),
            action,
            os_action: None,
            checked: false,
            disabled: false,
        })
        .collect()
}

/// Items in the Dock (macOS) / taskbar jump list menu.
pub fn dock_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::action("Add Books…", AddBooks),
        MenuItem::action("Open Book File…", OpenBookFile),
        MenuItem::action("Open Library…", OpenLibrary),
    ]
}

/// What the platform calls revealing a file in its file manager.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    }
}

pub const PROJECT_URL: &str = "https://github.com/flandyw/calibre";
