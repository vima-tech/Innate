use thiserror::Error;

#[derive(Debug, Error)]
pub enum InnateError {
    #[error("SQLite error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Embedding unavailable: {0}")]
    EmbeddingUnavailable(String),

    #[error("Chunk not found: {0}")]
    ChunkNotFound(String),

    #[error("Invalid state transition: {0}")]
    InvalidState(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, InnateError>;
