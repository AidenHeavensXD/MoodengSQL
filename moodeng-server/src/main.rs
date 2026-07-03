mod auth;
mod config;
mod protocol;
mod session;
mod tls;

use clap::{Parser, Subcommand};
use moodeng_core::{backup_live, restore, Database, ENGINE_NAME, ENGINE_VERSION, OWNER};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

use auth::{hash_password, AuthConfig};
use config::Config;
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
    let mut auth = AuthConfig::from_config_and_env(cfg.auth.password_hash.clone());
    if auth.required() && tls.tls_available() {
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
    let _ = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .try_init();
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
        Some(Command::HashPassword { password }) => {
            println!("{}", hash_password(&password));
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

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {addr}");

    let connection_limit = Arc::new(tokio::sync::Semaphore::new(max_connections));

    loop {
        let (stream, peer) = listener.accept().await?;
        let db = Arc::clone(&db);
        let auth = Arc::clone(&auth);
        let tls_settings = tls_settings.clone();
        let tls_acceptor = tls_acceptor.clone();
        let permit = Arc::clone(&connection_limit);

        tokio::spawn(async move {
            let _permit = match permit.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return,
            };
            info!("Connection from {peer}");
            if let Err(e) =
                protocol::handle_connection(stream, db, auth, tls_settings, tls_acceptor).await
            {
                tracing::debug!("Connection closed: {e}");
            }
        });
    }
}
