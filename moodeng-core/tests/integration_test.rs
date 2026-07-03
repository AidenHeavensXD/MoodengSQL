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

    {
        let db = Database::open(&dir).unwrap();
        let result = db.execute("SELECT * FROM t").unwrap();
        assert_eq!(result.rows.len(), 10);
    }

    let _ = std::fs::remove_dir_all(&dir);
}
