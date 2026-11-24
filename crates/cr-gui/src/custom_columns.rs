//! Custom column definitions and values in the active calibre library.

use cr_core::library::CustomColumn;
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::*;

use crate::app::{set_input, set_textarea, split_commas, text_area, text_input, CalibreApp, Tone};

pub struct CustomColumns {
    pub columns: Vec<CustomColumn>,
    pub selected: Option<i64>,
    pub lookup: Entity<InputState>,
    pub heading: Entity<InputState>,
    pub choices: Entity<InputState>,
    pub datatype: &'static str,
    pub multiple: bool,
    pub value: Entity<TextareaState>,
    pub value_book: Option<i64>,
}

impl CustomColumns {
    pub fn new(window: &mut Window, cx: &mut Context<CalibreApp>) -> Self {
        Self {
            columns: Vec::new(),
            selected: None,
            lookup: text_input("e.g. reading_status", window, cx),
            heading: text_input("e.g. Reading Status", window, cx),
            choices: text_input("e.g. To read, Reading, Finished", window, cx),
            datatype: "text",
            multiple: false,
            value: text_area("Leave blank to clear the value", 3, window, cx),
            value_book: None,
        }
    }

    pub fn clear(&mut self) {
        self.columns.clear();
        self.selected = None;
        self.value_book = None;
    }

    pub fn selected_column(&self) -> Option<&CustomColumn> {
        self.columns
            .iter()
            .find(|column| Some(column.id) == self.selected)
    }
}

impl CalibreApp {
    pub fn load_custom_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = &self.library else { return };
        match library.custom_columns() {
            Ok(columns) => {
                self.custom_columns.columns = columns;
                let id = self
                    .custom_columns
                    .selected_column()
                    .or_else(|| self.custom_columns.columns.first())
                    .map(|column| column.id);
                self.select_custom_column(id, window, cx);
            }
            Err(err) => {
                self.custom_columns.clear();
                self.announce(
                    format!("Could not load custom columns: {err}"),
                    Tone::Error,
                    window,
                    cx,
                );
            }
        }
    }

    pub fn select_custom_column(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.custom_columns.selected = id;
        self.custom_columns.value_book = None;
        let mut value = String::new();
        if let (Some(library), Some(book), Some(column)) = (
            &self.library,
            self.selected,
            self.custom_columns.selected_column(),
        ) {
            match library.custom_values(book) {
                Ok(values) => {
                    if let Some(items) = values.get(&column.label) {
                        value = items.join(", ");
                    }
                    self.custom_columns.value_book = Some(book);
                }
                Err(err) => self.announce(
                    format!("Could not read the custom value: {err}"),
                    Tone::Error,
                    window,
                    cx,
                ),
            }
        }
        set_textarea(&self.custom_columns.value, value, window, cx);
        cx.notify();
    }

    pub fn create_custom_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.writable_root(window, cx).is_none() {
            return;
        }
        let form = &self.custom_columns;
        let lookup = form.lookup.read(cx).value().trim().to_string();
        let name = form.heading.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.announce("Enter a column heading.", Tone::Info, window, cx);
            return;
        }
        let choices = split_commas(&form.choices.read(cx).value());
        let Some(library) = &mut self.library else {
            return;
        };
        match library.create_custom_column(
            &lookup,
            &name,
            form.datatype,
            form.datatype == "text" && form.multiple,
            &choices,
        ) {
            Ok(column) => {
                self.custom_columns.selected = Some(column.id);
                set_input(&self.custom_columns.lookup, String::new(), window, cx);
                set_input(&self.custom_columns.heading, String::new(), window, cx);
                set_input(&self.custom_columns.choices, String::new(), window, cx);
                self.load_custom_columns(window, cx);
                self.announce(
                    format!("Created #{}.", column.label),
                    Tone::Success,
                    window,
                    cx,
                );
            }
            Err(err) => self.announce(
                format!("Could not create the column: {err}"),
                Tone::Error,
                window,
                cx,
            ),
        }
    }

    pub fn save_custom_column_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.custom_columns.value_book != self.selected {
            self.load_custom_columns(window, cx);
            return;
        }
        if self.job.is_some() || self.writable_root(window, cx).is_none() {
            return;
        }
        let (Some(book), Some(column), Some(library)) = (
            self.selected,
            self.custom_columns.selected_column(),
            self.library.as_mut(),
        ) else {
            return;
        };
        let value = self.custom_columns.value.read(cx).value().to_string();
        match library.set_custom_value(book, &column.label, &value) {
            Ok(()) => {
                self.refresh_after_book_change(window, cx);
                self.load_custom_columns(window, cx);
                self.announce("Custom value saved.", Tone::Success, window, cx);
            }
            Err(err) => self.announce(
                format!("Could not save the value: {err}"),
                Tone::Error,
                window,
                cx,
            ),
        }
    }

    pub fn prompt_delete_custom_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let Some(column) = self.custom_columns.selected_column().cloned() else {
            return;
        };
        let response = window.prompt(
            PromptLevel::Warning,
            &format!("Delete “{}”?", column.name),
            Some("This permanently removes the column and all its values from the library."),
            &["Delete Column", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |app, cx| {
            if let Ok(0) = response.await {
                let _ = app.update_in(cx, |app, window, cx| {
                    if app.job.is_some() {
                        return;
                    }
                    let Some(library) = app
                        .library
                        .as_mut()
                        .filter(|library| library.root() == root)
                    else {
                        return;
                    };
                    let result = library.custom_columns().and_then(|columns| {
                        if !columns.contains(&column) {
                            return Err(cr_core::Error::Other("The column has changed.".into()));
                        }
                        library.delete_custom_column(&column.label)
                    });
                    match result {
                        Ok(()) => {
                            app.refresh_after_book_change(window, cx);
                            app.load_custom_columns(window, cx);
                            app.announce(
                                format!("Deleted #{}.", column.label),
                                Tone::Success,
                                window,
                                cx,
                            );
                        }
                        Err(err) => app.announce(
                            format!("Could not delete the column: {err}"),
                            Tone::Error,
                            window,
                            cx,
                        ),
                    }
                });
            }
        })
        .detach();
    }
}
