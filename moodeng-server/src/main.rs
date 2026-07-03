mod auth;
mod config;
mod metrics;
mod protocol;
mod scram;
mod session;
mod tls;

use clap::{Parser, Subcommand};
use moodeng_core::{backup_live, restore, Database, ENGINE_NAME, ENGINE_VERSION, OWNER};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use auth::{hash_password, hash_scram, AuthConfig};
use config::{Config, LogFormat, QueryLogConfig};
use metrics::ServerMetrics;
use tls::TlsSettings;

#[derive(Parser, Debug)]
#[command(name = "moodengsql", about = "MoodengSQL — blazing-fast PostgreSQL-like database")]
struct Cli {
    /// Path to moodeng.toml (overrides auto-discovery)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,

    /// Legacy flags when no subcommand is given (equivalent to `serve`)
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the TCP server
    Serve(ServeArgs),
    /// Validate data directory and exit
    Check(CheckArgs),
    /// Create a gzip backup of the data directory
    Backup(BackupArgs),
    /// Restore data directory from a backup archive
    Restore(RestoreArgs),
    /// Health check — connect and verify ReadyForQuery
    Ping(PingArgs),
    /// Print an argon2 hash for moodeng.toml [auth].password_hash
    HashPassword {
        /// Plain-text password to hash
        password: String,
        /// Emit SCRAM-SHA-256 secret for [auth].password_scram
        #[arg(long)]
        scram: bool,
    },
}

#[derive(Parser, Debug, Default)]
struct ServeArgs {
    #[arg(short, long)]
    port: Option<u16>,
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    max_connections: Option<usize>,
    /// Validate data directory and exit (legacy; prefer `check` subcommand)
    #[arg(long)]
    check: bool,
}

#[derive(Parser, Debug)]
struct CheckArgs {
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct BackupArgs {
    #[arg(short, long)]
    output: PathBuf,
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct RestoreArgs {
    #[arg(short, long)]
    from: PathBuf,
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct PingArgs {
    #[arg(short, long)]
    host: Option<String>,
    #[arg(short, long)]
    port: Option<u16>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long, default_value_t = 5)]
    timeout_secs: u64,
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    if let Some(path) = &cli.config {
        Config::load(path)
    } else {
        Ok(Config::find_and_load().unwrap_or_default())
    }
}

fn data_dir(cfg: &Config, override_dir: Option<PathBuf>) -> PathBuf {
    override_dir.unwrap_or_else(|| cfg.storage.data_dir.clone())
}

fn auth_config(cfg: &Config, tls: &TlsSettings) -> AuthConfig {
    let mut auth = AuthConfig::from_config_and_env(
        cfg.auth.password_hash.clone(),
        cfg.auth.password_scram.clone(),
    );
    if auth.password_hash.is_some() && tls.tls_available() && !auth.scram_available() {
        auth.require_tls_for_password = true;
    }
    auth
}

fn init_logging(cfg: &Config) {
    let level = match cfg.log.level.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };
    let builder = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false);
    let _ = match cfg.log.format {
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
        LogFormat::Text => builder.try_init(),
    };
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C"),
        _ = terminate => info!("Received SIGTERM"),
    }
}

async fn drain_connections(metrics: &ServerMetrics, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = metrics.active_connections();
        if remaining == 0 {
            info!("All client connections closed");
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                active_connections = remaining,
                "Shutdown timeout elapsed with active connections still open"
            );
            return;
        }
        debug!(active_connections = remaining, "Waiting for connections to drain");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    init_logging(&cfg);

    match cli.command {
        None => run_serve(&cfg, &cli.serve).await,
        Some(Command::Serve(args)) => run_serve(&cfg, &args).await,
        Some(Command::Check(args)) => run_check(&cfg, &args),
        Some(Command::Backup(args)) => run_backup(&cfg, &args),
        Some(Command::Restore(args)) => run_restore(&cfg, &args),
        Some(Command::Ping(args)) => run_ping(&cfg, &args).await,
        Some(Command::HashPassword { password, scram }) => {
            if scram {
                println!("{}", hash_scram(&password));
            } else {
                println!("{}", hash_password(&password));
            }
            Ok(())
        }
    }
}

fn run_check(cfg: &Config, args: &CheckArgs) -> anyhow::Result<()> {
    let data_dir = data_dir(cfg, args.data_dir.clone());
    let db = Database::open(&data_dir)?;
    for msg in db.check()? {
        info!("{msg}");
    }
    Ok(())
}

