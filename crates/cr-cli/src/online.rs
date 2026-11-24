use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use cr_core::ai::{BookContext, Message, OpenRouter, Role, DEFAULT_MODEL};
use cr_core::online_metadata::{Catalog, Field, Query, Source};
use cr_core::{BookUpdate, Library};
use serde_json::json;

#[derive(Debug, Args)]
pub struct AiArgs {
    /// OpenRouter model ID
    #[arg(long, global = true, env = "OPENROUTER_MODEL", default_value = DEFAULT_MODEL)]
    model: String,
    #[command(subcommand)]
    command: AiCommand,
}

#[derive(Debug, Subcommand)]
enum AiCommand {
    /// List available OpenRouter models
    Models,
    /// Verify OPENROUTER_API_KEY without sending book data
    Test,
    /// Ask a question using a book's metadata
    Ask {
        id: i64,
        question: Vec<String>,
        /// Also send up to 12,000 characters from the book's opening
        #[arg(long)]
        excerpt: bool,
    },
    /// Preview a generated book description and tags
    Suggest {
        id: i64,
        /// Also send up to 12,000 characters from the book's opening
        #[arg(long)]
        excerpt: bool,
        /// Apply these generated fields without another confirmation
        #[arg(long, value_enum, value_delimiter = ',')]
        apply: Vec<AiField>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AiField {
    Description,
    Tags,
}

pub fn ai(args: AiArgs, path: &Path, json_output: bool) -> Result<()> {
    let client = OpenRouter::new(None, &args.model);
    match args.command {
        AiCommand::Models => {
            let models = client.models()?;
            if json_output {
                println!("{}", serde_json::to_string_pretty(&models)?);
            } else {
                for model in models {
                    println!("{}\t{}", model.id, model.name);
                }
            }
        }
        AiCommand::Test => {
            client.test_connection()?;
            if json_output {
                println!("{}", json!({"connected": true}));
            } else {
                println!("OpenRouter API key verified.");
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
            let context = context(&library, id, excerpt)?;
            let answer =
                client.chat(&[context.system_message(), Message::new(Role::User, question)])?;
            if json_output {
                println!("{}", json!({"id": id, "answer": answer}));
            } else {
                println!("{answer}");
            }
        }
        AiCommand::Suggest { id, excerpt, apply } => {
            let mut library = Library::open(path)?;
            let suggestion = client.suggest(&context(&library, id, excerpt)?)?;
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
                    json!({"id": id, "suggestion": suggestion, "applied": applied})
                );
            } else {
                println!(
                    "Description: {}\nTags: {}",
                    suggestion.description,
                    suggestion.tags.join(", ")
                );
                if applied {
                    println!("Applied selected fields to #{id}.");
                }
            }
        }
    }
    Ok(())
}

fn context(library: &Library, id: i64, excerpt: bool) -> Result<BookContext> {
    let metadata = library.get_metadata(id)?;
    let text = excerpt.then(|| library.read_book(id, None)).transpose()?;
    Ok(BookContext::new(&metadata, text.as_ref()))
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
    if fields.is_empty() || fields.contains(&Field::Description) {
        catalog.enrich(&mut candidate)?;
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
