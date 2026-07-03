//! Simple insert/select throughput benchmark.
//!
//! Run: cargo run --release --example benchmark -p moodeng-core

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
    let insert_rate = insert_n as f64 / insert_elapsed.as_secs_f64();

    let select_n = 500;
    let start = Instant::now();
    for i in 0..select_n {
        db.execute(&format!("SELECT payload FROM bench WHERE id = {i}"))?;
    }
    let select_elapsed = start.elapsed();
    let select_rate = select_n as f64 / select_elapsed.as_secs_f64();

    println!("MoodengSQL benchmark (data dir: {})", dir.display());
    println!("  Inserts: {insert_n} rows in {insert_elapsed:?} ({insert_rate:.0} rows/s)");
    println!("  Point selects: {select_n} queries in {select_elapsed:?} ({select_rate:.0} qps)");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
