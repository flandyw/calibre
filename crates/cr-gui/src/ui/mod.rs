//! Rendering for the main window. State and behaviour live in [`crate::app`].

pub(crate) mod ai;
mod chrome;
mod crop;
mod custom_columns;
mod epub;
mod inspector;
mod library;
mod library_tools;
pub(crate) mod online;
mod preferences;
mod reader;
mod tools;
mod widgets;

use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::APP_CONTEXT;
use crate::app::{CalibreApp, Screen};

pub const SIDEBAR_WIDTH: f32 = 228.;
pub const INSPECTOR_WIDTH: f32 = 360.;
pub const LIST_ROW_HEIGHT: f32 = 56.;
pub const GRID_CARD_WIDTH: f32 = 132.;
pub const GRID_GAP: f32 = 18.;
pub const GRID_PADDING: f32 = 20.;

impl Render for CalibreApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_window_chrome(window);
        // Shortcuts and menu actions are dispatched from the focused element,
        // so keep focus inside this view when nothing else has claimed it.
        if window.focused(cx).is_none() {
            self.focus_main(window, cx);
        }

        let (background, foreground) = (cx.theme().background, cx.theme().foreground);

        let body = match self.screen {
            Screen::Library if self.library.is_none() => chrome::welcome(self, cx),
            Screen::Library => library::library_body(self, cx),
            Screen::Reader => reader::reader_body(self, cx),
            Screen::Epub => epub::epub_body(self, cx),
            Screen::Crop => crop::crop_body(self, cx),
            Screen::Tools => tools::tools_body(self, cx),
            Screen::Preferences => preferences::preferences_body(self, cx),
        };

        let root = div()
            .key_context(APP_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.open_paths(paths.paths().to_vec(), window, cx);
            }));

        self.register_actions(root, cx)
            .child(chrome::title_bar(self, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .overflow_hidden()
                    .child(body),
            )
            .child(chrome::status_bar(self, cx))
            .child(chrome::drop_overlay(self, cx))
            .when(self.show_shortcuts, |this| {
                this.child(chrome::shortcuts_overlay(window, cx))
            })
    }
}
