# MoodengSQL — Project Status

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)  
**Repository:** [MoodengSQL](https://github.com/AidenHeavensXD/MoodengSQL)  
**Last updated:** 2026-07-03  
**Version:** v0.1 (pre-production)

---

## Summary

MoodengSQL is a PostgreSQL-inspired relational database written in Rust.  
Core engine phases **0–3** are complete. Phase **4** and **production hardening** are in progress.

**Verified test run (local, 2026-07-03):**

```bash
cargo test --workspace
# moodeng-core integration: 22 passed
# moodeng-core proptest (WAL): 4 passed
# moodeng-core unit (CRC32):  1 passed
# moodeng-server auth:         2 passed
# Total: 29 tests, 0 failed
```

| Milestone | Status |
|-----------|--------|
| Phase 0 — Metadata & constraints | ✅ Done |
| Phase 1 — WAL, transactions, planner | ✅ Done |
| Phase 2 — SQL + extended protocol | ✅ Done |
| Phase 3 — Deploy & operations | ✅ Done |
| Phase 4 — Performance & hardening | 🔄 Partial |
| **Production-ready for real data** | ❌ **Not yet** — see blockers below |

---

## Production Blockers (must fix before real data)

| # | Item | Status | Verified by |
|---|------|--------|-------------|
| 1 | WAL replay fuzz / property tests + torn write | ✅ | `wal_replay_proptest.rs` (4 tests), `wal_torn_last_entry_replays_prior_entries_only` |
| 2 | Row-level concurrency (not table-wide write lock) | ✅ | `fifty_concurrent_row_inserts` (50 clients × 10 rows), optimistic `Row.version` |
| 3 | Basic password auth on wire protocol | ✅ | `auth::tests::*`, argon2 + cleartext password handshake |
| 4 | Accurate status tracking | ✅ | This file |

### Remaining before public / high-trust deployment

| Item | Status |
|------|--------|
| Buffer pool / mmap for tables larger than RAM | ⏳ Not started |
| Backup under concurrent writes (snapshot isolation) | ⏳ Not tested |
| Benchmark regression tracking in CI | ⏳ Manual only |
| TLS / SCRAM auth | ⏳ Out of scope for MVP |

---

## Transaction Crash-Atomicity (completed 2026-07-03)

**Implemented by:** WAL txn markers + two-pass replay + CRC32 per entry.

| Feature | Detail |
|---------|--------|
| WAL ops | `Begin{txn_id}`, `Commit{txn_id}`, `Abort{txn_id}`, data ops carry `txn_id` |
| Auto-commit | Each statement = `Begin → op → Commit` |
| Recovery | Pass 1: group by txn; Pass 2: apply only committed, non-aborted data ops |
| CRC32 | Checksum mismatch stops replay at that entry (no full-file error) |

**Verified by `cargo test -p moodeng-core` (human-run, 2026-07-03):**

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
| Row-level locks + optimistic versioning | ✅ (was table-level write lock) |
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
| INSERT ON CONFLICT (upsert) | ⏳ |
| SCRAM / md5 auth | ⏳ (cleartext argon2 password only) |

---

## Phase 3 — Deploy & Operations ✅

| Task | Status |
|------|--------|
| `moodeng.toml` config | ✅ |
| CLI subcommands (`serve`, `check`, `backup`, `restore`, `ping`, `hash-password`) | ✅ |
| Backup / restore (gzip tar) | ✅ |
| Docker + docker-compose | ✅ |
| GitHub Actions CI | ✅ |

---

## Phase 4 — Performance & Hardening 🔄

| Task | Status | Notes |
|------|--------|-------|
| EXPLAIN query plans | ✅ | |
| WAL replay proptest + torn write | ✅ | `tests/wal_replay_proptest.rs` |
| Row-level concurrency + 50-client stress | ✅ | ~500 inserts parallel |
| Batch WAL fsync (every 10 ops) | ✅ | Trade-off: durability vs throughput |
| Benchmark suite | ✅ | `cargo run --release --example benchmark -p moodeng-core` |
| Buffer pool (memmap2) | ⏳ | |
| Backup during live writes | ⏳ | |

**Benchmark (release, 2026-07-03, single machine):**

| Workload | Throughput |
|----------|------------|
| 5,000 inserts | ~102k rows/s |
| 500 PK point selects | ~122k qps |
| 50 × 10 concurrent inserts | see test stderr (`fifty_concurrent_row_inserts`) |

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
| Index usage via EXPLAIN | ✅ tested |
| Docker + backup docs | ✅ documented |
| **All tests pass in CI** | ⚠️ local pass; CI not re-run this session |
| Buffer pool for large tables | ❌ not done |
| Safe for untrusted network without TLS | ❌ cleartext password only |

**Overall: NOT marked complete** — buffer pool, live-backup validation, and TLS/SCRAM remain.

---

## Auth Setup

```bash
# Generate hash for moodeng.toml
moodengsql hash-password "your-secure-password"

# Or set at runtime
export MOODENG_PASSWORD="your-secure-password"
moodengsql serve
```

```toml
[auth]
password_hash = "$argon2id$..."  # from hash-password
```

Without `password_hash` or `MOODENG_PASSWORD`, server runs in **trust mode** (warns on startup).

---

## Quick Commands

```bash
cargo test --workspace          # 29 tests
cargo build --release
moodengsql serve --config moodeng.toml
moodengsql hash-password "secret"
moodengsql ping --password secret
```

---

*Updated after WAL txn atomicity, row-level locking, auth, and fuzz tests. Re-run `cargo test --workspace` before each release.*
