# MoodengSQL — Project Status

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)  
**Repository:** [MoodengSQL](https://github.com/AidenHeavensXD/MoodengSQL)  
**Last updated:** 2026-07-03  
**Version:** v0.1 (pre-production)

---

## Summary

MoodengSQL is a PostgreSQL-inspired relational database written in Rust.  
Core engine phases **0–3** are complete. Phase **4** and **production hardening** are largely done; SCRAM auth remains out of scope.

**Verified test run (local, 2026-07-03):**

```bash
cargo test --workspace
# moodeng-core unit:           3 passed (CRC32, row-lock cleanup, page-table LRU)
# moodeng-core integration:   24 passed
# moodeng-core proptest (WAL): 4 passed
# moodeng-server (auth+TLS):   6 passed
# Total: 37 tests, 0 failed
```

| Milestone | Status |
|-----------|--------|
| Phase 0 — Metadata & constraints | ✅ Done |
| Phase 1 — WAL, transactions, planner | ✅ Done |
| Phase 2 — SQL + extended protocol | ✅ Done |
| Phase 3 — Deploy & operations | ✅ Done |
| Phase 4 — Performance & hardening | 🔄 Partial (SCRAM pending) |
| **Production-ready for real data** | 🔄 **Closer** — TLS + buffer pool + backup consistency done; SCRAM / CI re-run pending |

---

## Production Blockers (must fix before real data)

| # | Item | Status | Verified by |
|---|------|--------|-------------|
| 1 | WAL replay fuzz / property tests + torn write | ✅ | `wal_replay_proptest.rs` (4 tests), `wal_torn_last_entry_replays_prior_entries_only` |
| 2 | Row-level concurrency (not table-wide write lock) | ✅ | `fifty_concurrent_row_inserts` (50 clients × 10 rows), optimistic `Row.version` |
| 3 | Basic password auth on wire protocol | ✅ | `auth::tests::*`, argon2 + cleartext password handshake |
| 4 | Accurate status tracking | ✅ | This file |
| 5 | **TLS for wire protocol** | ✅ **2026-07-03** | `protocol::tests::tls_handshake_succeeds`, `ssl_request_without_cert_responds_n`, `require_tls_rejects_plaintext_startup`, `password_requires_tls_when_configured` |
| 6 | **Buffer pool / mmap lazy page loading** | ✅ **2026-07-03** | `page_table::tests::inserts_span_many_pages_with_small_cache`, `paged_storage_queries_beyond_cache_limit` |
| 7 | **Backup point-in-time under concurrent writes** | ✅ **2026-07-03** | `backup_under_concurrent_writes_has_no_partial_transactions` |
| 8 | **Row lock DashMap cleanup (bounded growth)** | ✅ **2026-07-03** | `lock::tests::row_locks_do_not_grow_unbounded_on_repeated_access` (100k acquire/release on same row) |

### Remaining before public / high-trust deployment

| Item | Status |
|------|--------|
| SCRAM-SHA-256 auth (cleartext password only today, but TLS-protected when cert configured) | ⏳ Not started |
| Benchmark regression tracking in CI | ⏳ Manual only |
| CI re-run after this session | ⚠️ local pass only |

---

## TLS Wire Protocol (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| PostgreSQL `SSLRequest` (`80877103`) | Respond `'S'` when cert/key configured, `'N'` otherwise (no crash) |
| Upgrade | `rustls` + `tokio-rustls` |
| Config | `[server] tls_cert`, `tls_key`, `require_tls` in `moodeng.toml` |
| Password on wire | When TLS is configured + auth enabled, cleartext password rejected on plaintext connections |
| Tests | 4 protocol TLS tests + existing auth tests |

---

## Buffer Pool / Page Storage (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Page files | `{table}.pages` with mmap2, fixed 64 KiB slots, length-prefixed bincode pages |
| LRU cache | Configurable via `[storage] max_cached_pages` (0 = legacy in-memory `.dat` mode) |
| Default | `max_cached_pages = 0` preserves existing behaviour for deployments without config change |
| Tests | 200-row insert with 2-page cache limit; query correctness + cache bound |

---

## Backup Consistency (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Mechanism | `backup_lock` (exclusive) + wait for `active_write_txns` + WAL checkpoint before tar |
| DML | Per-statement backup read lock; explicit transactions tracked until COMMIT/ROLLBACK |
| Test | Concurrent paired parent/child inserts during repeated `backup_live()` + restore validation |

---

## Row Lock Cleanup (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Mechanism | `RowLockWriteGuard` drops DashMap entry when `Arc::strong_count == 2` after unlock |
| Test | 100k update/delete cycles on the same row → `row_locks.len() == 0` |

---

## Transaction Crash-Atomicity (completed 2026-07-03)

**Implemented by:** WAL txn markers + two-pass replay + CRC32 per entry.

| Feature | Detail |
|---------|--------|
| WAL ops | `Begin{txn_id}`, `Commit{txn_id}`, `Abort{txn_id}`, data ops carry `txn_id` |
| Auto-commit | Each statement = `Begin → op → Commit` |
| Recovery | Pass 1: group by txn; Pass 2: apply only committed, non-aborted data ops |
| CRC32 | Checksum mismatch stops replay at that entry (no full-file error) |

**Verified by `cargo test -p moodeng-core`:**

- `crash_before_commit_discards_uncommitted_insert` ✅
- `crash_after_commit_persists_insert` ✅
- `crash_after_rollback_discards_insert` ✅
- `wal_torn_last_entry_replays_prior_entries_only` ✅
- `replay_wal_committed_only_applies_data_ops` (proptest) ✅

---

## Phase 0 — Fix Showstoppers ✅

| Task | Status |
|------|--------|
| Persist catalog + indexes (`meta.bin`) | ✅ |
| Startup recovery + `--check` | ✅ |
| PRIMARY KEY / NOT NULL enforcement | ✅ |
| Integration test foundation | ✅ |

---

## Phase 1 — MVP Production Core ✅

| Task | Status |
|------|--------|
| Write-ahead log + checkpoint | ✅ |
| Crash recovery (txn-aware WAL replay) | ✅ |
| BEGIN / COMMIT / ROLLBACK | ✅ |
| Index scan planner | ✅ |
| Row-level locks + optimistic versioning | ✅ |
| Max connections | ✅ |

---

## Phase 2 — SQL & Protocol ✅

| Task | Status |
|------|--------|
| ORDER BY / LIMIT / OFFSET | ✅ |
| INNER JOIN | ✅ |
| GROUP BY aggregates | ✅ |
| Extended wire protocol (Parse/Bind/Execute) | ✅ |
| Parameterized queries (`$1`, `$2`) | ✅ |
| TLS (SSLRequest + rustls) | ✅ **2026-07-03** |
| INSERT ON CONFLICT (upsert) | ⏳ |
| SCRAM / md5 auth | ⏳ (cleartext argon2 + TLS when cert configured) |

---

## Phase 3 — Deploy & Operations ✅

| Task | Status |
|------|--------|
| `moodeng.toml` config | ✅ |
| CLI subcommands (`serve`, `check`, `backup`, `restore`, `ping`, `hash-password`) | ✅ |
| Backup / restore (gzip tar) | ✅ |
| Live backup consistency | ✅ **2026-07-03** |
| Docker + docker-compose | ✅ |
| GitHub Actions CI | ✅ (not re-run this session) |

---

## Phase 4 — Performance & Hardening 🔄

| Task | Status | Notes |
|------|--------|-------|
| EXPLAIN query plans | ✅ | |
| WAL replay proptest + torn write | ✅ | `tests/wal_replay_proptest.rs` |
| Row-level concurrency + 50-client stress | ✅ | ~500 inserts parallel |
| Batch WAL fsync (every 10 ops) | ✅ | Trade-off: durability vs throughput |
| Benchmark suite | ✅ | `cargo run --release --example benchmark -p moodeng-core` |
| Buffer pool (memmap2 + LRU) | ✅ **2026-07-03** | opt-in via `max_cached_pages` |
| Backup during live writes | ✅ **2026-07-03** | txn-aware snapshot |
| TLS wire protocol | ✅ **2026-07-03** | |

---

## Definition of Done — Production MVP

| Criterion | Status |
|-----------|--------|
| Restart preserves schema, data, indexes | ✅ tested |
| WAL replay after crash | ✅ tested |
| Crash mid-transaction → uncommitted rows discarded | ✅ tested |
| Crash after COMMIT → data persists | ✅ tested |
| Crash after ROLLBACK → rolled-back data absent | ✅ tested |
| WAL fuzz / torn write robustness | ✅ proptest + integration |
| 50+ concurrent row inserts (same table) | ✅ tested |
| Password auth on wire protocol | ✅ tested |
| TLS wire protocol (SSLRequest + optional require_tls) | ✅ tested **2026-07-03** |
| Buffer pool for large tables | ✅ tested **2026-07-03** |
| Live backup without partial transactions | ✅ tested **2026-07-03** |
| Row lock map bounded after heavy reuse | ✅ tested **2026-07-03** |
| Index usage via EXPLAIN | ✅ tested |
| Docker + backup docs | ✅ documented |
| **All tests pass in CI** | ⚠️ local 37/37; CI not re-run this session |
| SCRAM auth | ❌ not done |
| Safe for untrusted network without TLS | ❌ use TLS + password when exposing publicly |

**Overall:** TLS, buffer pool, backup consistency, and row-lock cleanup are done and tested. SCRAM and CI verification remain before marking full production MVP complete.

---

## Auth & TLS Setup

```bash
# Generate hash for moodeng.toml
moodengsql hash-password "your-secure-password"

# Generate self-signed cert (example)
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 365 -nodes -subj "/CN=localhost"
```

```toml
[server]
tls_cert = "/path/to/cert.pem"
tls_key = "/path/to/key.pem"
require_tls = true   # reject plaintext startup

[auth]
password_hash = "$argon2id$..."  # from hash-password

[storage]
max_cached_pages = 64   # 0 = legacy in-memory .dat mode
rows_per_page = 16
```

When TLS cert/key are configured and auth is enabled, passwords are only accepted on TLS connections.

Without `password_hash` or `MOODENG_PASSWORD`, server runs in **trust mode** (warns on startup).

---

## Quick Commands

```bash
cargo test --workspace          # 37 tests
cargo build --release
moodengsql serve --config moodeng.toml
moodengsql hash-password "secret"
moodengsql ping --password secret
```

---

*Updated after TLS, buffer pool, backup consistency, and row-lock cleanup (2026-07-03). Re-run `cargo test --workspace` before each release.*
