use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use cr_core::ai::{
    apple, AiClient, AiConfig, BookContext, Message, Provider as AiProvider, Role,
    DEFAULT_LOCAL_MODEL, DEFAULT_LOCAL_URL, DEFAULT_MODEL,
};
use cr_core::online_metadata::{Catalog, Field, Query, Source};
use cr_core::{BookUpdate, Library};
use serde_json::json;

pub(crate) fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliAiProvider {
    Auto,
    AppleIntelligence,
    Openrouter,
    LocalServer,
}

impl From<CliAiProvider> for AiProvider {
    fn from(value: CliAiProvider) -> Self {
        match value {
            CliAiProvider::Auto => Self::Auto,
            CliAiProvider::AppleIntelligence => Self::AppleIntelligence,
            CliAiProvider::Openrouter => Self::OpenRouter,
            CliAiProvider::LocalServer => Self::LocalServer,
        }
    }
}

#[derive(Debug, Args)]
pub struct AiArgs {
    /// AI provider to use (auto prefers Apple Intelligence on Mac when available)
    #[arg(long, global = true, value_enum, env = "CR_AI_PROVIDER", default_value = "auto")]
    provider: CliAiProvider,
    /// OpenRouter model ID
    #[arg(long, global = true, env = "OPENROUTER_MODEL", default_value = DEFAULT_MODEL)]
    model: String,
    /// Local OpenAI-compatible server URL (Ollama, LM Studio, etc.)
    #[arg(long, global = true, env = "CR_AI_LOCAL_URL", default_value = DEFAULT_LOCAL_URL)]
    local_url: String,
    /// Local server model ID
    #[arg(long, global = true, env = "CR_AI_LOCAL_MODEL", default_value = DEFAULT_LOCAL_MODEL)]
    local_model: String,
    #[command(subcommand)]
    command: AiCommand,
}

