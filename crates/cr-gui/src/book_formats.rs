//! File-level actions scoped to a single format of the selected book.

use std::path::{Path, PathBuf};

use cr_core::{jobs, library::Library};
use gpui_kit::*;

use crate::actions::{BookFormatAction, FormatCommand};
use crate::app::{default_library_path, display_name, CalibreApp, Screen, Tone};
use crate::files;

impl CalibreApp {
    pub fn act_on_format(
        &mut self,
        action: &BookFormatAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let BookFormatAction(command, format) = action;
        let Some(id) = self.selected else { return };
        if self.screen != Screen::Library || self.editing {
            return;
        }
        match command {
            FormatCommand::Read => self.read_library_book(id, Some(format), None, window, cx),
            FormatCommand::Edit => self.begin_edit_format(Some(format), window, cx),
            FormatCommand::Remove => self.prompt_remove_format(id, format.clone(), window, cx),
            FormatCommand::Open
            | FormatCommand::Reveal
            | FormatCommand::CopyPath
            | FormatCommand::SaveCopy => {
                let path = self
                    .library
                    .as_ref()
                    .and_then(|library| library.format_path(id, format).ok().flatten())
                    .filter(|path| path.is_file());
                let Some(path) = path else {
                    self.announce(
                        format!("No {format} file was found."),
                        Tone::Error,
                        window,
                        cx,
                    );
                    return;
                };
                match command {
                    FormatCommand::Open => cx.open_with_system(&path),
                    FormatCommand::Reveal => cx.reveal_path(&path),
                    FormatCommand::SaveCopy => self.prompt_save_format_copy(path, window, cx),
                    FormatCommand::CopyPath => {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            path.display().to_string(),
                        ));
                        self.set_status("Copied the file path.", Tone::Success);
                        cx.notify();
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn prompt_save_format_copy(
        &mut self,
        source: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() {
            return;
        }
        let default = default_library_path();
        let home = default.parent().unwrap_or_else(|| Path::new("."));
        let receiver = cx.prompt_for_new_path(home, Some(&display_name(&source)));
        cx.spawn_in(window, async move |app, cx| {
            let Ok(Ok(Some(mut destination))) = receiver.await else {
                return;
            };
            let replace = destination.extension().is_some();
            if !replace {
                if let Some(extension) = source.extension() {
                    destination.set_extension(extension);
                }
            }
            let _ = app.update_in(cx, |app, window, cx| {
                app.run_job(
                    "Saving format copy",
                    window,
                    cx,
                    move |context| {
                        if source.canonicalize()?
                            == destination
                                .canonicalize()
                                .unwrap_or_else(|_| destination.clone())
                        {
                            return Err(cr_core::Error::Other(
                                "Choose a different location for the copy.".into(),
                            ));
                        }
                        let mut input = std::fs::File::open(source)?;
                        files::write_atomic(&destination, replace, context, |output| {
                            std::io::copy(&mut input, output)?;
                            Ok(())
                        })?;
                        Ok(destination)
                    },
                    |app, result, window, cx| match result {
                        Ok(destination) => {
                            app.announce(
                                format!("Saved {}.", display_name(&destination)),
                                Tone::Success,
                                window,
                                cx,
                            );
                            if app.settings.reveal_export {
                                cx.reveal_path(&destination);
                            }
                        }
                        Err(cr_core::Error::Cancelled) => {
                            app.announce("Saving a copy cancelled.", Tone::Info, window, cx);
                        }
                        Err(err) => app.announce(
                            format!("Could not save the copy: {err}"),
                            Tone::Error,
                            window,
                            cx,
                        ),
                    },
                );
            });
        })
        .detach();
    }

    pub fn prompt_attach_formats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected else { return };
        if self.screen != Screen::Library || self.editing || self.job.is_some() {
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach Formats".into()),
        });
        cx.spawn_in(window, async move |app, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                let _ = app.update_in(cx, |app, window, cx| {
                    if app
                        .library
                        .as_ref()
                        .is_some_and(|library| library.root() == root)
                    {
                        app.attach_formats(root, id, paths, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn attach_formats(
        &mut self,
        root: PathBuf,
        id: i64,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let books = files::classify(paths).books;
        if books.is_empty() {
            self.announce("No ebook files were selected.", Tone::Info, window, cx);
            return;
        }
        self.run_job(
            "Attaching formats",
            window,
            cx,
            move |context| {
                let mut library = Library::open(&root)?;
                Ok(jobs::for_each(
                    context,
                    books,
                    |path| format!("Attaching {}", display_name(path)),
                    |path| library.add_format(id, path, false),
                ))
            },
            move |app, result, window, cx| match result {
                Ok(results) => {
                    let added = results
                        .iter()
                        .filter(|item| matches!(item.result, Ok(true)))
                        .count();
                    let present = results
                        .iter()
                        .filter(|item| matches!(item.result, Ok(false)))
                        .count();
                    let failed = results.iter().filter(|item| item.result.is_err()).count();
                    let mut message =
                        format!("Attached {added} format(s). {present} already present.");
                    if let Some(error) = results.iter().find_map(|item| item.result.as_ref().err())
                    {
                        message.push_str(&format!(" {failed} failed. {error}"));
                    }
                    app.refresh_formats(id, window, cx);
                    app.announce(
                        message,
                        if failed == 0 {
                            Tone::Success
                        } else {
                            Tone::Error
                        },
                        window,
                        cx,
                    );
                }
                Err(err) => app.announce(
                    format!("Could not attach formats: {err}"),
                    Tone::Error,
                    window,
                    cx,
                ),
            },
        );
    }

    fn prompt_remove_format(
        &mut self,
        id: i64,
        format: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() {
            return;
        }
        let Some(root) = self.writable_root(window, cx) else {
            return;
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            &format!("Remove the {format} format?"),
            Some("The file goes to the library’s Trash. The book’s metadata and other formats stay in the library."),
            &["Remove Format", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |app, cx| {
            if let Ok(0) = receiver.await {
                let _ = app.update_in(cx, |app, window, cx| {
                    if app
                        .library
                        .as_ref()
                        .is_some_and(|library| library.root() == root)
                    {
                        app.remove_book_format(root, id, format, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn remove_book_format(
        &mut self,
        root: PathBuf,
        id: i64,
        format: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = format.clone();
        self.run_job(
            format!("Removing {label} format"),
            window,
            cx,
            move |_| Library::open(&root)?.remove_format(id, &format),
            move |app, result, window, cx| match result {
                Ok(()) => {
                    app.refresh_formats(id, window, cx);
                    app.announce(
                        format!("Moved the {label} format to Trash."),
                        Tone::Success,
                        window,
                        cx,
                    );
                }
                Err(err) => app.announce(
                    format!("Could not remove the {label} format: {err}"),
                    Tone::Error,
                    window,
                    cx,
                ),
            },
        );
    }

    fn refresh_formats(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_after_book_change(window, cx);
        if self.selected == Some(id) {
            self.selected_formats = self
                .library
                .as_ref()
                .and_then(|library| library.formats(id).ok())
                .unwrap_or_default();
        }
    }
}
