//! End-to-end workflows against a real server process (in-process, real
//! HTTP on an ephemeral port, real data directory).

mod common;

mod admin;
mod admission;
mod api;
mod auth;
mod backup;
mod crash;
mod data_dir_access;
mod decode_limits;
mod ingest_limits;
mod layouts;
mod live;
mod query_budgets;
mod recovery_health;
mod shutdown;
mod signals;
mod storage_flows;
mod transport;
// Needs `--features fault-injection` (CI runs `--all-features`).
#[cfg(feature = "fault-injection")]
mod storage_faults;
