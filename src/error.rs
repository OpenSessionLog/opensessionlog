use thiserror::Error;

#[derive(Debug, Error)]
pub enum OslError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("UUID error: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("connector error: {0}")]
    Connector(String),

    #[error("embed error: {0}")]
    Embed(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("already exists: {0}")]
    AlreadyExists(String),

    #[error("usage error: {0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, OslError>;