#[derive(Debug, Subcommand)]
enum AiCommand {
    /// Show active AI provider, privacy mode, and Apple Intelligence availability
    Status,
    /// List available models from the active provider
    Models,
    /// Verify the active AI provider without sending book data
    Test,
    /// Ask a question using a book's metadata
    Ask {
        id: i64,
        question: Vec<String>,
        /// Also send an excerpt from the book's opening
        #[arg(long)]
        excerpt: bool,
    },
    /// Preview a generated book description and tags
    Suggest {
        id: i64,
        /// Also send an excerpt from the book's opening
        #[arg(long)]
        excerpt: bool,
        /// Apply these generated fields without another confirmation
        #[arg(long, value_enum, value_delimiter = ',')]
        apply: Vec<AiField>,
    },
    /// Ask your library, translate searches, and preview organization suggestions
    Library(crate::studio::LibraryArgs),
    /// Get a recap, characters, discussion questions, translation, or reading time
    Book(crate::studio::BookArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AiField {
    Description,
    Tags,
}

pub fn ai(args: AiArgs, path: &Path, json_output: bool) -> Result<()> {
    let config = AiConfig {
        provider: args.provider.into(),
        openrouter_key: None,
        openrouter_model: args.model.clone(),
        local_url: args.local_url.clone(),
        local_key: std::env::var("CR_AI_LOCAL_API_KEY").ok(),
        local_model: args.local_model.clone(),
    };
    let client = AiClient::new(config);
    match args.command {
        AiCommand::Status => {
            let apple_status = apple::availability();
            let resolved = client.resolved_provider();
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "active_provider": resolved.label(),
                        "active_model": client.active_model(),
                        "on_device": client.is_on_device(),
                        "privacy": client.privacy_note(),
                        "apple_intelligence": {
                            "available": apple_status.is_available(),
                            "summary": apple_status.summary(),
                        },
                        "local_url": args.local_url,
                        "local_model": args.local_model,
                    }))?
                );
            } else {
                println!("Active provider: {}", client.active_label());
                println!("Privacy: {}", client.privacy_note());
                println!("Apple Intelligence: {}", apple_status.summary());
            }
        }
        AiCommand::Models => {
            let models = client.models()?;
            if json_output {
                println!("{}", serde_json::to_string_pretty(&models)?);
            } else {
                for model in models {
                    println!(
                        "{}\t{}",
                        terminal_text(&model.id),
                        terminal_text(&model.name)
                    );
                }
            }
        }
        AiCommand::Test => {
            let summary = client.test_connection()?;
            if json_output {
                println!(
                    "{}",
                    json!({"connected": true, "provider": client.provider_name(), "summary": summary})
                );
            } else {
                println!("{summary}");
            }
        }
        AiCommand::Ask {
            id,
            question,
            excerpt,
        } => {
            let question = question.join(" ");
            if question.trim().is_empty() {
                bail!("enter a question");
            }
            let library = Library::open(path)?;
            let context = context(&library, id, excerpt, client.context_budget().excerpt_chars)?;
            let answer = client.chat_stream(
                &[context.system_message(), Message::new(Role::User, question)],
                |text| {
                    if !json_output {
                        print!("{}", terminal_text(text));
                        io::stdout().flush()?;
                    }
                    Ok(())
                },
            )?;
            if json_output {
                println!(
                    "{}",
                    json!({"id": id, "answer": answer.text, "model": answer.model, "usage": answer.usage, "provider": client.provider_name()})
                );
            } else {
                println!();
                if let Some(usage) = answer.usage {
                    eprintln!(
                        "{} input + {} output tokens",
                        usage.prompt_tokens, usage.completion_tokens
                    );
                }
            }
        }
        AiCommand::Suggest { id, excerpt, apply } => {
            let mut library = Library::open(path)?;
            let suggestion =
                client.suggest(&context(&library, id, excerpt, client.context_budget().excerpt_chars)?)?;
            let mut update = BookUpdate::default();
            for field in apply {
                match field {
                    AiField::Description if !suggestion.description.is_empty() => {
                        update.comments = Some(Some(suggestion.description_html()));
                    }
                    AiField::Tags if !suggestion.tags.is_empty() => {
                        update.tags = Some(suggestion.tags.clone());
                    }
                    _ => {}
                }
            }
            let applied = update != BookUpdate::default();
            if applied {
                library.update_book(id, &update)?;
            }
            if json_output {
                println!(
                    "{}",
                    json!({"id": id, "suggestion": suggestion, "applied": applied, "provider": client.provider_name()})
                );
            } else {
                println!(
                    "Description: {}\nTags: {}",
                    terminal_text(&suggestion.description),
                    terminal_text(&suggestion.tags.join(", "))
                );
                if applied {
                    println!("Applied selected fields to #{id}.");
                }
            }
        }
        AiCommand::Library(args) => crate::studio::library(args, &client, path, json_output)?,
        AiCommand::Book(args) => crate::studio::book(args, &client, path, json_output)?,
    }
    Ok(())
}

fn context(library: &Library, id: i64, excerpt: bool, limit: usize) -> Result<BookContext> {
    let metadata = library.get_metadata(id)?;
    let text = excerpt.then(|| library.read_book(id, None)).transpose()?;
    Ok(BookContext::new_with_budget(&metadata, text.as_ref(), limit))
}

#[cfg(test)]
mod tests {
    use super::terminal_text;

