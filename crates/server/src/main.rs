use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use server::App;
use server::app::{IncompleteShutdown, StartOptions};
use server::config::{ByteSize, Config, LogFormat, Source, format_duration};
use tokio::sync::watch;

/// Vyrtel: small, fast, self-hosted observability (logs, traces, metrics).
#[derive(Parser)]
#[command(name = "vyrtel", version, about, long_about = None)]
#[command(after_help = "Configuration precedence: defaults < saved settings < config file < VYRTEL_* environment \
                        variables < flags.\n\
                        Operational settings are managed in Settings or with `vyrtel settings`; a value the file, \
                        the environment or a flag sets wins and cannot be changed there.\n\
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
    /// Start even if the data directory is missing files recorded at its last
    /// clean shutdown, or its metadata database is missing or empty, and
    /// continue without them. For one start after deciding a partial restore
    /// is acceptable; see docs/configuration.md#backup-and-restore.
    #[arg(long, global = true)]
    accept_incomplete_restore: bool,
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
    /// Check, reading only, whether this version can start the data
    /// directory (its layout identity, formats and completeness) and what
    /// starting it would change. Exit 0 if it can start, 1 (with the reason)
    /// if not. Run the new version's binary on the stopped directory before
    /// switching versions.
    CheckDataDir,
    /// Inspect or repair the admin-managed settings saved in the data
    /// directory. Changes need the server stopped and apply at its next
    /// start. See docs/configuration.md#admin-managed-settings.
    #[command(subcommand)]
    Settings(SettingsCommand),
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Every setting with the value the next start uses, and its source.
    Show,
    /// Only the settings that are not at their defaults.
    Diff,
    /// Print the saved settings as JSON (no deployment inputs or credentials).
    Export,
    /// Replace every saved setting with an export (a file, or - for stdin).
    Import {
        file: PathBuf,
        /// Allow changes that can lose data or weaken durability.
        #[arg(long)]
        confirm: bool,
    },
    /// Save one setting, e.g. `vyrtel settings set query.timeout 1m`.
    Set {
        key: String,
        value: String,
        /// Allow a change that can lose data or weaken durability.
        #[arg(long)]
        confirm: bool,
    },
    /// Return settings to their defaults: the keys given, or --all. Also
    /// removes a saved value this version cannot read.
    Reset {
        keys: Vec<String>,
        #[arg(long, conflicts_with = "keys")]
        all: bool,
        /// Allow changes that can lose data or weaken durability.
        #[arg(long)]
        confirm: bool,
    },
}

