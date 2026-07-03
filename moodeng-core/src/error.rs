use thiserror::Error;

#[derive(Debug, Error)]
pub enum MoodengError {
    #[error("SQL parse error: {0}")]
    Parse(String),

    #[error("execution error: {0}")]
    Execution(String),

    #[error("table '{0}' does not exist")]
    TableNotFound(String),

    #[error("column '{0}' does not exist")]
    ColumnNotFound(String),

    #[error("type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },

    #[error("duplicate key: {0}")]
    DuplicateKey(String),

    #[error("index '{0}' already exists")]
    IndexExists(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, MoodengError>;