    #[test]
    fn terminal_text_drops_controls_but_keeps_unicode_and_newlines() {
        assert_eq!(
            terminal_text("Hi\u{001B}[31m\u{009D}江\r\u{0007}\nHello"),
            "Hi[31m江\nHello"
        );
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    OpenLibrary,
    GoogleBooks,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MetadataField {
    Title,
    Authors,
    Publisher,
    Date,
    Description,
    Tags,
    Isbn,
    Cover,
}

impl From<MetadataField> for Field {
    fn from(field: MetadataField) -> Self {
        match field {
            MetadataField::Title => Self::Title,
            MetadataField::Authors => Self::Authors,
            MetadataField::Publisher => Self::Publisher,
            MetadataField::Date => Self::Date,
            MetadataField::Description => Self::Description,
            MetadataField::Tags => Self::Tags,
            MetadataField::Isbn => Self::Isbn,
            MetadataField::Cover => Self::Cover,
        }
    }
}

#[derive(Debug, Args)]
pub struct MetadataArgs {
    /// Library book ID (defaults search fields from the book)
    id: Option<i64>,
    /// Book title
    #[arg(short, long)]
    title: Option<String>,
    /// Author name
    #[arg(short, long, visible_alias = "authors")]
    author: Option<String>,
    /// ISBN (takes precedence over title and author; use an empty value to omit)
    #[arg(short, long)]
    isbn: Option<String>,
    /// Catalog to search
    #[arg(long, value_enum, default_value = "open-library")]
    source: Provider,
    /// Candidate ID returned by a previous search
    #[arg(long)]
    result: Option<String>,
    /// Apply only these fields to the library book
    #[arg(long, value_enum, value_delimiter = ',', requires_all = ["id", "result"])]
    apply: Vec<MetadataField>,
    /// Save the selected candidate's cover to this file
    #[arg(short, long, requires = "result")]
    cover: Option<PathBuf>,
}

pub fn metadata(args: MetadataArgs, path: &Path, json_output: bool) -> Result<()> {
    let mut library = args.id.map(|_| Library::open(path)).transpose()?;
    let current = library
        .as_ref()
        .zip(args.id)
        .map(|(library, id)| library.get_metadata(id))
        .transpose()?;
    let mut query = current.as_ref().map(Query::from_book).unwrap_or_default();
    if args.title.is_some() || args.author.is_some() {
        query.isbn.clear();
    }
    if let Some(title) = args.title {
        query.title = title;
    }
    if let Some(author) = args.author {
        query.author = author;
    }
    if let Some(isbn) = args.isbn {
        query.isbn = isbn;
    }
    let source = match args.source {
        Provider::OpenLibrary => Source::OpenLibrary,
        Provider::GoogleBooks => Source::GoogleBooks,
    };
    let catalog = Catalog::default();
    let results = catalog.search(source, &query)?;
    let Some(result) = args.result else {
        if json_output {
            let results = results
                .iter()
                .map(|candidate| candidate.to_json())
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else if results.is_empty() {
            println!("No results from {source}.");
        } else {
            for candidate in results {
                println!(
                    "{}\t{}\t{}",
                    candidate.id,
                    candidate.title,
                    candidate.authors.join(", ")
                );
            }
        }
        return Ok(());
    };
    let mut candidate = results
        .into_iter()
        .find(|candidate| candidate.id == result)
        .ok_or_else(|| anyhow!("candidate {result} is not in these search results"))?;
    let fields: Vec<_> = args.apply.into_iter().map(Field::from).collect();
    if fields.is_empty() {
        catalog.enrich(&mut candidate)?;
    } else {
        catalog.enrich_fields(&mut candidate, &fields)?;
    }
    let cover = if args.cover.is_some() || fields.contains(&Field::Cover) {
        Some(
            catalog
                .cover(&candidate, false)?
                .ok_or_else(|| anyhow!("candidate has no cover"))?,
        )
    } else {
        None
    };
    if let (Some(path), Some(bytes)) = (args.cover, &cover) {
        std::fs::write(path, bytes)?;
    }
    let mut applied = false;
    if !fields.is_empty() {
        if let (Some(library), Some(id), Some(original)) = (&mut library, args.id, &current) {
            let patch = candidate.update(original, &fields, cover);
            if patch == BookUpdate::default() {
                bail!("none of the selected fields are available for this result");
            }
            library.update_book(id, &patch)?;
            applied = true;
        }
    }
    let proposal = candidate.to_json();
    let preview = json!({
        "current": current.as_ref().map(|book| book.to_json()),
        "proposed": proposal,
        "applied": applied
    });
    println!("{}", serde_json::to_string_pretty(&preview)?);
    Ok(())
}