fn settings_command(config: &Config, command: SettingsCommand) -> ExitCode {
    use server::settings::{self, Edit};
    let result = match command {
        SettingsCommand::Show => {
            settings::read_offline(config).map(|s| print!("{}", settings::describe(config, &s, false)))
        }
        SettingsCommand::Diff => {
            settings::read_offline(config).map(|s| print!("{}", settings::describe(config, &s, true)))
        }
        SettingsCommand::Export => settings::read_offline(config).map(|s| {
            println!("{}", serde_json::to_string_pretty(&settings::export(&s)).unwrap_or_default());
        }),
        SettingsCommand::Import { file, confirm } => read_export(&file)
            .and_then(|doc| {
                let values = settings::import_values(&doc).map_err(|e| e.to_string())?;
                settings::change_offline(config, Edit::Import { revisions: None, values, confirm })
            })
            .map(|_| ()),
        SettingsCommand::Set { key, value, confirm } => match key.split_once('.') {
            None => Err(format!("{key} is not a setting (use section.key, e.g. query.timeout)")),
            Some((group, name)) => {
                let values = [(name.to_string(), Some(settings::parse_cli_value(&value)))].into();
                let edit = Edit::Save { group: group.to_string(), revision: 0, values, confirm };
                settings::change_offline(config, edit).map(|_| ())
            }
        },
        SettingsCommand::Reset { keys, all, confirm } => {
            let keys = if all { settings::FIELDS.iter().map(|f| f.key.to_string()).collect() } else { keys };
            if keys.is_empty() {
                Err("name the settings to reset, or use --all".to_string())
            } else {
                settings::change_offline(config, Edit::Reset { keys, confirm }).map(|_| ())
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vyrtel: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A settings export from `file` (`-`: stdin), at most
/// [`server::settings::MAX_IMPORT_BYTES`].
fn read_export(file: &std::path::Path) -> Result<serde_json::Value, String> {
    let limit = server::settings::MAX_IMPORT_BYTES as u64 + 1;
    let mut text = String::new();
    let read = if file.as_os_str() == "-" {
        std::io::stdin().take(limit).read_to_string(&mut text)
    } else {
        std::fs::File::open(file).and_then(|f| f.take(limit).read_to_string(&mut text))
    };
    read.map_err(|e| format!("reading {}: {e}", file.display()))?;
    if text.len() as u64 >= limit {
        return Err(format!("{} is larger than a settings export can be", file.display()));
    }
    serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", file.display()))
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    let mut config = match &cli.config {
        Some(p) => Config::load_file(p).map_err(anyhow::Error::msg)?,
        None => {
            let default = PathBuf::from("vyrtel.toml");
            if default.exists() { Config::load_file(&default).map_err(anyhow::Error::msg)? } else { Config::default() }
        }
    };
    server::config::check_env_names(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()))
        .map_err(anyhow::Error::msg)?;
    config.apply_env(|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?;
    if let Some(d) = &cli.data_dir {
        config.storage.path = d.clone();
        config.origin.set("storage.path", Source::Flag);
    }
    if let Some(b) = &cli.bind {
        config.server.bind = b.clone();
        config.origin.set("server.bind", Source::Flag);
    }
    if let Some(m) = &cli.memory_limit {
        config.storage.max_memory = ByteSize::parse(m).map_err(anyhow::Error::msg)?;
        config.origin.set("storage.max_memory", Source::Flag);
    }
    if let Some(l) = &cli.log_level {
        config.log.level = l.clone();
        config.origin.set("log.level", Source::Flag);
    }
    if let Some(f) = &cli.log_format {
        config.log.format = if f == "json" { LogFormat::Json } else { LogFormat::Text };
        config.origin.set("log.format", Source::Flag);
    }
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(config)
}

/// The one logging setup, from the final configuration (saved settings
/// included): nothing is logged before it, and what goes wrong before it
/// is printed to stderr instead.
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
    // Before any thread is started: arenas that exist already stay (see server::malloc).
    let arenas = server::malloc::cap_arenas();
    let cli = Cli::parse();
    let mut config = match load_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vyrtel: configuration error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let options = StartOptions { accept_incomplete_restore: cli.accept_incomplete_restore };
    let command = cli.command;
    if matches!(command, Some(Command::Serve | Command::Config) | None) {
        // Saved settings decide logging and what is set up before the data
        // directory is opened; the start reads them again under its lock.
        config = match server::settings::for_start(&config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("vyrtel: configuration error: {e}");
                return ExitCode::from(2);
            }
        };
    }
    match command {
        Some(Command::Settings(c)) => return settings_command(&config, c),
        Some(Command::Healthcheck) => return healthcheck(&config),
        Some(Command::Config) => {
            println!("{}", toml::to_string_pretty(&config).unwrap_or_default());
            return ExitCode::SUCCESS;
        }
        Some(Command::CheckDataDir) => {
            let dir = &config.storage.path;
            return match server::datadir::check(dir, cli.accept_incomplete_restore) {
                Ok(checked) => {
                    print!("{}", server::datadir::report(dir, &checked));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("vyrtel: {e}");
                    let error = e.to_string();
                    if server::access::is_access_error(&error) {
                        eprint!("{}", server::access::hint(dir, &error));
                    }
                    ExitCode::FAILURE
                }
            };
        }
        Some(Command::Serve) | None => {}
    }
    init_logging(&config);
    server::malloc::report(arenas);
    // Before storage opens anything, so the WAL and segment files count against the raised limit.
    server::limits::report(server::limits::raise(), config.server.max_connections);
    let data_dir = config.storage.path.clone();
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
        let backlog = config.server.listen_backlog;
        let app = App::build_with(config, options)?;
        let listener =
            match server::transport::bind(&bind, backlog).await.with_context(|| format!("cannot listen on {bind}")) {
                Ok(l) => l,
                Err(e) => {
                    // A first start has already stored its generated password;
                    // show it now, or nothing ever will (IAM-02, #77).
                    if let Some(notice) = app.admin_login_notice() {
                        println!("{notice}");
                    }
                    return Err(e);
                }
            };
        if let Some((effective, somaxconn)) = server::transport::effective_backlog(backlog)
            && effective < backlog
        {
            tracing::warn!(
                listen_backlog = backlog,
                somaxconn,
                "server.listen_backlog is above net.core.somaxconn; the kernel uses {effective}"
            );
        }
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
            let error = format!("{e:#}");
            if server::access::is_access_error(&error) {
                eprint!("{}", server::access::hint(&data_dir, &error));
            }
            ExitCode::FAILURE
        }
    }
}
