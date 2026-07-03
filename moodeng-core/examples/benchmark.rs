//! Simple insert/select throughput benchmark.
//!
//! Run: cargo run --release --example benchmark -p moodeng-core
//!
//! CI regression (conservative floors):
//!   MOODENG_BENCH_MIN_INSERT=500 MOODENG_BENCH_MIN_SELECT=500 cargo run --release --example benchmark -p moodeng-core

use moodeng_core::Database;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("moodeng_bench_{}", uuid::Uuid::new_v4()));
    let db = Database::open(&dir)?;

    db.execute("CREATE TABLE bench (id INT PRIMARY KEY, payload TEXT)")?;

    let insert_n = 5_000;
    let start = Instant::now();
    for i in 0..insert_n {
        db.execute(&format!(
            "INSERT INTO bench VALUES ({i}, 'row-{i}')"
        ))?;
    }
    let insert_elapsed = start.elapsed();
    let insert_rate = insert_n as f64 / insert_elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    let select_n = 500;
    let start = Instant::now();
    for i in 0..select_n {
        db.execute(&format!("SELECT payload FROM bench WHERE id = {i}"))?;
    }
    let select_elapsed = start.elapsed();
    let select_rate = select_n as f64 / select_elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    println!("MoodengSQL benchmark (data dir: {})", dir.display());
    println!("  Inserts: {insert_n} rows in {insert_elapsed:?} ({insert_rate:.0} rows/s)");
    println!("  Point selects: {select_n} queries in {select_elapsed:?} ({select_rate:.0} qps)");

    let min_insert = std::env::var("MOODENG_BENCH_MIN_INSERT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let min_select = std::env::var("MOODENG_BENCH_MIN_SELECT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    if min_insert > 0 && insert_rate < min_insert as f64 {
        eprintln!("benchmark regression: insert {insert_rate:.0} rows/s < floor {min_insert}");
        std::process::exit(1);
    }
    if min_select > 0 && select_rate < min_select as f64 {
        eprintln!("benchmark regression: select {select_rate:.0} qps < floor {min_select}");
        std::process::exit(1);
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
