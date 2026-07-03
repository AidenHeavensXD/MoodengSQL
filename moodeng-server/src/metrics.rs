use moodeng_core::Database;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Prometheus counters/gauges for the wire-protocol server.
#[derive(Debug, Default)]
pub struct ServerMetrics {
    connections_active: AtomicUsize,
    connections_accepted: AtomicU64,
    connections_rejected: AtomicU64,
    queries_total: AtomicU64,
    query_errors_total: AtomicU64,
    slow_queries_total: AtomicU64,
    query_duration_ms_total: AtomicU64,
}

impl ServerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_connections(&self) -> usize {
        self.connections_active.load(Ordering::Relaxed)
    }

    pub fn connection_accepted(&self) {
        self.connections_accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_rejected(&self) {
        self.connections_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_opened(&self) {
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_closed(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn query_executed(&self, duration_ms: u64, slow: bool) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.query_duration_ms_total
            .fetch_add(duration_ms, Ordering::Relaxed);
        if slow {
            self.slow_queries_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn query_failed(&self, duration_ms: u64) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.query_errors_total.fetch_add(1, Ordering::Relaxed);
        self.query_duration_ms_total
            .fetch_add(duration_ms, Ordering::Relaxed);
    }

    pub fn render(&self, db: &Database) -> String {
        let tables = db.catalog.list_tables().len();
        let active = self.connections_active.load(Ordering::Relaxed);
        let accepted = self.connections_accepted.load(Ordering::Relaxed);
        let rejected = self.connections_rejected.load(Ordering::Relaxed);
        let queries = self.queries_total.load(Ordering::Relaxed);
        let errors = self.query_errors_total.load(Ordering::Relaxed);
        let slow = self.slow_queries_total.load(Ordering::Relaxed);
        let duration_ms = self.query_duration_ms_total.load(Ordering::Relaxed);

        format!(
            "\
# HELP moodeng_up MoodengSQL server is running.
# TYPE moodeng_up gauge
moodeng_up 1
# HELP moodeng_connections_active Current open client connections.
# TYPE moodeng_connections_active gauge
moodeng_connections_active {active}
# HELP moodeng_connections_accepted_total Total accepted client connections.
# TYPE moodeng_connections_accepted_total counter
moodeng_connections_accepted_total {accepted}
# HELP moodeng_connections_rejected_total Connections rejected at max_connections.
# TYPE moodeng_connections_rejected_total counter
moodeng_connections_rejected_total {rejected}
# HELP moodeng_queries_total Total SQL queries executed.
# TYPE moodeng_queries_total counter
moodeng_queries_total {queries}
# HELP moodeng_query_errors_total Total failed SQL queries.
# TYPE moodeng_query_errors_total counter
moodeng_query_errors_total {errors}
# HELP moodeng_slow_queries_total Queries exceeding slow_query_ms threshold.
# TYPE moodeng_slow_queries_total counter
moodeng_slow_queries_total {slow}
# HELP moodeng_query_duration_ms_total Cumulative query duration in milliseconds.
# TYPE moodeng_query_duration_ms_total counter
moodeng_query_duration_ms_total {duration_ms}
# HELP moodeng_tables Number of user tables in the catalog.
# TYPE moodeng_tables gauge
moodeng_tables {tables}
"
        )
    }
}

pub async fn serve_http(
    bind_addr: &str,
    metrics: Arc<ServerMetrics>,
    db: Arc<Database>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("Prometheus metrics listening on http://{bind_addr}/metrics");

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (mut stream, _) = accept?;
                let body = metrics.render(&db);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            }
            _ = shutdown.recv() => break,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_core_series() {
        let metrics = ServerMetrics::new();
        metrics.connection_accepted();
        metrics.query_executed(12, false);
        let db = Database::in_memory().unwrap();
        let body = metrics.render(&db);
        assert!(body.contains("moodeng_up 1"));
        assert!(body.contains("moodeng_connections_accepted_total 1"));
        assert!(body.contains("moodeng_queries_total 1"));
        assert!(body.contains("moodeng_query_duration_ms_total 12"));
    }
}
