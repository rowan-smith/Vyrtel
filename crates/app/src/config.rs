use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    /// Product / instance name shown in the UI and about endpoint.
    #[serde(default = "default_name")]
    pub name: String,
    /// When true, ingest endpoints accept telemetry without an API key.
    #[serde(default = "default_development")]
    pub development: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

impl ServerConfig {
    pub fn host_addr(&self) -> IpAddr {
        self.host
            .parse()
            .unwrap_or_else(|_| IpAddr::from([0, 0, 0, 0]))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_storage_path")]
    pub path: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            path: default_storage_path(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetentionConfig {
    #[serde(default = "default_retention_days")]
    pub days: Option<i64>,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            days: default_retention_days(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            retention: RetentionConfig::default(),
            name: default_name(),
            development: default_development(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut config = Config::default();

        let config_path = path.map(|p| p.to_path_buf()).or_else(|| {
            let p = PathBuf::from("config.toml");
            if p.exists() {
                Some(p)
            } else {
                None
            }
        });

        if let Some(path) = config_path {
            let text = std::fs::read_to_string(&path)?;
            config = toml::from_str(&text)?;
        }

        if let Ok(host) = std::env::var("OBSERVATORY_SERVER_HOST") {
            config.server.host = host;
        }
        if let Ok(port) = std::env::var("OBSERVATORY_SERVER_PORT") {
            config.server.port = port.parse()?;
        }
        if let Ok(path) = std::env::var("OBSERVATORY_STORAGE_PATH") {
            config.storage.path = PathBuf::from(path);
        }
        if let Ok(days) = std::env::var("OBSERVATORY_RETENTION_DAYS") {
            if days.eq_ignore_ascii_case("forever") {
                config.retention.days = None;
            } else {
                config.retention.days = Some(days.parse()?);
            }
        }
        if let Ok(name) = std::env::var("OBSERVATORY_NAME") {
            config.name = name;
        }
        if let Ok(dev) = std::env::var("OBSERVATORY_DEVELOPMENT") {
            config.development = parse_bool(&dev);
        }

        Ok(config)
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    5341
}
fn default_storage_path() -> PathBuf {
    PathBuf::from("./data")
}
fn default_retention_days() -> Option<i64> {
    Some(30)
}
fn default_name() -> String {
    "Observatory".into()
}
fn default_development() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV_VARS: &[&str] = &[
        "OBSERVATORY_SERVER_HOST",
        "OBSERVATORY_SERVER_PORT",
        "OBSERVATORY_STORAGE_PATH",
        "OBSERVATORY_RETENTION_DAYS",
        "OBSERVATORY_NAME",
        "OBSERVATORY_DEVELOPMENT",
    ];

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 5341);
        assert_eq!(c.storage.path, PathBuf::from("./data"));
        assert_eq!(c.retention.days, Some(30));
        assert_eq!(c.name, "Observatory");
        assert!(c.development);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let c: Config = toml::from_str("[server]\nport = 8080\n").unwrap();
        assert_eq!(c.server.port, 8080);
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.retention.days, Some(30));
        assert!(c.development);

        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.server.port, 5341);
    }

    #[test]
    fn full_toml() {
        let c: Config = toml::from_str(
            r#"
            name = "Acme Logs"
            development = false
            [server]
            host = "127.0.0.1"
            port = 9000
            [storage]
            path = "/var/lib/obs"
            [retention]
            days = 7
            "#,
        )
        .unwrap();
        assert_eq!(c.name, "Acme Logs");
        assert!(!c.development);
        assert_eq!(c.server.host_addr(), IpAddr::from([127, 0, 0, 1]));
        assert_eq!(c.server.port, 9000);
        assert_eq!(c.storage.path, PathBuf::from("/var/lib/obs"));
        assert_eq!(c.retention.days, Some(7));
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(toml::from_str::<Config>("[server]\nport = \"high\"\n").is_err());
        assert!(toml::from_str::<Config>("[server\n").is_err());
    }

    #[test]
    fn host_addr_falls_back_to_unspecified() {
        let s = ServerConfig { host: "not-an-ip".into(), port: 1 };
        assert_eq!(s.host_addr(), IpAddr::from([0, 0, 0, 0]));
        let s = ServerConfig { host: "::1".into(), port: 1 };
        assert!(s.host_addr().is_loopback());
    }

    #[test]
    fn parse_bool_values() {
        for t in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(parse_bool(t), "{t}");
        }
        for f in ["0", "false", "no", "off", "", "maybe"] {
            assert!(!parse_bool(f), "{f}");
        }
    }

    /// All env-var behaviour lives in one test because the process environment is shared
    /// between test threads.
    #[test]
    fn load_from_file_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "name = \"FromFile\"\n[server]\nport = 7000\n[retention]\ndays = 10\n").unwrap();
        for v in ENV_VARS {
            std::env::remove_var(v);
        }

        let from_file = Config::load(Some(&path));

        std::env::set_var("OBSERVATORY_SERVER_HOST", "10.0.0.1");
        std::env::set_var("OBSERVATORY_SERVER_PORT", "7100");
        std::env::set_var("OBSERVATORY_STORAGE_PATH", "/tmp/obs");
        std::env::set_var("OBSERVATORY_RETENTION_DAYS", "Forever");
        std::env::set_var("OBSERVATORY_NAME", "FromEnv");
        std::env::set_var("OBSERVATORY_DEVELOPMENT", "off");
        let from_env = Config::load(Some(&path));

        std::env::set_var("OBSERVATORY_SERVER_PORT", "not-a-port");
        let bad_port = Config::load(Some(&path));
        std::env::set_var("OBSERVATORY_SERVER_PORT", "7100");
        std::env::set_var("OBSERVATORY_RETENTION_DAYS", "a week");
        let bad_days = Config::load(Some(&path));
        std::env::set_var("OBSERVATORY_RETENTION_DAYS", "3");
        let days = Config::load(Some(&path)).map(|c| c.retention.days);

        // Restore the environment before asserting so a failure can't leak into other tests.
        for v in ENV_VARS {
            std::env::remove_var(v);
        }

        let c = from_file.unwrap();
        assert_eq!(c.name, "FromFile");
        assert_eq!(c.server.port, 7000);
        assert_eq!(c.retention.days, Some(10));

        let c = from_env.unwrap();
        assert_eq!(c.server.host, "10.0.0.1");
        assert_eq!(c.server.port, 7100);
        assert_eq!(c.storage.path, PathBuf::from("/tmp/obs"));
        assert_eq!(c.retention.days, None);
        assert_eq!(c.name, "FromEnv");
        assert!(!c.development);

        assert!(bad_port.is_err());
        assert!(bad_days.is_err());
        assert_eq!(days.unwrap(), Some(3));
        assert!(Config::load(Some(&dir.path().join("missing.toml"))).is_err());
    }
}
