//! Vyrtel's telemetry storage engine.
//!
//! * [`wal`] — append-only write-ahead log with per-record CRCs
//! * [`segment`] — immutable, block-compressed, versioned segment files
//! * [`index`] — Bloom filters, bitmap indexes, zone maps, HLL sketches
//! * [`Storage`] — per-signal streams with crash recovery, sealing,
//!   compaction and retention
//!
//! The on-disk format is documented in `docs/storage-format.md`.

mod cache;
pub mod codec;
mod config;
mod crash;
pub mod encoding;
mod error;
pub mod fsutil;
pub mod index;
mod recovery;
pub mod segment;
mod store;
mod stream;
pub mod wal;

pub use cache::{CacheStats, IndexCache};
pub use config::{CompactionConfig, Durability, StorageConfig, StreamConfig};
pub use error::{DecodeError, Result, StorageError};
pub use recovery::{QuarantineKind, QuarantinedFile, RecoveryReport};
pub use store::{Storage, StorageStats};
pub use stream::{Ack, Batch, CommitHook, Committed, Done, StreamSnapshot, StreamStats};