fn run_backup(cfg: &Config, args: &BackupArgs) -> anyhow::Result<()> {
    let data_dir = data_dir(cfg, args.data_dir.clone());
    let db = Database::open(&data_dir)?;
    backup_live(&db, &args.output)?;
    info!("Backup written to {}", args.output.display());
    Ok(())
}

fn run_restore(cfg: &Config, args: &RestoreArgs) -> anyhow::Result<()> {
    let data_dir = data_dir(cfg, args.data_dir.clone());
    restore(&data_dir, &args.from)?;
    info!("Restored {} to {}", args.from.display(), data_dir.display());
    Ok(())
}

async fn run_ping(cfg: &Config, args: &PingArgs) -> anyhow::Result<()> {
    let host = args.host.as_deref().unwrap_or(&cfg.server.host);
    let port = args.port.unwrap_or(cfg.server.port);
    let timeout = Duration::from_secs(args.timeout_secs);
    let connect_host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1"
    } else {
        host
    };
    let password = args
        .password
        .clone()
        .or_else(|| std::env::var("MOODENG_PASSWORD").ok());

    ping(connect_host, port, timeout, password.as_deref()).await?;
    info!("ok: {connect_host}:{port} is ready");
    Ok(())
}

async fn ping(
    host: &str,
    port: u16,
    timeout: Duration,
    password: Option<&str>,
) -> anyhow::Result<()> {
    let addr = format!("{host}:{port}");
    let mut stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("connection timed out after {timeout:?}"))??;

    let mut body = Vec::new();
    body.extend_from_slice(&196608i32.to_be_bytes());
    body.extend_from_slice(b"user\0moodeng\0");
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    pkt.extend_from_slice(&body);
    stream.write_all(&pkt).await?;

    if !read_until_auth_ready(&mut stream, password).await? {
        anyhow::bail!("authentication failed during ping");
    }

    loop {
        let msg_type = match stream.read_u8().await {
            Ok(t) => t,
            Err(_) => anyhow::bail!("connection closed before ReadyForQuery"),
        };
        let len = stream.read_i32().await? as usize;
        let mut payload = vec![0u8; len.saturating_sub(4)];
        if !payload.is_empty() {
            stream.read_exact(&mut payload).await?;
        }
        match msg_type {
            b'Z' => return Ok(()),
            b'E' => anyhow::bail!("server returned error during ping"),
            _ => {}
        }
    }
}

async fn read_until_auth_ready(
    stream: &mut TcpStream,
    password: Option<&str>,
) -> anyhow::Result<bool> {
    loop {
        let msg_type = stream.read_u8().await?;
        let len = stream.read_i32().await? as usize;
        let mut payload = vec![0u8; len.saturating_sub(4)];
        if !payload.is_empty() {
            stream.read_exact(&mut payload).await?;
        }
        match msg_type {
            b'R' => {
                if payload.len() >= 4 {
                    let auth_type = i32::from_be_bytes(payload[..4].try_into()?);
                    if auth_type == 0 {
                        return Ok(true);
                    }
                    if auth_type == 3 {
                        let pwd = password.unwrap_or("");
                        let mut body = Vec::new();
                        body.extend_from_slice(pwd.as_bytes());
                        body.push(0);
                        let msg_len = (body.len() + 4) as i32;
                        stream.write_u8(b'p').await?;
                        stream.write_i32(msg_len).await?;
                        stream.write_all(&body).await?;
                        continue;
                    }
                }
                return Ok(false);
            }
            b'E' => return Ok(false),
            b'S' | b'K' => continue,
            b'Z' => return Ok(true),
            _ => continue,
        }
    }
}

