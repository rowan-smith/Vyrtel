//! Ingestion formats → common telemetry model.
//!
//! * [`native`] — Vyrtel's JSON / NDJSON event format (CLEF-compatible)
//! * [`otlp`] — OpenTelemetry OTLP/HTTP logs, traces and metrics, in both
//!   protobuf and JSON encodings
//!
//! This crate only parses and maps. Queueing, the WAL and backpressure live
//! in `storage`; HTTP concerns live in `server`.

pub mod native;
pub mod otlp;
mod template;

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
}
