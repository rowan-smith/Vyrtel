mod api;
mod config;
mod seed;
mod state;
mod static_files;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::state::AppState;

#[derive(Parser, Debug)]
#[command(name = "observatory", about = "Simple structured logging & trace server")]
struct Cli {
    /// Generate sample telemetry on startup
    #[arg(long)]
    seed: bool,

    /// Path to config.toml
    #[arg(long)]
    config: Option<PathBuf>,

    /// Instance / product name
    #[arg(long)]
    name: Option<String>,

    /// Development mode: allow ingest without an API key
    #[arg(long)]
    development: Option<bool>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    let mut config = Config::load(cli.config.as_deref())?;
    if let Some(name) = cli.name {
        config.name = name;
    }
    if let Some(development) = cli.development {
        config.development = development;
    }

    std::fs::create_dir_all(&config.storage.path)
        .with_context(|| format!("create data dir {}", config.storage.path.display()))?;

    let index_path = config.storage.path.join("index");
    let meta_path = config.storage.path.join("metadata.db");

    let store = storage::EventStore::open(&index_path)?;
    let metadata = metadata::MetadataStore::open(&meta_path).await?;

    seed_retention(&metadata, &config).await?;

    let ingest = ingest::start_ingest_worker(store.clone());
    let state = AppState {
        store: store.clone(),
        ingest,
        metadata: Arc::new(metadata),
        config: config.clone(),
    };

    if cli.seed {
        info!("Seeding sample telemetry…");
        seed::seed_sample_data(&state).await?;
        // Give the writer a moment to flush
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _ = state.store.reload();
    }

    // Retention cleanup loop
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                if let Err(err) = run_retention(&state).await {
                    tracing::warn!("retention cleanup failed: {err}");
                }
            }
        });
    }

    // Alert evaluation loop
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            loop {
                ticker.tick().await;
                if let Err(err) = api::evaluate_alerts(&state).await {
                    tracing::warn!("alert evaluation failed: {err}");
                }
            }
        });
    }

    let app = api::router_with_state(state.clone())
        .fallback(static_files::static_handler)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr = SocketAddr::from((config.server.host_addr(), config.server.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    let event_count = state.store.event_count().unwrap_or(0);
    let retention = state
        .metadata
        .get_retention_days()
        .await?
        .map(|d| format!("{d} days"))
        .unwrap_or_else(|| "Forever".into());

    println!();
    println!("{}", config.name);
    println!();
    println!("HTTP       http://localhost:{}", config.server.port);
    println!("Data       {}", config.storage.path.display());
    println!("Events     {}", format_number(event_count));
    println!("Retention  {retention}");
    println!(
        "Mode       {}",
        if config.development {
            "development (open ingest)"
        } else {
            "production (API key required)"
        }
    );
    println!();
    println!("Ready");
    println!();

    axum::serve(listener, app).await?;
    Ok(())
}

/// On first run, store the configured retention (30 days unless configured otherwise).
/// Afterwards the saved setting wins, including an explicit "forever".
async fn seed_retention(metadata: &metadata::MetadataStore, config: &Config) -> anyhow::Result<()> {
    if !metadata.has_retention_setting().await? {
        metadata.set_retention_days(config.retention.days).await?;
    }
    Ok(())
}

async fn run_retention(state: &AppState) -> anyhow::Result<()> {
    let days = state.metadata.get_retention_days().await?;
    let Some(days) = days else {
        return Ok(());
    };
    if days <= 0 {
        return Ok(());
    }
    let cutoff = Utc::now() - ChronoDuration::days(days);
    state.store.delete_before(cutoff)?;
    Ok(())
}

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn metadata() -> (tempfile::TempDir, metadata::MetadataStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = metadata::MetadataStore::open(&dir.path().join("metadata.db")).await.unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn first_run_seeds_retention_from_config() {
        let (_dir, store) = metadata().await;
        seed_retention(&store, &Config::default()).await.unwrap();
        assert_eq!(store.get_retention_days().await.unwrap(), Some(30));

        let (_dir, store) = metadata().await;
        let mut config = Config::default();
        config.retention.days = None;
        seed_retention(&store, &config).await.unwrap();
        assert!(store.has_retention_setting().await.unwrap());
        assert_eq!(store.get_retention_days().await.unwrap(), None, "configured forever");
    }

    #[tokio::test]
    async fn saved_retention_survives_restart() {
        let (_dir, store) = metadata().await;
        store.set_retention_days(None).await.unwrap(); // user chose "Forever"
        seed_retention(&store, &Config::default()).await.unwrap();
        assert_eq!(store.get_retention_days().await.unwrap(), None);

        store.set_retention_days(Some(7)).await.unwrap();
        seed_retention(&store, &Config::default()).await.unwrap();
        assert_eq!(store.get_retention_days().await.unwrap(), Some(7));
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }
}
