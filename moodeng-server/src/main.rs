mod config;
mod protocol;
mod session;

use clap::{Parser, Subcommand};
use moodeng_core::{backup_live, restore, Database, ENGINE_NAME, ENGINE_VERSION, OWNER};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use config::Config;

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

    ping(connect_host, port, timeout).await?;
    info!("ok: {connect_host}:{port} is ready");
    Ok(())
}

async fn ping(host: &str, port: u16, timeout: Duration) -> anyhow::Result<()> {
    let addr = format!("{host}:{port}");
    let mut stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("connection timed out after {timeout:?}"))??;

    let mut pkt = Vec::with_capacity(32);
    pkt.extend_from_slice(&16i32.to_be_bytes());
    pkt.extend_from_slice(&196608i32.to_be_bytes());
    pkt.extend_from_slice(b"user\0moodeng\0");
    stream.write_all(&pkt).await?;

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

    let db = Arc::new(Database::open(&data_dir)?);

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
        let permit = Arc::clone(&connection_limit);

        tokio::spawn(async move {
            let _permit = match permit.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return,
            };
            info!("Connection from {peer}");
            if let Err(e) = protocol::handle_connection(stream, db).await {
                tracing::debug!("Connection closed: {e}");
            }
        });
    }
}
