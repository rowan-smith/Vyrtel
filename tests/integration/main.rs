//! End-to-end workflows against a real server process (in-process, real
//! HTTP on an ephemeral port, real data directory).

mod common;

mod admin;
mod admission;
mod api;
mod auth;
mod crash;
mod ingest_limits;
mod live;
mod shutdown;
mod signals;
mod storage_flows;
// Needs `--features fault-injection` (CI runs `--all-features`).
#[cfg(feature = "fault-injection")]
mod storage_faults;
