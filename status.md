# MoodengSQL — Project Status

**Owner:** [AidenHeavensXD](https://github.com/AidenHeavensXD)  
**Repository:** [MoodengSQL](https://github.com/AidenHeavensXD/MoodengSQL)  
**Last updated:** 2026-07-03  
**Version:** v0.1 (production MVP)

---

## Summary

MoodengSQL is a PostgreSQL-inspired relational database written in Rust.  
All **Production MVP** criteria are met locally. Ready for controlled production deployment with the checklist below.

**Verified test run (local, 2026-07-03):**

```bash
cargo test --workspace
# moodeng-core unit:           3 passed
# moodeng-core integration:   28 passed
# moodeng-core proptest:       4 passed
# moodeng-server auth+TLS+SCRAM: 10 passed
# Total: 45 tests, 0 failed
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
| Phase 2 — SQL + extended protocol | ✅ Done (SCRAM + upsert) |
| Phase 3 — Deploy & operations | ✅ Done |
| Phase 4 — Performance & hardening | ✅ Done |
| **Production MVP** | ✅ **Ready locally** |

---

## Production Blockers — All Clear

| # | Item | Status | Verified by |
|---|------|--------|-------------|
| 1 | WAL replay fuzz + torn write | ✅ | proptest + integration |
| 2 | Row-level concurrency | ✅ | `fifty_concurrent_row_inserts` |
| 3 | Password auth (cleartext + SCRAM) | ✅ | `scram_sha256_handshake_succeeds` |
| 4 | TLS wire protocol | ✅ | 4 TLS protocol tests |
| 5 | Buffer pool / mmap lazy loading | ✅ | `paged_storage_queries_beyond_cache_limit` |
| 6 | Backup point-in-time consistency | ✅ | `backup_under_concurrent_writes_*` |
| 7 | Row lock DashMap cleanup | ✅ | 100k lock/unlock test |
| 8 | Benchmark regression in CI | ✅ | `.github/workflows/ci.yml` |
| 9 | INSERT ON CONFLICT (upsert) | ✅ **2026-07-03** | 4 upsert integration tests |

---

## INSERT ON CONFLICT (completed 2026-07-03)

| Feature | Detail |
|---------|--------|
| `DO NOTHING` | Skip duplicate rows silently |
| `DO UPDATE SET` | Update existing row on conflict |
| `EXCLUDED.col` | Reference proposed insert values in SET / WHERE |
| Conflict target | `(col)` or infer PRIMARY KEY |
| Persistence | Survives restart via WAL |

```sql
INSERT INTO users (id, name) VALUES (1, 'Alice')
  ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name;

INSERT INTO users (id, name) VALUES (1, 'Bob')
  ON CONFLICT DO NOTHING;
```

---

## Definition of Done — Production MVP

| Criterion | Status |
|-----------|--------|
| Crash-safe WAL + txn atomicity | ✅ |
| 50+ concurrent row inserts | ✅ |
| TLS + SCRAM auth | ✅ |
| Buffer pool for large tables | ✅ |
| Live backup consistency | ✅ |
| Benchmark CI regression floor | ✅ |
| INSERT ON CONFLICT | ✅ |
| **All tests pass in CI** | ⚠️ verify on GitHub after push |

**Overall:** Production MVP complete locally (45 tests).

---

## Go-Live Checklist (before real traffic)

Do these once before pointing production apps at MoodengSQL:

| Step | Action | Why |
|------|--------|-----|
| 1 | Push + confirm [GitHub Actions](https://github.com/AidenHeavensXD/MoodengSQL/actions) green | CI is the source of truth |
| 2 | Set `password_scram` + `require_tls = true` in `moodeng.toml` | No cleartext passwords on the wire |
| 3 | Use real TLS cert (Let's Encrypt / internal CA) | Self-signed breaks most clients |
| 4 | Set `[storage] max_cached_pages = 64` (or higher) for large tables | Avoid loading entire table into RAM |
| 5 | Schedule `moodengsql backup` (cron) + test restore on staging | Prove DR works |
| 6 | Run `moodengsql --check` after deploy | Catalog/storage consistency |
| 7 | Set `MOODENG_PASSWORD` via secrets manager, not shell history | Credential hygiene |
| 8 | Monitor disk usage on data dir + WAL | WAL grows until checkpoint |

---

## Recommended Next (post-MVP, not blockers)

These improve operability but are **not required** for a first production cut:

| Priority | Item | Notes |
|----------|------|-------|
| High | Connection limit + graceful shutdown | Prevent OOM under load spikes |
| High | Structured logging (request latency, slow queries) | Debug production issues |
| Medium | `max_connections` config | Wire protocol already async |
| Medium | Health metrics endpoint (Prometheus) | Beyond `ping` |
| Medium | Unique-index upsert on non-PK columns | Works if unique index exists |
| Low | MD5 auth (legacy clients) | SCRAM covers modern psql/libpq |
| Low | Replication / read replicas | Single-node is fine for MVP |
| Low | Full PostgreSQL SQL compatibility | Subset is intentional |

---

## Auth Setup

```bash
moodengsql hash-password --scram "your-secure-password"
moodengsql hash-password "your-secure-password"   # argon2 fallback
```

```toml
[auth]
password_scram = "SCRAM-SHA-256$4096:..."
password_hash = "$argon2id$..."

[server]
tls_cert = "/path/to/cert.pem"
tls_key = "/path/to/key.pem"
require_tls = true

[storage]
max_cached_pages = 64
```

---

## Quick Commands

```bash
cargo test --workspace          # 45 tests
cargo build --release
moodengsql serve --config moodeng.toml
moodengsql hash-password --scram "secret"
moodengsql backup --data-dir ./data --output backup.tar.gz
moodengsql --check --data-dir ./data
psql "host=127.0.0.1 port=5432 user=moodeng password=secret sslmode=require"
```

---

*Updated after INSERT ON CONFLICT upsert (2026-07-03).*
