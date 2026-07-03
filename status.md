# MoodengSQL — Project Status

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)  
**Repository:** [MoodengSQL](https://github.com/AidenHeavensXD/MoodengSQL)  
**Last updated:** 2026-07-03  
**Version:** v0.1 (pre-production)

---

## Summary

MoodengSQL is a PostgreSQL-inspired relational database written in Rust.  
Phases **0–3** and production hardening (TLS, buffer pool, backup consistency, SCRAM) are complete. INSERT ON CONFLICT remains optional future work.

**Verified test run (local, 2026-07-03):**

```bash
cargo test --workspace
# moodeng-core unit:           3 passed
# moodeng-core integration:   24 passed
# moodeng-core proptest:       4 passed
# moodeng-server auth+TLS+SCRAM: 10 passed
# Total: 41 tests, 0 failed
```

**Benchmark regression smoke:**

```bash
MOODENG_BENCH_MIN_INSERT=500 MOODENG_BENCH_MIN_SELECT=500 \
  cargo run --release --example benchmark -p moodeng-core
# Inserts: 5000 rows (~70k rows/s local)
# Point selects: 500 queries (~123k qps local)
```

| Milestone | Status |
|-----------|--------|
| Phase 0 — Metadata & constraints | ✅ Done |
| Phase 1 — WAL, transactions, planner | ✅ Done |
| Phase 2 — SQL + extended protocol | 🔄 SCRAM done; upsert pending |
| Phase 3 — Deploy & operations | ✅ Done |
| Phase 4 — Performance & hardening | ✅ Done |
| **Production-ready for real data** | 🔄 **Near** — CI re-run on GitHub pending |

---

## Production Blockers

| # | Item | Status | Verified by |
|---|------|--------|-------------|
| 1 | WAL replay fuzz + torn write | ✅ | proptest + integration |
| 2 | Row-level concurrency | ✅ | `fifty_concurrent_row_inserts` |
| 3 | Password auth (cleartext + SCRAM) | ✅ **2026-07-03** | `scram_sha256_handshake_succeeds`, `auth::tests::*` |
| 4 | TLS wire protocol | ✅ **2026-07-03** | 4 TLS protocol tests |
| 5 | Buffer pool / mmap lazy loading | ✅ **2026-07-03** | `paged_storage_queries_beyond_cache_limit` |
| 6 | Backup point-in-time consistency | ✅ **2026-07-03** | `backup_under_concurrent_writes_has_no_partial_transactions` |
| 7 | Row lock DashMap cleanup | ✅ **2026-07-03** | 100k lock/unlock test |
| 8 | Benchmark regression in CI | ✅ **2026-07-03** | `.github/workflows/ci.yml` floor 500 rows/s & qps |

### Remaining (non-blocking)

| Item | Status |
|------|--------|
| INSERT ON CONFLICT (upsert) | ⏳ Not started |
| GitHub Actions re-run after SCRAM commit | ⚠️ local pass only |

---

## SCRAM-SHA-256 Auth (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Wire protocol | AuthenticationSASL (10) → Continue (11) → Final (12) → Ok (0) |
| Credentials | `[auth].password_scram` or auto from `MOODENG_PASSWORD` |
| CLI | `moodengsql hash-password --scram "secret"` |
| Fallback | Cleartext auth type 3 when only `password_hash` (argon2) configured |
| Tests | `scram::tests::*`, `protocol::tests::scram_sha256_handshake_succeeds` |

---

## TLS Wire Protocol (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| PostgreSQL `SSLRequest` | `'S'` / `'N'` response, rustls upgrade |
| Config | `[server] tls_cert`, `tls_key`, `require_tls` |
| Password policy | Cleartext password requires TLS when only argon2 configured; SCRAM works on plaintext TCP |

---

## Buffer Pool (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Storage | `{table}.pages` mmap + LRU cache |
| Config | `[storage] max_cached_pages` (0 = legacy `.dat` mode) |

---

## Backup Consistency (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| Snapshot | `active_write_txns` wait + `backup_lock` + checkpoint |
| Test | Concurrent paired inserts during live backup |

---

## Definition of Done — Production MVP

| Criterion | Status |
|-----------|--------|
| Crash-safe WAL + txn atomicity | ✅ |
| 50+ concurrent row inserts | ✅ |
| TLS + SCRAM auth | ✅ **2026-07-03** |
| Buffer pool for large tables | ✅ |
| Live backup consistency | ✅ |
| Benchmark CI regression floor | ✅ |
| **All tests pass in CI** | ⚠️ pending GitHub run |
| INSERT ON CONFLICT | ❌ not done |

**Overall:** Production MVP criteria met locally (41 tests). Push to CI for final verification.

---

## Auth Setup

```bash
# SCRAM for psql/libpq clients
moodengsql hash-password --scram "your-secure-password"

# Argon2 fallback (cleartext wire auth type 3)
moodengsql hash-password "your-secure-password"
```

```toml
[auth]
password_scram = "SCRAM-SHA-256$4096:..."   # preferred for psql
password_hash = "$argon2id$..."            # optional cleartext fallback

[server]
tls_cert = "/path/to/cert.pem"
tls_key = "/path/to/key.pem"
require_tls = true
```

---

## Quick Commands

```bash
cargo test --workspace          # 41 tests
cargo build --release
moodengsql serve --config moodeng.toml
moodengsql hash-password --scram "secret"
psql "host=127.0.0.1 port=5432 user=moodeng password=secret"  # uses SCRAM when configured
```

---

*Updated after SCRAM auth and CI benchmark regression (2026-07-03).*
