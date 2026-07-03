use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub auth: AuthConfigSection,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfigSection {
    /// Argon2 password hash (`moodengsql hash-password`). When empty, uses `MOODENG_PASSWORD` env.
    #[serde(default)]
    pub password_hash: Option<String>,
    /// SCRAM-SHA-256 secret (`moodengsql hash-password --scram`).
    #[serde(default)]
    pub password_scram: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Seconds to wait for active connections during graceful shutdown.
    #[serde(default = "default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,
    /// Prometheus metrics HTTP port (0 = disabled). Binds to metrics_host.
    #[serde(default)]
    pub metrics_port: u16,
    /// Host for the Prometheus /metrics HTTP endpoint.
    #[serde(default = "default_metrics_host")]
    pub metrics_host: String,
    /// PEM certificate for TLS (PostgreSQL SSLRequest upgrade).
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    /// PEM private key for TLS.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// Reject plaintext connections; clients must send SSLRequest first.
    #[serde(default)]
    pub require_tls: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// LRU page cache size per table (0 = legacy in-memory `.dat` mode).
    #[serde(default)]
    pub max_cached_pages: usize,
    /// Rows stored per on-disk page when page cache is enabled.
    #[serde(default = "default_rows_per_page")]
    pub rows_per_page: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Log queries slower than this threshold at WARN (all queries at DEBUG).
    #[serde(default = "default_slow_query_ms")]
    pub slow_query_ms: u64,
    #[serde(default)]
    pub format: LogFormat,
}

/// Query logging thresholds passed into the wire protocol layer.
#[derive(Debug, Clone, Copy)]
pub struct QueryLogConfig {
    pub slow_query_ms: u64,
}

impl From<&LogConfig> for QueryLogConfig {
    fn from(log: &LogConfig) -> Self {
        Self {
            slow_query_ms: log.slow_query_ms,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            log: LogConfig::default(),
            auth: AuthConfigSection::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            max_connections: default_max_connections(),
            shutdown_timeout_secs: default_shutdown_timeout_secs(),
            metrics_port: 0,
            metrics_host: default_metrics_host(),
            tls_cert: None,
            tls_key: None,
            require_tls: false,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            max_cached_pages: 0,
            rows_per_page: default_rows_per_page(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            slow_query_ms: default_slow_query_ms(),
            format: LogFormat::default(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    5432
}
fn default_max_connections() -> usize {
    100
}
fn default_shutdown_timeout_secs() -> u64 {
    30
}
fn default_metrics_host() -> String {
    "127.0.0.1".into()
}
fn default_slow_query_ms() -> u64 {
    1000
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./moodeng_data")
}
fn default_log_level() -> String {
    "info".into()
}
fn default_rows_per_page() -> usize {
    16
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn find_and_load() -> Option<Self> {
        for candidate in ["moodeng.toml", "./moodeng.toml", "/etc/moodengsql/moodeng.toml"] {
            let path = Path::new(candidate);
            if path.exists() {
                return Self::load(path).ok();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_config_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.log.slow_query_ms, 1000);
        assert_eq!(cfg.log.format, LogFormat::Text);
    }

    #[test]
    fn server_shutdown_timeout_default() {
        let cfg = Config::default();
        assert_eq!(cfg.server.shutdown_timeout_secs, 30);
    }
}