async fn run_serve(cfg: &Config, args: &ServeArgs) -> anyhow::Result<()> {
    if args.check {
        let check = CheckArgs {
            data_dir: args.data_dir.clone(),
        };
        return run_check(cfg, &check);
    }

    let host = args.host.as_deref().unwrap_or(&cfg.server.host);
    let port = args.port.unwrap_or(cfg.server.port);
    let data_dir = data_dir(cfg, args.data_dir.clone());
    let max_connections = args.max_connections.unwrap_or(cfg.server.max_connections);
    let shutdown_timeout = Duration::from_secs(cfg.server.shutdown_timeout_secs);
    let query_log = QueryLogConfig::from(&cfg.log);
    let tls_settings = TlsSettings::from_server_config(&cfg.server);
    let tls_acceptor = tls_settings
        .load_server_config()?
        .map(|cfg| Arc::new(tokio_rustls::TlsAcceptor::from(cfg)));
    let auth = Arc::new(auth_config(cfg, &tls_settings));

    if tls_settings.require_tls && !tls_settings.tls_available() {
        anyhow::bail!("require_tls is enabled but tls_cert / tls_key are not configured");
    }

    if tls_settings.tls_available() {
        info!("TLS enabled for wire protocol");
    }

    if auth.required() {
        info!("Password authentication enabled");
    } else {
        warn!("No password configured — trust mode (set [auth].password_hash or MOODENG_PASSWORD)");
    }

    let db = Arc::new(Database::open_with_options(
        &data_dir,
        moodeng_core::StorageOptions {
            max_cached_pages: cfg.storage.max_cached_pages,
            rows_per_page: cfg.storage.rows_per_page,
        },
    )?);

    info!("╔══════════════════════════════════════════╗");
    info!("║  {ENGINE_NAME} v{ENGINE_VERSION}                  ║");
    info!("║  Owner: {OWNER:<30} ║");
    info!("║  PostgreSQL-compatible · Ultra-fast      ║");
    info!("╚══════════════════════════════════════════╝");
    info!("Data directory: {}", data_dir.display());
    info!("Max connections: {max_connections}");
    info!("Slow query threshold: {} ms", query_log.slow_query_ms);
    info!("Shutdown drain timeout: {} s", shutdown_timeout.as_secs());

    let metrics = Arc::new(ServerMetrics::new());
    let (metrics_shutdown_tx, _) = tokio::sync::broadcast::channel(1);

    if cfg.server.metrics_port > 0 {
        let metrics_addr = format!("{}:{}", cfg.server.metrics_host, cfg.server.metrics_port);
        let metrics_db = Arc::clone(&db);
        let metrics_arc = Arc::clone(&metrics);
        let metrics_shutdown = metrics_shutdown_tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) = metrics::serve_http(&metrics_addr, metrics_arc, metrics_db, metrics_shutdown).await
            {
                warn!(error = %e, "Metrics server stopped");
            }
        });
    }

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {addr}");

    let connection_limit = Arc::new(Semaphore::new(max_connections));

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, peer) = accept_result?;
                let db = Arc::clone(&db);
                let auth = Arc::clone(&auth);
                let tls_settings = tls_settings.clone();
                let tls_acceptor = tls_acceptor.clone();
                let connection_limit = Arc::clone(&connection_limit);
                let metrics = Arc::clone(&metrics);

                let permit = match connection_limit.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(tokio::sync::TryAcquireError::NoPermits) => {
                        metrics.connection_rejected();
                        warn!(peer = %peer, "Connection rejected: max_connections reached");
                        drop(stream);
                        continue;
                    }
                    Err(tokio::sync::TryAcquireError::Closed) => break,
                };

                metrics.connection_accepted();
                info!(peer = %peer, "Connection accepted");

                tokio::spawn(async move {
                    let _permit = permit;
                    let _guard = ActiveConnectionGuard::new(metrics.clone());
                    if let Err(e) = protocol::handle_connection(
                        stream,
                        peer,
                        db,
                        auth,
                        tls_settings,
                        tls_acceptor,
                        query_log,
                        metrics,
                    )
                    .await
                    {
                        debug!(peer = %peer, error = %e, "Connection closed with error");
                    }
                });
            }
            _ = shutdown_signal() => {
                info!("Graceful shutdown started — no longer accepting connections");
                break;
            }
        }
    }

    drop(listener);
    connection_limit.close();
    let _ = metrics_shutdown_tx.send(());
    drain_connections(&metrics, shutdown_timeout).await;

    if let Err(e) = db.checkpoint() {
        warn!(error = %e, "Final checkpoint failed during shutdown");
    } else {
        info!("Final checkpoint completed");
    }

    info!("Server stopped");
    Ok(())
}

struct ActiveConnectionGuard {
    metrics: Arc<ServerMetrics>,
}

impl ActiveConnectionGuard {
    fn new(metrics: Arc<ServerMetrics>) -> Self {
        metrics.connection_opened();
        Self { metrics }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.metrics.connection_closed();
    }
}
