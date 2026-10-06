use std::path::PathBuf;
use std::time::Duration;

use telemetry::Signal;

use crate::segment::SegmentWriteOptions;

/// How hard the WAL tries to survive power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Writes reach the OS before acknowledgement; fsync runs on a timer
    /// (`fsync_interval`). A process crash loses nothing; a power failure
    /// can lose up to one interval of acknowledged data.
    #[default]
    Normal,
    /// fsync before every acknowledgement (group-committed, so concurrent
    /// requests share one fsync).
    Strict,
}

impl Durability {
    pub fn as_str(self) -> &'static str {
        match self {
            Durability::Normal => "normal",
            Durability::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Seal the active segment once its in-memory size reaches this.
    /// This bounds memory, so it is measured in uncompressed bytes.
    pub segment_target_bytes: u64,
    pub segment_max_age: Duration,
    pub segment_max_events: u64,
    /// Maximum events waiting to be written (backpressure threshold).
    pub queue_events: usize,
    /// Delete segments whose newest event is older than this.
    pub retention: Option<Duration>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            segment_target_bytes: 64 * 1024 * 1024,
            segment_max_age: Duration::from_secs(300),
            segment_max_events: 2_000_000,
            queue_events: 10_000,
            retention: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompactionConfig {
    pub enabled: bool,
    /// Merge only when at least this many small segments exist.
    pub min_inputs: usize,
    /// Upper bound on uncompressed input per compaction run (memory bound).
    pub max_input_bytes: u64,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { enabled: true, min_inputs: 4, max_input_bytes: 32 * 1024 * 1024 }
    }
}

#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub durability: Durability,
    pub fsync_interval: Duration,
    pub index_cache_bytes: usize,
    pub maintenance_interval: Duration,
    pub segment: SegmentWriteOptions,
    pub compaction: CompactionConfig,
    pub logs: StreamConfig,
    pub traces: StreamConfig,
    pub metrics: StreamConfig,
}

impl StorageConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            durability: Durability::Normal,
            fsync_interval: Duration::from_secs(1),
            index_cache_bytes: 64 * 1024 * 1024,
            maintenance_interval: Duration::from_secs(30),
            segment: SegmentWriteOptions::default(),
            compaction: CompactionConfig::default(),
            logs: StreamConfig::default(),
            traces: StreamConfig::default(),
            metrics: StreamConfig::default(),
        }
    }

    pub fn stream(&self, s: Signal) -> &StreamConfig {
        match s {
            Signal::Logs => &self.logs,
            Signal::Traces => &self.traces,
            Signal::Metrics => &self.metrics,
        }
    }

    pub fn stream_mut(&mut self, s: Signal) -> &mut StreamConfig {
        match s {
            Signal::Logs => &mut self.logs,
            Signal::Traces => &mut self.traces,
            Signal::Metrics => &mut self.metrics,
        }
    }
}
