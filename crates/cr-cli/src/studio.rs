use std::path::Path;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use cr_core::ai::{
    reading_minutes, recap_messages, BookAction, BookContext, CatalogContext, CleanupField,
    LibraryAnswer, Message, OpenRouter, Role, CLEANUP_LIMIT,
};
use cr_core::{Library, ListOptions};
use serde_json::json;

use crate::online::terminal_text;

#[derive(Debug, Args)]
pub struct LibraryArgs {
    #[command(subcommand)]
    command: LibraryCommand,
}

#[derive(Debug, Subcommand)]
enum LibraryCommand {
    /// Ask a question using a bounded sample of the catalog
    Ask {
        #[arg(required = true)]
        question: Vec<String>,
    },
    /// Translate plain language into a calibre query
    Search {
        /// Run the draft query in addition to showing it
        #[arg(long)]
        run: bool,
        #[arg(required = true)]
        request: Vec<String>,
    },
    /// Recommend books using ratings, recent reads, or a chosen book
    Next {
        #[arg(long)]
        book: Option<i64>,
        preference: Vec<String>,
    },
    /// Preview shelves without writing to the library
    Shelves {
        #[arg(long)]
        filter: Option<String>,
        idea: Vec<String>,
    },
    /// Preview field-level cleanup for selected books
    Cleanup {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
}

pub fn library(args: LibraryArgs, client: &OpenRouter, path: &Path, machine: bool) -> Result<()> {
    let library = Library::open(path)?;
    match args.command {
        LibraryCommand::Search { request, run } => {
            let draft = client.draft_search(&library, &request.join(" "))?;
            if !machine {
                println!(
                    "Query: {}\n{}",
                    terminal_text(&draft.query),
                    terminal_text(&draft.note)
                );
            }
            let matches = if run && !draft.query.is_empty() {
                Some(library.list_books(&ListOptions {
                    query: Some(draft.query.clone()),
                    ..Default::default()
                })?)
            } else {
                None
            };
            if machine {
                let matches =
                    matches.map(|rows| rows.iter().map(|book| book.to_json()).collect::<Vec<_>>());
                println!(
                    "{}",
                    json!({"query": draft.query, "note": draft.note, "matches": matches})
                );
            } else {
                for book in matches.into_iter().flatten() {
                    println!("#{}\t{}", book.id, terminal_text(&book.title));
                }
            }
        }
        LibraryCommand::Ask { question } => {
            let books = library.list_books(&ListOptions::default())?;
            let catalog = CatalogContext::new(&books, &[], None);
            let answer =
                client.librarian(&catalog, &[Message::new(Role::User, question.join(" "))])?;
            print_answer(&catalog, answer, machine)?;
        }
        LibraryCommand::Next { book, preference } => {
            if let Some(id) = book {
                library.get_summary(id)?;
            }
            let books = library.list_books(&ListOptions::default())?;
            let catalog = CatalogContext::new(&books, &library.recent_read_ids(12)?, book);
            print_answer(
                &catalog,
                client.recommend(&catalog, &preference.join(" "))?,
                machine,
            )?;
        }
        LibraryCommand::Shelves { filter, idea } => {
            let books = library.list_books(&ListOptions {
                query: filter,
                ..Default::default()
            })?;
            let catalog = CatalogContext::new(&books, &[], None);
            let shelves = client.shelves(&catalog, &idea.join(" "))?;
            let rows = shelves
                .iter()
                .map(|shelf| json!({"shelf": shelf, "query": shelf.query()}))
                .collect::<Vec<_>>();
            print_json(
                &json!({"included":catalog.included(), "total":catalog.total, "shelves":rows}),
                machine,
            )?;
        }
        LibraryCommand::Cleanup { mut ids } => {
            ids.sort_unstable();
            ids.dedup();
            if ids.len() > CLEANUP_LIMIT {
                bail!("select 1–{CLEANUP_LIMIT} books");
            }
            let books = ids
                .into_iter()
                .map(|id| library.get_summary(id))
                .collect::<cr_core::Result<Vec<_>>>()?;
            let drafts = client.cleanup(&books)?;
            let rows = drafts.iter().filter_map(|draft| {
                let book = books.iter().find(|book| book.id == draft.id)?;
                let fields = CleanupField::ALL.into_iter().filter_map(|field| {
                    let (current, proposed) = draft.values(book, field)?;
                    Some(json!({"field":field.label(), "current":current, "proposed":proposed}))
                }).collect::<Vec<_>>();
                Some(json!({"id":draft.id,"title":book.title,"reason":draft.reason,"fields":fields}))
            }).collect::<Vec<_>>();
            print_json(&json!(rows), machine)?;
        }
    }
    Ok(())
}

fn print_answer(catalog: &CatalogContext, answer: LibraryAnswer, machine: bool) -> Result<()> {
    if machine {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"included":catalog.included(),"total":catalog.total,"result":answer})
            )?
        );
    } else {
        println!(
            "Catalog: {} of {} books\n{}",
            catalog.included(),
            catalog.total,
            terminal_text(&answer.answer)
        );
        for book in answer.books {
            println!(
                "#{} {} — {}",
                book.id,
                terminal_text(&book.title),
                terminal_text(&book.reason)
            );
        }
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct BookArgs {
    #[command(subcommand)]
    command: BookCommand,
}

#[derive(Debug, Subcommand)]
enum BookCommand {
    /// Recap excerpts through the chosen chapter without sending later chapters
    Recap {
        id: i64,
        #[arg(long)]
        through: usize,
    },
    /// Generate a spoiler-light character list
    Characters {
        id: i64,
        #[arg(long)]
        excerpt: bool,
    },
    /// Generate book-club discussion questions
    Discussion {
        id: i64,
        #[arg(long)]
        excerpt: bool,
    },
    /// Preview a translation of a book description
    Translate {
        id: i64,
        #[arg(long)]
        language: String,
    },
    /// Estimate reading time locally
    ReadingTime { id: i64 },
}

pub fn book(args: BookArgs, client: &OpenRouter, path: &Path, machine: bool) -> Result<()> {
    let library = Library::open(path)?;
    let messages = match args.command {
        BookCommand::ReadingTime { id } => {
            let minutes = reading_minutes(&library.read_book(id, None)?);
            if machine {
                println!(
                    "{}",
                    json!({"id":id,"minutes":minutes,"words_per_minute":230})
                );
            } else {
                println!("About {minutes} minutes at 230 words per minute (computed locally).");
            }
            return Ok(());
        }
        BookCommand::Recap { id, through } => recap_messages(
            &library.get_metadata(id)?,
            &library.read_book(id, None)?,
            through,
        )?,
        BookCommand::Characters { id, excerpt } => {
            action_messages(&library, id, BookAction::Characters, excerpt)?
        }
        BookCommand::Discussion { id, excerpt } => {
            action_messages(&library, id, BookAction::Discussion, excerpt)?
        }
        BookCommand::Translate { id, language } => {
            action_messages(&library, id, BookAction::Translate(&language), false)?
        }
    };
    let reply = client.chat_stream(&messages, |_| Ok(()))?;
    if machine {
        println!("{}", serde_json::to_string_pretty(&reply)?);
    } else {
        println!("{}", terminal_text(&reply.text));
    }
    Ok(())
}

fn print_json(value: &serde_json::Value, machine: bool) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    println!("{}", if machine { text } else { terminal_text(&text) });
    Ok(())
}

fn action_messages(
    library: &Library,
    id: i64,
    action: BookAction<'_>,
    excerpt: bool,
) -> Result<Vec<Message>> {
    let metadata = library.get_metadata(id)?;
    if let BookAction::Translate(language) = action {
        if language.trim().is_empty() || metadata.comments.is_none() {
            bail!("translation needs a target language and an existing description");
        }
    }
    let text = excerpt.then(|| library.read_book(id, None)).transpose()?;
    let context = BookContext::new(&metadata, text.as_ref());
    Ok(vec![
        context.system_message(),
        Message::new(Role::User, action.prompt()),
    ])
}
