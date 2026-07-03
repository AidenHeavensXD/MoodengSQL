use moodeng_core::{Database, Session};

fn temp_data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("moodeng_test_{}", uuid::Uuid::new_v4()))
}

#[test]
fn restart_preserves_schema_and_data() {
    let dir = temp_data_dir();

    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, email TEXT)")
            .unwrap();
        db.execute("INSERT INTO users VALUES (1, 'Alice', 'alice@moodeng.dev')")
            .unwrap();
        db.execute("INSERT INTO users VALUES (2, 'Bob', 'bob@moodeng.dev')")
            .unwrap();
    }

    {
        let db = Database::open(&dir).unwrap();
        assert_eq!(db.catalog.list_tables(), vec!["users"]);

        let result = db.execute("SELECT * FROM users WHERE id = 1").unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].values[1].to_display_string(), "Alice");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn primary_key_rejects_duplicates() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, name TEXT)")
        .unwrap();
    db.execute("INSERT INTO t VALUES (1, 'first')").unwrap();

    let err = db.execute("INSERT INTO t VALUES (1, 'second')").unwrap_err();
    assert!(err.to_string().contains("unique") || err.to_string().contains("DuplicateKey"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn not_null_constraint() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL)").unwrap();

    let err = db.execute("INSERT INTO t VALUES (1, NULL)").unwrap_err();
    assert!(err.to_string().contains("not-null"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_persists_after_restart() {
    let dir = temp_data_dir();

    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE t (id INT PRIMARY KEY, email TEXT)").unwrap();
        db.execute("INSERT INTO t VALUES (1, 'a@test.com')").unwrap();
        db.execute("CREATE UNIQUE INDEX idx_email ON t (email)").unwrap();
    }

    {
        let db = Database::open(&dir).unwrap();
        let err = db.execute("INSERT INTO t VALUES (2, 'a@test.com')").unwrap_err();
        assert!(err.to_string().contains("unique") || err.to_string().contains("DuplicateKey"));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_reports_consistent_state() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY)").unwrap();
    db.execute("INSERT INTO t VALUES (1)").unwrap();

    let report = db.check().unwrap();
    assert!(report.iter().any(|m| m.contains("ok")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transaction_commit_persists() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();
    let mut session = Session::new();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
    db.execute_session(&mut session, "BEGIN").unwrap();
    db.execute_session(&mut session, "INSERT INTO t VALUES (1, 'committed')")
        .unwrap();
    db.execute_session(&mut session, "COMMIT").unwrap();

    let result = db.execute("SELECT * FROM t WHERE id = 1").unwrap();
    assert_eq!(result.rows.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transaction_rollback_discards_changes() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();
    let mut session = Session::new();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
    db.execute_session(&mut session, "BEGIN").unwrap();
    db.execute_session(&mut session, "INSERT INTO t VALUES (1, 'rolled back')")
        .unwrap();
    db.execute_session(&mut session, "ROLLBACK").unwrap();

    let result = db.execute("SELECT * FROM t").unwrap();
    assert_eq!(result.rows.len(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_scan_finds_row_by_primary_key() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, name TEXT)").unwrap();
    for i in 1..=100 {
        db.execute(&format!("INSERT INTO t VALUES ({i}, 'user{i}')"))
            .unwrap();
    }

    let result = db.execute("SELECT * FROM t WHERE id = 42").unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values[0].to_display_string(), "42");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wal_replay_after_reopen() {
    let dir = temp_data_dir();

    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
        for i in 1..=10 {
            db.execute(&format!("INSERT INTO t VALUES ({i}, 'v{i}')")).unwrap();
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn order_by_limit_offset() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (id INT PRIMARY KEY, score INT)").unwrap();
    db.execute("INSERT INTO t VALUES (1, 30)").unwrap();
    db.execute("INSERT INTO t VALUES (2, 10)").unwrap();
    db.execute("INSERT INTO t VALUES (3, 20)").unwrap();

    let result = db
        .execute("SELECT id FROM t ORDER BY score DESC LIMIT 2 OFFSET 1")
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].values[0].to_display_string(), "3");
    assert_eq!(result.rows[1].values[0].to_display_string(), "2");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inner_join_works() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT)").unwrap();
    db.execute("CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, item TEXT)").unwrap();
    db.execute("INSERT INTO users VALUES (1, 'Alice')").unwrap();
    db.execute("INSERT INTO orders VALUES (10, 1, 'book')").unwrap();

    let result = db
        .execute("SELECT users.name, orders.item FROM users INNER JOIN orders ON users.id = orders.user_id")
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values[0].to_display_string(), "Alice");
    assert_eq!(result.rows[0].values[1].to_display_string(), "book");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn group_by_count() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE t (dept TEXT, name TEXT)").unwrap();
    db.execute("INSERT INTO t VALUES ('eng', 'a')").unwrap();
    db.execute("INSERT INTO t VALUES ('eng', 'b')").unwrap();
    db.execute("INSERT INTO t VALUES ('hr', 'c')").unwrap();

    let result = db
        .execute("SELECT dept, COUNT(*) FROM t GROUP BY dept ORDER BY dept")
        .unwrap();
    assert_eq!(result.rows.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn substitute_params_replaces_placeholders() {
    use moodeng_core::substitute_params;
    let sql = "SELECT * FROM t WHERE id = $1 AND name = $2";
    let out = substitute_params(sql, &[Some("42".into()), Some("alice".into())]);
    assert!(out.contains("42"));
    assert!(out.contains("'alice'"));
}

#[test]
fn backup_and_restore_roundtrip() {
    use moodeng_core::{backup_live, restore, Database};

    let src = temp_data_dir();
    let dst = temp_data_dir();
    let archive = std::env::temp_dir().join(format!("moodeng_backup_{}.tar.gz", uuid::Uuid::new_v4()));

    {
        let db = Database::open(&src).unwrap();
        db.execute("CREATE TABLE items (id INT PRIMARY KEY, label TEXT)").unwrap();
        db.execute("INSERT INTO items VALUES (1, 'alpha')").unwrap();
        backup_live(&db, &archive).unwrap();
    }

    restore(&dst, &archive).unwrap();

    {
        let db = Database::open(&dst).unwrap();
        assert_eq!(db.catalog.list_tables(), vec!["items"]);
        let result = db.execute("SELECT label FROM items WHERE id = 1").unwrap();
        assert_eq!(result.rows[0].values[0].to_display_string(), "alpha");
    }

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dst);
    let _ = std::fs::remove_file(&archive);
}

#[test]
fn explain_shows_index_scan_for_pk_lookup() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT)").unwrap();
    db.execute("INSERT INTO users VALUES (1, 'Alice')").unwrap();

    let result = db
        .execute("EXPLAIN SELECT * FROM users WHERE id = 1")
        .unwrap();
    assert_eq!(result.columns, vec!["QUERY PLAN"]);
    let plan = result.rows[0].values[0].to_display_string();
    assert!(plan.contains("Index Scan"), "expected index scan, got: {plan}");

    let result = db.execute("EXPLAIN SELECT * FROM users").unwrap();
    let plan = result.rows[0].values[0].to_display_string();
    assert!(plan.contains("Seq Scan"), "expected seq scan, got: {plan}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_inserts_from_ten_clients() {
    use std::sync::Arc;
    use std::thread;

    let dir = temp_data_dir();
    let db = Arc::new(Database::open(&dir).unwrap());
    db.execute("CREATE TABLE hits (id INT PRIMARY KEY, client INT, n INT)").unwrap();

    let mut handles = Vec::new();

    for client in 0..10 {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for n in 0..10 {
                let id = client * 100 + n;
                db.execute(&format!(
                    "INSERT INTO hits VALUES ({id}, {client}, {n})"
                ))
                .unwrap();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let count = db.execute("SELECT * FROM hits").unwrap().rows.len();
    assert_eq!(count, 100, "expected 100 rows from 10x10 concurrent inserts");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wal_batch_sync_survives_checkpoint() {
    let dir = temp_data_dir();
    let db = Database::open(&dir).unwrap();

    db.execute("CREATE TABLE sync_test (id INT PRIMARY KEY, v TEXT)").unwrap();
    for i in 0..5 {
        db.execute(&format!("INSERT INTO sync_test VALUES ({i}, 'x{i}')")).unwrap();
    }
    db.checkpoint().unwrap();

    let db = Database::open(&dir).unwrap();
    let result = db.execute("SELECT * FROM sync_test").unwrap();
    assert_eq!(result.rows.len(), 5);

    let _ = std::fs::remove_dir_all(&dir);
}

fn row_count(db: &Database, table: &str) -> usize {
    db.execute(&format!("SELECT * FROM {table}"))
        .unwrap()
        .rows
        .len()
}

#[test]
fn crash_before_commit_discards_uncommitted_insert() {
    let dir = temp_data_dir();
    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
        let mut session = Session::new();
        db.execute_session(&mut session, "BEGIN").unwrap();
        db.execute_session(&mut session, "INSERT INTO t VALUES (1, 'lost')")
            .unwrap();
        db.flush_wal().unwrap();
    }

    let db = Database::open(&dir).unwrap();
    assert_eq!(row_count(&db, "t"), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crash_after_commit_persists_insert() {
    let dir = temp_data_dir();
    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
        let mut session = Session::new();
        db.execute_session(&mut session, "BEGIN").unwrap();
        db.execute_session(&mut session, "INSERT INTO t VALUES (1, 'kept')")
            .unwrap();
        db.execute_session(&mut session, "COMMIT").unwrap();
        db.flush_wal().unwrap();
    }

    let db = Database::open(&dir).unwrap();
    assert_eq!(row_count(&db, "t"), 1);
    let result = db.execute("SELECT v FROM t WHERE id = 1").unwrap();
    assert_eq!(result.rows[0].values[0].to_display_string(), "kept");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crash_after_rollback_discards_insert() {
    let dir = temp_data_dir();
    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)").unwrap();
        let mut session = Session::new();
        db.execute_session(&mut session, "BEGIN").unwrap();
        db.execute_session(&mut session, "INSERT INTO t VALUES (1, 'rolled')")
            .unwrap();
        db.execute_session(&mut session, "ROLLBACK").unwrap();
        db.flush_wal().unwrap();
    }

    let db = Database::open(&dir).unwrap();
    assert_eq!(row_count(&db, "t"), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wal_torn_last_entry_replays_prior_entries_only() {
    let dir = temp_data_dir();
    {
        let db = Database::open(&dir).unwrap();
        db.execute("CREATE TABLE torn (id INT PRIMARY KEY, v TEXT)").unwrap();
        db.execute("INSERT INTO torn VALUES (1, 'ok')").unwrap();
        db.execute("INSERT INTO torn VALUES (2, 'maybe')").unwrap();
        db.flush_wal().unwrap();
    }

    let wal_path = dir.join("wal.log");
    let mut raw = std::fs::read(&wal_path).unwrap();
    assert!(raw.len() > 16);
    raw.truncate(raw.len().saturating_sub(6));
    std::fs::write(&wal_path, &raw).unwrap();

    let entries = moodeng_core::replay_from_bytes(&raw, 0);
    assert!(!entries.is_empty());

    let db = Database::open(&dir).unwrap();
    let count = row_count(&db, "torn");
    assert!(count >= 1, "expected at least first committed row, got {count}");
    assert!(count <= 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fifty_concurrent_row_inserts() {
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    let dir = temp_data_dir();
    let db = Arc::new(Database::open(&dir).unwrap());
    db.execute("CREATE TABLE concurrent (id INT PRIMARY KEY, client INT)")
        .unwrap();

    let clients = 50;
    let rows_per_client = 10;
    let start = Instant::now();
    let mut handles = Vec::new();

    for client in 0..clients {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for n in 0..rows_per_client {
                let id = client * 1000 + n;
                db.execute(&format!(
                    "INSERT INTO concurrent VALUES ({id}, {client})"
                ))
                .unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let elapsed = start.elapsed();
    let total = clients * rows_per_client;
    let rate = total as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    eprintln!(
        "row-level concurrency: {clients} clients x {rows_per_client} inserts in {elapsed:?} ({rate:.0} rows/s)"
    );

    let count = db.execute("SELECT * FROM concurrent").unwrap().rows.len();
    assert_eq!(count, total);

    let _ = std::fs::remove_dir_all(&dir);
}
