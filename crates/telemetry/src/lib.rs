//! Common telemetry model shared by ingestion, storage, query and the server.
//!
//! The model is versioned through the storage codec, never through Rust
//! struct layout. See `docs/storage-format.md`.

mod json;
mod model;
mod time;
mod value;

#[cfg(feature = "testing")]
pub mod testing;

pub use model::*;
pub use time::{NANOS_PER_MILLI, NANOS_PER_SEC, Timestamp, parse_json_timestamp};
pub use value::{Fields, Value, format_float, parse_number};
