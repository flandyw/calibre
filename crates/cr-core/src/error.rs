//! Error types shared by the calibre-rust engine.

/// Errors produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("not a calibre library (no metadata.db): {0}")]
    InvalidLibrary(String),

    #[error("book {0} not found")]
    BookNotFound(i64),

    #[error("unsupported ebook format: {0}")]
    UnsupportedFormat(String),

    #[error("invalid EPUB: {0}")]
    InvalidEpub(String),

    #[error("Kindle ebook: {0}")]
    Kindle(String),

    #[error("invalid OPF: {0}")]
    InvalidOpf(String),

    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    #[error("XML error: {0}")]
    Xml(String),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("device error: {0}")]
    Device(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    ReadOnly(String),

    #[error("cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
