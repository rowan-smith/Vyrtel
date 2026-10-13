//! The Vyrtel server: one process with HTTP API, OTLP ingestion, query
//! engine, storage, metadata, background jobs and the embedded web UI.

pub mod access;
pub mod alerts;
pub mod app;
pub mod auth;
pub mod config;
pub mod datadir;
pub mod error;
pub mod routes;
pub mod state;
pub mod web;

pub use app::App;
pub use config::Config;
