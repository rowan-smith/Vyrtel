//! Per-request execution budgets and cancellation (REL-12).
//!
//! One [`Budget`] covers everything a request runs: every page of a trace
//! search, the sample behind facets, the span assembly after it. Its
//! runtime and scan-byte limits are shared by all of them, so splitting work
//! into sub-scans never resets the clock or the byte count.
//!
//! Execution calls [`Budget::check`] at these checkpoints:
//!
//! | checkpoint | where                                                       |
//! |------------|-------------------------------------------------------------|
//! | `start`    | before any data is read                                     |
//! | `segment`  | before planning each segment (index lookups)                |
//! | `block`    | before and after each block read                            |
//! | `rows`     | every [`ROWS_PER_CHECK`] rows evaluated, sealed or unsealed |
//! | `assemble` | between the phases of a multi-phase query                   |
//! | `aggregate`| every [`ROWS_PER_CHECK`] items processed after the scan:    |
//! |            | facet values counted, metric names collected                |
//!
//! A check fails with [`QueryError::Cancelled`] once the [`CancelToken`] is
//! cancelled, [`QueryError::Timeout`] once `timeout` has passed and
//! [`QueryError::TooExpensive`] once more than `max_scan_bytes` were read.
//! A failed query returns only the error, never the rows it had gathered.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::error::QueryError;
use crate::exec::QueryLimits;

/// Rows evaluated between two budget checks. Small enough that a cancelled
/// or timed-out query stops within microseconds of work, large enough that
/// the check costs nothing measurable.
pub const ROWS_PER_CHECK: u32 = 256;

/// Shared cancellation flag. Clones observe the same flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Where execution checks its budget (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    Start,
    Segment,
    Block,
    Rows,
    Assemble,
    /// In-memory work after (or instead of) a scan: facet counting, metric
    /// name discovery.
    Aggregate,
}

/// Test seam: called at every checkpoint before the budget is checked. See
/// [`crate::Engine::set_checkpoint_hook`].
pub type CheckpointHook = Arc<dyn Fn(Checkpoint, &CancelToken) + Send + Sync>;

/// The limits and cancellation state of one request.
pub struct Budget {
    started: Instant,
    timeout: Duration,
    max_scan_bytes: u64,
    /// Bytes read by every scan of this request.
    scanned: Cell<u64>,
    /// Items counted by [`Budget::tick`] since its last check.
    ticks: Cell<u32>,
    cancel: CancelToken,
    hook: Option<CheckpointHook>,
}

impl Budget {
    pub fn new(limits: &QueryLimits, cancel: CancelToken) -> Self {
        Self::with_hook(limits, cancel, None)
    }

    pub(crate) fn with_hook(limits: &QueryLimits, cancel: CancelToken, hook: Option<CheckpointHook>) -> Self {
        Self {
            started: Instant::now(),
            timeout: limits.timeout,
            max_scan_bytes: limits.max_scan_bytes,
            scanned: Cell::new(0),
            ticks: Cell::new(0),
            cancel,
            hook,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Compressed segment bytes read so far by this request.
    pub fn scanned_bytes(&self) -> u64 {
        self.scanned.get()
    }

    pub(crate) fn add_scanned(&self, bytes: u64) {
        self.scanned.set(self.scanned.get().saturating_add(bytes));
    }

    /// Count one item of in-memory work at `at`; check the budget every
    /// [`ROWS_PER_CHECK`] items.
    pub fn tick(&self, at: Checkpoint) -> Result<(), QueryError> {
        let n = self.ticks.get() + 1;
        if n >= ROWS_PER_CHECK {
            self.ticks.set(0);
            return self.check(at);
        }
        self.ticks.set(n);
        Ok(())
    }

    /// Fail if the request was cancelled or is over a limit. Reading exactly
    /// `max_scan_bytes` is allowed; one byte more is not.
    pub fn check(&self, at: Checkpoint) -> Result<(), QueryError> {
        if let Some(hook) = &self.hook {
            hook(at, &self.cancel);
        }
        if self.cancel.is_cancelled() {
            return Err(QueryError::Cancelled);
        }
        if self.started.elapsed() > self.timeout {
            return Err(QueryError::Timeout(self.timeout.as_millis() as u64));
        }
        if self.scanned.get() > self.max_scan_bytes {
            return Err(QueryError::TooExpensive(self.max_scan_bytes));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(timeout: Duration, max_scan_bytes: u64) -> QueryLimits {
        QueryLimits { timeout, max_scan_bytes, ..QueryLimits::default() }
    }

    #[test]
    fn cancellation_is_shared_by_clones() {
        let t = CancelToken::new();
        let b = Budget::new(&QueryLimits::default(), t.clone());
        b.check(Checkpoint::Start).unwrap();
        t.cancel();
        assert!(matches!(b.check(Checkpoint::Rows), Err(QueryError::Cancelled)));
    }

    #[test]
    fn scan_bytes_boundary() {
        let b = Budget::new(&limits(Duration::from_secs(60), 100), CancelToken::new());
        b.add_scanned(100);
        b.check(Checkpoint::Block).expect("exactly the budget is allowed");
        b.add_scanned(1);
        assert!(matches!(b.check(Checkpoint::Block), Err(QueryError::TooExpensive(100))));
    }

    #[test]
    fn zero_timeout_expires_at_the_first_checkpoint() {
        let b = Budget::new(&limits(Duration::ZERO, u64::MAX), CancelToken::new());
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(b.check(Checkpoint::Start), Err(QueryError::Timeout(0))));
    }

    #[test]
    fn tick_checks_every_rows_per_check_items() {
        let t = CancelToken::new();
        let b = Budget::new(&QueryLimits::default(), t.clone());
        t.cancel();
        for _ in 1..ROWS_PER_CHECK {
            b.tick(Checkpoint::Aggregate).expect("no check yet");
        }
        assert!(matches!(b.tick(Checkpoint::Aggregate), Err(QueryError::Cancelled)));
    }

    #[test]
    fn cancellation_wins_over_other_limits() {
        let t = CancelToken::new();
        let b = Budget::new(&limits(Duration::ZERO, 0), t.clone());
        b.add_scanned(1);
        t.cancel();
        assert!(matches!(b.check(Checkpoint::Start), Err(QueryError::Cancelled)));
    }

    #[test]
    fn hook_runs_before_the_check() {
        let hook: CheckpointHook = Arc::new(|at, t| {
            if at == Checkpoint::Assemble {
                t.cancel();
            }
        });
        let b = Budget::with_hook(&QueryLimits::default(), CancelToken::new(), Some(hook));
        b.check(Checkpoint::Rows).unwrap();
        assert!(matches!(b.check(Checkpoint::Assemble), Err(QueryError::Cancelled)));
    }
}
