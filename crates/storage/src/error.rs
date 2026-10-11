use std::io;
use std::path::PathBuf;

/// Errors produced while decoding bytes read from disk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("varint overflow")]
    VarintOverflow,
    #[error("length exceeds remaining input")]
    LengthOutOfBounds,
    #[error("invalid UTF-8")]
    InvalidUtf8,
    #[error("invalid tag {0}")]
    InvalidTag(u8),
    #[error("unsupported version {0}")]
    UnsupportedVersion(u16),
    #[error("nesting too deep")]
    DepthExceeded,
    #[error("checksum mismatch")]
    Checksum,
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// I/O failure with the file it concerned. The path is for operators'
    /// logs; the HTTP layer never forwards it to clients.
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("corrupt data in {what}: {source}")]
    Corrupt {
        what: String,
        #[source]
        source: DecodeError,
    },
    #[error("compression error: {0}")]
    Compression(io::Error),
    /// The ingest queue is full. Callers should retry later (HTTP 429).
    #[error("ingest queue is full")]
    QueueFull,
    /// A batch larger than the whole queue can never be accepted (HTTP 413).
    #[error("batch of {0} events exceeds queue capacity")]
    BatchTooLarge(usize),
    /// Storage is shutting down or has failed (HTTP 503).
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    /// Storage could not write the batch, so it was not acknowledged and
    /// is not stored (HTTP 503). The fault is safe to show to operators.
    #[error("storage write failed: {0}")]
    WriteFailed(crate::fault::StorageFault),
    /// Like [`StorageError::WriteFailed`], but the batch had fully reached
    /// the WAL and cutting it off again could not be made durable: it is
    /// not acknowledged, yet may reappear after a power loss (HTTP 503).
    #[error("storage write failed, outcome unknown: {0}")]
    WriteUncertain(crate::fault::StorageFault),
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

pub(crate) trait IoContext<T> {
    fn ctx(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| StorageError::Io { path: path.into(), source })
    }
}

pub(crate) fn corrupt(what: impl Into<String>, source: DecodeError) -> StorageError {
    StorageError::Corrupt { what: what.into(), source }
}
