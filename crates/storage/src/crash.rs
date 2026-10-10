//! Fault injection for crash tests.
//!
//! With the `crash-points` feature, `VYRTEL_CRASH_AT=<point>[:<n>]` aborts
//! the process the `n`-th time (default: first) execution reaches `point`.
//! `abort` skips destructors, buffered writers and shutdown hooks, exactly
//! like `kill -9`. Without the feature [`point`] compiles to nothing, so
//! release builds carry no trace of it.
//!
//! Points (in the order a write passes them):
//!
//! | point                          | state on disk when the process dies                      |
//! |--------------------------------|----------------------------------------------------------|
//! | `wal.before_sync`              | records written to the WAL, not fsynced, not acknowledged |
//! | `wal.before_ack`               | records durable (strict) but not yet acknowledged         |
//! | `wal.after_ack`                | records acknowledged                                      |
//! | `rotate.after_wal_create`      | next WAL created, buffer not yet frozen                   |
//! | `segment.before_rename`        | segment complete and fsynced in `tmp/`, not committed     |
//! | `segment.after_rename`         | segment committed, directory not fsynced                  |
//! | `seal.before_wal_delete`       | segment committed, its WAL still present                  |
//! | `compact.before_input_delete`  | compaction output committed, every input still present    |
//! | `compact.mid_input_delete`     | compaction output committed, some inputs deleted          |
//! | `recovery.before_wal_delete`   | recovery sealed an old WAL into a segment, WAL present    |

#[cfg(feature = "crash-points")]
pub fn point(name: &str) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TARGET: OnceLock<Option<(String, u64)>> = OnceLock::new();
    static HITS: AtomicU64 = AtomicU64::new(0);

    let target = TARGET.get_or_init(|| {
        let v = std::env::var("VYRTEL_CRASH_AT").ok()?;
        Some(match v.split_once(':') {
            Some((p, n)) => (p.to_string(), n.parse().unwrap_or(1).max(1)),
            None => (v, 1),
        })
    });
    if let Some((p, n)) = target
        && p == name
        && HITS.fetch_add(1, Ordering::SeqCst) + 1 == *n
    {
        eprintln!("crash point {name} reached; aborting");
        std::process::abort();
    }
}

#[cfg(not(feature = "crash-points"))]
#[inline(always)]
pub fn point(_name: &str) {}
