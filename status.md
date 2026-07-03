# MoodengSQL — Project Status

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)  
**Repository:** [MoodengSQL](https://github.com/AidenHeavensXD/MoodengSQL)  
**Last updated:** 2026-07-03  
**Version:** v0.1 (production MVP in progress)

---

## Summary

MoodengSQL is a PostgreSQL-inspired relational database written in Rust.  
Phases **0–3 are complete**. Phase **4 is in progress**.  
**20 integration tests** pass locally (including crash-atomicity tests).

| Milestone | Status |
|-----------|--------|
| Phase 0 — Metadata & constraints | ✅ Done |
| Phase 1 — WAL, transactions, planner | ✅ Done |
| Phase 2 — SQL + extended protocol | ✅ Done |
| Phase 3 — Deploy & operations | ✅ Done |
| Phase 4 — Performance & hardening | 🔄 In progress |
| Transaction crash-atomicity (WAL txn + CRC32) | ✅ Done |
| Production MVP (Definition of Done) | 🔄 Mostly met — see checklist below |

---

## Phase 0 — Fix Showstoppers ✅

| Task | Status | Notes |
|------|--------|-------|
| Persist catalog + indexes (`meta.bin`) | ✅ | Schema survives restart |
| Startup recovery + `--check` | ✅ | `moodengsql check` |
| PRIMARY KEY / NOT NULL enforcement | ✅ | Auto PK index `{table}_pkey` |
| Integration test foundation | ✅ | Restart, PK, NOT NULL tests |

---

## Phase 1 — MVP Production Core ✅

| Task | Status | Notes |
|------|--------|-------|
| Write-ahead log + checkpoint | ✅ | `wal.log`, checkpoint every 50 ops |
| Crash recovery (WAL replay) | ✅ | Two-pass txn-aware replay |
| BEGIN / COMMIT / ROLLBACK | ✅ | WAL `Begin`/`Commit`/`Abort` + undo log |
| Index scan planner | ✅ | `WHERE col = value` uses B-tree |
| Table-level locking | ✅ | Read/write locks per table |
| Max connections | ✅ | Semaphore in server |

---

## Phase 2 — SQL & Protocol ✅

| Task | Status | Notes |
|------|--------|-------|
| ORDER BY / LIMIT / OFFSET | ✅ | |
| INNER JOIN | ✅ | `ON` clause with `table.column` |
| GROUP BY aggregates | ✅ | COUNT, SUM, AVG, MIN, MAX |
| Extended wire protocol | ✅ | Parse / Bind / Execute / Sync |
| Parameterized queries | ✅ | `$1`, `$2` via `substitute_params` |
| INSERT ON CONFLICT (upsert) | ⏳ | Planned, not implemented |
| Basic auth (password) | ⏳ | Planned, not implemented |

---

## Phase 3 — Deploy & Operations ✅

| Task | Status | Notes |
|------|--------|-------|
| `moodeng.toml` config | ✅ | `[server]`, `[storage]`, `[log]` |
| CLI subcommands | ✅ | `serve`, `check`, `backup`, `restore`, `ping` |
| Backup / restore | ✅ | gzip tar of data directory |
| Docker + docker-compose | ✅ | Volume `/data`, health check |
| GitHub Actions CI | ✅ | `cargo test` + release build |

---

## Phase 4 — Performance & Hardening 🔄

| Task | Status | Notes |
|------|--------|-------|
| EXPLAIN query plans | ✅ | Index Scan vs Seq Scan |
| Concurrent client test | ✅ | 10 clients × 10 inserts |
| Batch WAL fsync | ✅ | Sync every 10 ops; flush on checkpoint |
| Benchmark suite | ✅ | `cargo run --release --example benchmark -p moodeng-core` |
| **Transaction crash-atomicity** | ✅ | WAL txn markers + CRC32 + replay filtering |
| Buffer pool (memmap2) | ⏳ | Not started |
| Fuzz testing (parser + WAL) | ⏳ | Not started |

**Latest benchmark (release, local):**

| Workload | Result |
|----------|--------|
| 5,000 inserts | ~102k rows/s |
| 500 point selects (PK) | ~122k qps |

---

## WAL Transaction Model (new)

Each write is tagged with a `txn_id`:

```
Begin{txn_id} → Insert/Update/Delete{txn_id,…} → Commit{txn_id} | Abort{txn_id}
```

- **Auto-commit** statements wrap as a single txn: `Begin → op → Commit`
- **Recovery** (two-pass): only replay data ops from txns with a `Commit` record (no `Abort`)
- **CRC32** per WAL entry — torn/corrupt entries stop replay at that point (no full-file error)

---

## Definition of Done — Production MVP

| Criterion | Status |
|-----------|--------|
| Restart preserves schema, data, indexes | ✅ |
| WAL replay after crash / reopen | ✅ |
| Transactions (BEGIN / COMMIT / ROLLBACK) | ✅ |
| **Crash mid-transaction does not persist uncommitted rows** | ✅ |
| **Crash after COMMIT persists committed rows** | ✅ |
| **Crash after ROLLBACK does not persist rolled-back rows** | ✅ |
| 10 concurrent clients without corruption | ✅ |
| Index usage visible via EXPLAIN | ✅ |
| Parameterized queries (extended protocol) | ✅ |
| Docker deploy + backup/restore documented | ✅ |
| Integration tests pass in CI | ✅ (local; push pending) |

---

## Test Coverage

```
cargo test -p moodeng-core   → 20 tests passing
cargo test --workspace       → full workspace green
```

Key tests: metadata persist, WAL replay, transactions, JOIN, GROUP BY, backup/restore, EXPLAIN, concurrent inserts, **crash-before-commit**, **crash-after-commit**, **crash-after-rollback**, WAL CRC32 unit test.

---

## Architecture

```
moodengsql/
├── moodeng-core/     # Engine: storage, WAL, executor, indexes, transactions
├── moodeng-server/   # Binary: moodengsql (TCP, PostgreSQL wire protocol)
├── moodeng-cli/      # Binary: moodeng (interactive client)
├── Dockerfile
├── docker-compose.yml
└── .github/workflows/ci.yml
```

---

## Quick Commands

```bash
# Build
cargo build --release

# Run server
./target/release/moodengsql serve --data-dir ./moodeng_data

# Health check
./target/release/moodengsql ping

# Backup
./target/release/moodengsql backup --output backup.tar.gz

# Benchmark
cargo run --release --example benchmark -p moodeng-core

# Docker
docker compose up -d --build
```

---

## Git History (recent)

| Commit | Description |
|--------|-------------|
| `729b381` | Phase 4: EXPLAIN, batch WAL, benchmark, status.md (local) |
| `c3d4fe0` | Phase 3: config, Docker, backup/restore, ping, CI |
| `5165c75` | Phase 2: ORDER BY, JOIN, GROUP BY, extended protocol |
| `3fffa44` | Initial commit (Phase 0/1 core) |

---

## Next Up

1. Buffer pool with `memmap2` page cache  
2. Fuzz testing for SQL parser + WAL replay  
3. Optional: upsert (`ON CONFLICT`), basic password auth  
4. Phase 5+ (out of MVP scope): MVCC, replication, SSL/TLS

---

*This file is updated as phases complete. See [README.md](README.md) for usage docs.*
