use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use server::App;
use server::app::IncompleteShutdown;
use server::config::{ByteSize, Config, LogFormat, format_duration};
use tokio::sync::watch;

/// Vyrtel: small, fast, self-hosted observability (logs, traces, metrics).
#[derive(Parser)]
#[command(name = "vyrtel", version, about, long_about = None)]
#[command(after_help = "Configuration precedence: defaults < config file < VYRTEL_* environment variables < flags.\n\
                        See docs/configuration.md for every option.")]
struct Cli {
    /// Path to a TOML config file (default: ./vyrtel.toml if present).
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
    /// Log level filter, e.g. info, debug, vyrtel=debug.
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
            let default = PathBuf::from("vyrtel.toml");
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
    // Vyrtel's own logs go to stderr/stdout only. They are never fed back
    // into Vyrtel's ingestion pipeline.
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

/// The stop requests Vyrtel handles: SIGINT and SIGTERM on Unix; on
/// Windows the console control events Ctrl+C, Ctrl+Break, closing the
/// console, and system shutdown. Registered before serving, so a signal
/// arriving during startup is not lost.
struct StopSignals {
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
}

impl StopSignals {
    #[cfg(unix)]
    fn register() -> std::io::Result<StopSignals> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(StopSignals { int: signal(SignalKind::interrupt())?, term: signal(SignalKind::terminate())? })
    }

    #[cfg(windows)]
    fn register() -> std::io::Result<StopSignals> {
        use tokio::signal::windows;
        Ok(StopSignals {
            ctrl_c: windows::ctrl_c()?,
            ctrl_break: windows::ctrl_break()?,
            ctrl_close: windows::ctrl_close()?,
            ctrl_shutdown: windows::ctrl_shutdown()?,
        })
    }

    #[cfg(unix)]
    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.int.recv() => "SIGINT",
            _ = self.term.recv() => "SIGTERM",
        }
    }

    #[cfg(windows)]
    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "Ctrl+C",
            _ = self.ctrl_break.recv() => "Ctrl+Break",
            _ = self.ctrl_close.recv() => "console close",
            _ = self.ctrl_shutdown.recv() => "system shutdown",
        }
    }

    /// Count stop requests as they arrive. The handlers stay installed, so
    /// a second Ctrl+C reaches Vyrtel instead of killing it outright.
    async fn count(mut self, count: watch::Sender<u32>, timeout: Duration) {
        loop {
            let name = self.next().await;
            count.send_modify(|n| *n += 1);
            if *count.borrow() == 1 {
                tracing::info!(
                    signal = name,
                    "stop requested; draining accepted work for up to {} (send it again to stop at once)",
                    format_duration(timeout)
                );
            } else {
                tracing::warn!(signal = name, "stop requested again; no longer waiting for accepted work");
            }
        }
    }
}

/// Resolves once `n` stop requests have arrived.
async fn stop_requests(mut count: watch::Receiver<u32>, n: u32) {
    if count.wait_for(|c| *c >= n).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match load_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vyrtel: configuration error: {e:#}");
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
            eprintln!("vyrtel: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(async move {
        let signals = StopSignals::register().context("installing stop signal handlers")?;
        let (count, stops) = watch::channel(0u32);
        tokio::spawn(signals.count(count, config.server.shutdown_timeout));
        let bind = config.server.bind.clone();
        let app = App::build(config)?;
        let listener =
            tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("cannot listen on {bind}"))?;
        println!("{}", app.banner(listener.local_addr()?));
        app.serve_until(listener, stop_requests(stops.clone(), 1), stop_requests(stops, 2)).await
    });
    if let Err(e) = &result
        && e.is::<IncompleteShutdown>()
    {
        // Already logged. Exit now, running no destructors: they would wait
        // for the very work that was abandoned.
        eprintln!("vyrtel: {e}");
        std::process::exit(1);
    }
    // Do not wait for a seal or compaction that shutdown stopped waiting for.
    rt.shutdown_background();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vyrtel: {e:#}");
            let text = format!("{e:#}").to_ascii_lowercase();
            if text.contains("permission denied") || text.contains("access is denied") {
                eprintln!(
                    "hint: the data directory must be writable by the user running Vyrtel                      (in Docker the image runs as uid 65532; see docs/configuration.md)"
                );
            }
            ExitCode::FAILURE
        }
    }
}
