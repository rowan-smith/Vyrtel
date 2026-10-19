//! Ingestion formats → common telemetry model.
//!
//! * [`native`] — Vyrtel's JSON / NDJSON event format (CLEF-compatible)
//! * [`otlp`] — OpenTelemetry OTLP/HTTP logs, traces and metrics, in both
//!   protobuf and JSON encodings
//!
//! This crate only parses and maps, within the per-request [`Limits`].
//! Queueing, the WAL and backpressure live in `storage`; HTTP concerns live
//! in `server`.

mod jsontree;
pub mod limits;
pub mod native;
pub mod otlp;
mod template;

pub use limits::{Limits, MAX_DEPTH};
pub use template::render_template;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IngestError {
    /// The request as a whole is malformed.
    #[error("{0}")]
    Malformed(String),
    /// One event in a batch is invalid (0-based index).
    #[error("event {index}: {message}")]
    InvalidEvent { index: usize, message: String },
    #[error("unsupported content type '{0}'")]
    UnsupportedContentType(String),
    /// More records than [`Limits::max_events`].
    #[error("batch has more than {limit} events; split it into requests of at most {limit} events")]
    TooManyEvents { limit: usize },
    /// Decoding would hold more than [`Limits::max_decoded_bytes`].
    #[error(
        "batch would take more than {limit_bytes} bytes of memory to decode (it has too many events or values for its \
         size); split it into smaller requests"
    )]
    TooLarge { limit_bytes: usize },
    /// A value nested deeper than [`Limits::max_depth`].
    #[error("values are nested more than {limit} levels deep; flatten them")]
    TooDeep { limit: usize },
}
