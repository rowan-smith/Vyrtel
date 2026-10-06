use storage::StorageError;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message} (at position {position})")]
pub struct ParseError {
    pub message: String,
    /// Byte offset into the query text.
    pub position: usize,
}

impl ParseError {
    pub fn new(message: impl Into<String>, position: usize) -> Self {
        Self { message: message.into(), position }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("query exceeded its time limit of {0} ms")]
    Timeout(u64),
    #[error("query would scan more than {0} bytes; narrow the time range or add filters")]
    TooExpensive(u64),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
