mod protocol;
mod session;

use clap::Parser;
use moodeng_core::{Database, ENGINE_NAME, ENGINE_VERSION, OWNER};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(name = "moodengsql", about = "MoodengSQL — blazing-fast PostgreSQL-like database")]
struct Args {
    /// TCP port to listen on
    #[arg(short, long, default_value_t = 5432)]
    port: u16,

    /// Data directory for persistent storage
    #[arg(short, long, default_value = "./moodeng_data")]
    data_dir: PathBuf,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Validate data directory and exit
    #[arg(long)]
    check: bool,

    /// Maximum concurrent connections
    #[arg(long, default_value_t = 100)]
    max_connections: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    let db = Database::open(&args.data_dir)?;

    if args.check {
        for msg in db.check()? {
            info!("{msg}");
        }
        return Ok(());
    }

    let db = Arc::new(db);
    let connection_limit = Arc::new(tokio::sync::Semaphore::new(args.max_connections));

    info!("╔══════════════════════════════════════════╗");
    info!("║  {ENGINE_NAME} v{ENGINE_VERSION}                  ║");
    info!("║  Owner: {OWNER:<30} ║");
    info!("║  PostgreSQL-compatible · Ultra-fast      ║");
    info!("╚══════════════════════════════════════════╝");
    info!("Data directory: {}", args.data_dir.display());
    info!("Max connections: {}", args.max_connections);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {addr}");

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
