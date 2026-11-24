mod actions;
mod app;
mod assets;
mod book_formats;
mod custom_columns;
mod files;
mod library_tools;
mod settings;
mod tools;
mod ui;

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

use crate::actions::*;
use crate::files::Launch;
use crate::settings::{SavedBounds, Settings};

fn main() {
    let launch = match files::launch(std::env::args_os().skip(1)) {
        Ok(Launch::Help) => {
            println!(
                "Usage: calibre-rust [PATH ...]\n\
                 Open a library folder or import ebook files.\n\n\
                 --read PATH       Read an ebook file directly\n\
                 --open-book       Choose an ebook file to read\n\
                 --choose-library  Choose a library folder\n\
                 --add-books       Choose ebooks to import\n\
                 --preferences     Open Preferences\n\
                 --version         Print the app version"
            );
            return;
        }
        Ok(Launch::Version) => {
            println!("calibre-rust {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("calibre-rust: {error}\nSee calibre-rust --help.");
            std::process::exit(2);
        }
    };
    let application = gpui_kit::application().with_assets(assets::AppAssets);

    // Files and folders handed to the app by the OS (Finder "Open With",
    // dropping onto the Dock icon, `open -a`) can arrive before the window
    // exists, so queue them until it does.
    let (open_tx, mut open_rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
    application.on_open_urls(move |urls| {
        let paths: Vec<PathBuf> = urls
            .iter()
            .filter_map(|url| files::path_from_url(url))
            .collect();
        if !paths.is_empty() {
            let _ = open_tx.unbounded_send(paths);
        }
    });
    application.on_reopen(|cx| {
        if let Some(window) = cx.windows().first().copied() {
            cx.activate(true);
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
    });

    application.run(move |cx| {
        // Required before any gpui-kit component is used.
        gpui_kit::init(cx);

        let settings = Settings::load();
        actions::bind_keys(cx);
        cx.set_menus(actions::app_menus(&settings));
        cx.set_dock_menu(actions::dock_menu());
        register_app_actions(cx);

        let options = window_options(&settings, cx);
        let (window, view) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| app::CalibreApp::new(settings, window, cx))
        })
        .expect("failed to open the calibre-rust window");

        // Forward actions that arrive while no window is active (Dock menu
        // items, the app menu after the window was hidden) to the main window.
        let target = view.clone();
        cx.on_action(move |action: &AddBooks, cx| forward_to_window(window, &target, action, cx));
        let target = view.clone();
        cx.on_action(move |action: &OpenLibrary, cx| {
            forward_to_window(window, &target, action, cx)
        });
        let target = view.clone();
        cx.on_action(move |action: &NewLibrary, cx| forward_to_window(window, &target, action, cx));
        let target = view.clone();
        cx.on_action(move |action: &OpenPreferences, cx| {
            forward_to_window(window, &target, action, cx)
        });
        let target = view.clone();
        cx.on_action(move |action: &OpenBookFile, cx| {
            forward_to_window(window, &target, action, cx)
        });

        let quitting = view.clone();
        cx.on_app_quit(move |cx| {
            let view = quitting.clone();
            let _ = window.update(cx, |_, window, cx| {
                view.update(cx, |app, _| app.prepare_for_quit(window));
            });
            async {}
        })
        .detach();

        let target = view.clone();
        cx.spawn(async move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                target.update(cx, |app, cx| match launch {
                    Launch::Paths(paths) if !paths.is_empty() => {
                        app.open_external_paths(paths, window, cx)
                    }
                    Launch::Preferences => app.open_preferences(window, cx),
                    Launch::ChooseLibrary => app.prompt_open_library(window, cx),
                    Launch::AddBooks => app.prompt_add_books(window, cx),
                    Launch::OpenBook => app.prompt_read_file(window, cx),
                    Launch::Read(path) => app.read_file(path, window, cx),
                    _ => {}
                });
            });
        })
        .detach();

        cx.spawn(async move |cx| {
            while let Some(paths) = open_rx.next().await {
                let view = view.clone();
                let result = window.update(cx, |_, window, cx| {
                    window.activate_window();
                    view.update(cx, |app, cx| app.open_external_paths(paths, window, cx));
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();

        // calibre-rust is a single-window app: closing the window quits.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Actions that only need the application, not the window.
fn register_app_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &OpenProjectPage, cx| cx.open_url(PROJECT_URL));
}

fn forward_to_window(
    window: AnyWindowHandle,
    view: &Entity<app::CalibreApp>,
    action: &dyn Action,
    cx: &mut App,
) {
    cx.activate(true);
    let action = action.boxed_clone();
    let _ = window.update(cx, |_, window, cx| {
        window.activate_window();
        // Focus must be inside the app view, or the action would bubble
        // straight back to this global handler.
        if view.update(cx, |app, cx| app.focus_main(window, cx)) {
            window.dispatch_action(action, cx);
        }
    });
}

fn window_options(settings: &Settings, cx: &mut App) -> WindowOptions {
    let default_size = size(px(1280.), px(820.));
    let bounds = settings
        .window
        .filter(|saved| settings.restore_window && saved.width >= 860.0 && saved.height >= 560.0)
        .and_then(|saved| restore_bounds(saved, cx))
        .unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, default_size, cx)));

    WindowOptions {
        window_bounds: Some(bounds),
        window_min_size: Some(size(px(860.), px(560.))),
        app_id: Some("calibre-rust".to_string()),
        ..TitleBar::window_options()
    }
}

/// Restores saved window bounds if they are still on a connected display.
fn restore_bounds(saved: SavedBounds, cx: &App) -> Option<WindowBounds> {
    let bounds = Bounds {
        origin: point(px(saved.x), px(saved.y)),
        size: size(px(saved.width), px(saved.height)),
    };
    let visible = cx
        .displays()
        .iter()
        .any(|display| display.bounds().intersects(&bounds));
    if !visible {
        return None;
    }
    Some(if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}
