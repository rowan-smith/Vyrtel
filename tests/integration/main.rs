//! End-to-end workflows against a real server process (in-process, real
//! HTTP on an ephemeral port, real data directory).

mod common;

mod admin;
mod api;
mod auth;
mod crash;
mod live;
mod signals;
mod storage_flows;
