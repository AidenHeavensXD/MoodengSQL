# MoodengSQL

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)

MoodengSQL is a blazing-fast, PostgreSQL-inspired relational database engine written in Rust. It speaks a subset of the PostgreSQL wire protocol and SQL dialect, optimized for low-latency reads and writes.

## Features

- **PostgreSQL-compatible SQL** — `CREATE TABLE`, `INSERT`, `SELECT`, `UPDATE`, `DELETE`, `CREATE INDEX`
- **PostgreSQL wire protocol** — connect with the built-in CLI or any compatible client
- **B-tree indexes** — fast point lookups and range scans
- **Binary persistence** — durable on-disk storage with bincode serialization
- **Metadata persistence** — schema and indexes survive server restart (`meta.bin`)
- **WAL + checkpoint** — write-ahead log with automatic checkpointing (every 50 ops)
- **Batch WAL fsync** — groups disk sync every 10 ops for higher write throughput
- **Transactions** — `BEGIN` / `COMMIT` / `ROLLBACK` with undo log
- **Index scan** — `SELECT ... WHERE col = value` uses B-tree index when available
- **ORDER BY / LIMIT / OFFSET** — sorted and paginated queries
- **INNER JOIN** — multi-table queries with `ON` clause
- **GROUP BY** — `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` aggregates
- **Extended protocol** — Parse/Bind/Execute for parameterized queries (`$1`, `$2`)
- **EXPLAIN** — query plan shows Index Scan vs Seq Scan
- **Table-level locking** — concurrent connections with read/write locks
- **Constraints** — PRIMARY KEY (auto-index) and NOT NULL enforcement
- **Data validation** — `moodengsql --check` verifies catalog/storage consistency
- **Configuration** — `moodeng.toml` for server, storage, and logging
- **Backup / restore** — gzip tar archives of the data directory
- **Health check** — `moodengsql ping` for load balancers and Docker
- **Docker** — Dockerfile and docker-compose for container deployment
- **CI** — GitHub Actions runs tests and release builds
- **Concurrent catalog** — lock-free table metadata via DashMap
- **Rich types** — INT4, INT8, FLOAT4, FLOAT8, TEXT, VARCHAR, BOOL, TIMESTAMP, JSON

## Quick Start

```bash
# Build everything
cargo build --release

# Start the server (default port 5432)
./target/release/moodengsql --data-dir ./moodeng_data

# Connect with the CLI (in another terminal)
./target/release/moodeng
```

## Example Session

```sql
CREATE TABLE users (
    id INT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT
);

INSERT INTO users VALUES (1, 'Alice', 'alice@moodeng.dev');
INSERT INTO users VALUES (2, 'Bob', 'bob@moodeng.dev');

SELECT * FROM users WHERE id = 1;

CREATE INDEX idx_users_email ON users (email);

UPDATE users SET email = 'alice@sql.dev' WHERE id = 1;
DELETE FROM users WHERE id = 2;
```

## Architecture

```
moodengsql/
├── moodeng-core/     # Storage engine, B-tree indexes, SQL executor
├── moodeng-server/   # Async TCP server (PostgreSQL wire protocol)
└── moodeng-cli/      # Interactive command-line client
```

| Component | Technology | Purpose |
|-----------|-----------|---------|
| SQL Parser | sqlparser (PostgreSQL dialect) | Parse incoming SQL |
| Storage | bincode + fsync | Durable row persistence |
| Indexes | BTreeMap | O(log n) lookups |
| Catalog | DashMap | Concurrent schema metadata |
| Server | tokio | Async connection handling |

## Server Options

MoodengSQL uses subcommands for operations. Legacy flat flags still work for `serve`:

```bash
# Start server (subcommand or legacy)
moodengsql serve --data-dir ./moodeng_data
moodengsql --data-dir ./moodeng_data --port 5432

# With config file
moodengsql serve --config moodeng.toml

# Validate data directory
moodengsql check --data-dir ./moodeng_data

# Backup and restore
moodengsql backup --output backup.tar.gz --data-dir ./moodeng_data
moodengsql restore --from backup.tar.gz --data-dir ./moodeng_data

# Health check
moodengsql ping --host 127.0.0.1 --port 5432
```

Copy `moodeng.toml.example` to `moodeng.toml` to customize host, port, data directory, and log level.

## Docker

```bash
docker compose up -d --build
docker compose exec moodengsql moodeng ping --config /etc/moodengsql/moodeng.toml
```

Data is persisted in the `moodeng_data` Docker volume at `/data`.

## Benchmark

```bash
cargo run --release --example benchmark -p moodeng-core
```

Prints insert and indexed point-select throughput for a quick sanity check.

## CLI Options

```
moodeng [OPTIONS]

Options:
  -h, --host <HOST>      Server host [default: 127.0.0.1]
  -p, --port <PORT>      Server port [default: 5432]
  -c, --command <CMD>    Execute a single SQL command and exit
```

## Performance Design

MoodengSQL prioritizes speed through:

1. **In-memory row cache** with lazy disk persistence
2. **B-tree indexes** for indexed column access instead of full scans
3. **Zero-copy value serialization** where possible
4. **Lock-free concurrent catalog** for multi-connection workloads
5. **Async I/O** with tokio for non-blocking connection handling

## License

MIT — Copyright (c) 2026 AidenHeavensXD
