use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use server::App;
use server::config::{ByteSize, Config, LogFormat};

/// Observer: small, fast, self-hosted observability (logs, traces, metrics).
#[derive(Parser)]
#[command(name = "observer", version, about, long_about = None)]
#[command(
    after_help = "Configuration precedence: defaults < config file < OBSERVER_* environment variables < flags.\n\
                        See docs/configuration.md for every option."
)]
struct Cli {
    /// Path to a TOML config file (default: ./observer.toml if present).
    #[arg(long, short = 'c', global = true)]
    config: Option<PathBuf>,
    /// Data directory (overrides storage.path).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Listen address, e.g. 0.0.0.0:8080 (overrides server.bind).
    #[arg(long, global = true)]
    bind: Option<String>,
    /// Global memory budget, e.g. 512MB (overrides storage.max_memory).
    #[arg(long, global = true)]
    memory_limit: Option<String>,
    /// Log level filter, e.g. info, debug, observer=debug.
    #[arg(long, global = true)]
    log_level: Option<String>,
    /// Log output format.
    #[arg(long, global = true, value_parser = ["text", "json"])]
    log_format: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (default).
    Serve,
    /// Exit 0 if the server at the configured address answers /health.
    /// Used by the Docker HEALTHCHECK (no curl needed in the image).
    Healthcheck,
    /// Print the effective configuration as TOML and exit.
    Config,
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    let mut config = match &cli.config {
        Some(p) => Config::load_file(p).map_err(anyhow::Error::msg)?,
        None => {
            let default = PathBuf::from("observer.toml");
            if default.exists() { Config::load_file(&default).map_err(anyhow::Error::msg)? } else { Config::default() }
        }
    };
    config.apply_env(|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?;
    if let Some(d) = &cli.data_dir {
        config.storage.path = d.clone();
    }
    if let Some(b) = &cli.bind {
        config.server.bind = b.clone();
    }
    if let Some(m) = &cli.memory_limit {
        config.storage.max_memory = ByteSize::parse(m).map_err(anyhow::Error::msg)?;
    }
    if let Some(l) = &cli.log_level {
        config.log.level = l.clone();
    }
    if let Some(f) = &cli.log_format {
        config.log.format = if f == "json" { LogFormat::Json } else { LogFormat::Text };
    }
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(config)
}

fn init_logging(config: &Config) {
    use tracing_subscriber::EnvFilter;
    // Observer's own logs go to stderr/stdout only. They are never fed back
    // into Observer's ingestion pipeline.
    let filter = EnvFilter::try_new(&config.log.level).unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    match config.log.format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}

fn healthcheck(config: &Config) -> ExitCode {
    let mut addr: SocketAddr = match config.server.bind.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => return ExitCode::FAILURE,
    };
    if addr.ip().is_unspecified() {
        addr.set_ip([127, 0, 0, 1].into());
    }
    let ok = (|| -> std::io::Result<bool> {
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        write!(s, "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut buf = String::new();
        s.read_to_string(&mut buf)?;
        Ok(buf.starts_with("HTTP/1.1 200"))
    })()
    .unwrap_or(false);
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
    tracing::info!("shutting down");
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match load_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("observer: configuration error: {e:#}");
            return ExitCode::from(2);
        }
    };
    match cli.command {
        Some(Command::Healthcheck) => return healthcheck(&config),
        Some(Command::Config) => {
            println!("{}", toml::to_string_pretty(&config).unwrap_or_default());
            return ExitCode::SUCCESS;
        }
        Some(Command::Serve) | None => {}
    }
    init_logging(&config);
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("observer: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(async move {
        let bind = config.server.bind.clone();
        let app = App::build(config)?;
        let listener =
            tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("cannot listen on {bind}"))?;
        println!("{}", app.banner(listener.local_addr()?));
        app.serve(listener, shutdown_signal()).await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("observer: {e:#}");
            let text = format!("{e:#}").to_ascii_lowercase();
            if text.contains("permission denied") || text.contains("access is denied") {
                eprintln!(
                    "hint: the data directory must be writable by the user running Observer                      (in Docker the image runs as uid 65532; see docs/configuration.md)"
                );
            }
            ExitCode::FAILURE
        }
    }
}
