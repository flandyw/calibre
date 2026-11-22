//! calibre-rust core: a fast, small engine for calibre-compatible ebook
//! libraries.
//!
//! ```no_run
//! use cr_core::{Library, ListOptions};
//!
//! # fn main() -> cr_core::Result<()> {
//! let mut library = Library::open_or_create(std::path::Path::new("/tmp/Calibre Library"))?;
//! let added = library.add_book(std::path::Path::new("book.epub"))?;
//! println!("added #{}: {}", added.id, added.title);
//! for book in library.list_books(&ListOptions::default())? {
//!     println!("{}: {}", book.id, book.title);
//! }
//! # Ok(())
//! # }
//! ```

pub mod device;
pub mod error;
pub mod formats;
pub mod library;
pub mod metadata;
pub mod opf;
pub mod text;
pub mod xml;

pub use device::{Device, DeviceKind, SendOptions, SendReport, SentBook};
pub use error::{Error, Result};
pub use formats::{BookText, Chapter};
pub use library::{AddResult, BookSort, BookUpdate, Library, LibraryStats, ListOptions};
pub use metadata::{BookSummary, ImportedBook, Metadata};
