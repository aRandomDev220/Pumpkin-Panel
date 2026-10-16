# Phase 6 — Work Log (monitoring + automation)

Spec §25 Phase 6. One implementer per task (3 lost to rate limits mid-flight — work recovered from tree, verified, repaired once); reviews + fix loops; controller wiring; final review + fix wave. Plan at `docs/superpowers/plans/2026-10-15-phase6.md`.

## What shipped

- `metrics_collector.rs` + migration `0006_metrics.sql`: per-server sampling (Docker stats + ping + restart_count, NULLs-never-zeros, one row/server/call), 7-day / 50k-row prune; `RuntimeSnapshot.restart_count` (`-1` unknown).
- `routes_metrics.rs`: history (`1h|24h|7d`, 1000-cap, DB errors → 500) + stateless alerts (disk-pressure, repeated-crashes ≥3/h, failed-backup, unverified-image, missing-backup; single failures never alert).
- Scheduler restarts (`scheduled_tasks` table, due/overdue catch-up + audit, no-duplicate) executed as claimed task rows; diagnostics export (version, os/arch, schema, docker, servers, capabilities, last-20 failures, per-server config validation — secrets scanned absent).
- UI: Monitoring page (host cards, CSS-bar history with null-gaps, alerts, diagnostics download), ServerDetail metrics section, Overview alerts embed.
- Wiring: 6 routes with permission layers, collector on the 60s loop, allows removed.

## Rulings

- No metric aggregation (raw bound only; 50k rows fine for SQLite).
- WebSocket events deferred to Phase 7 (polling drives all UI).
- `sample_all`/history-query canonical reuse enforced (two fix rounds); DB failures → 500, sampler degrades.
- Truthful states only (`paused→paused`, unknown→`unknown`).
- Rate-limit-decimated sessions recovered from tree + controller verification instead of re-dispatch (cost: extra controller verification load; no quality shortcut — all reviews still ran).

## Verification evidence

- `cargo fmt --check`, `clippy --all-targets -D warnings`: clean. `cargo test`: 133 green (bounds, redaction, reboot-once, flap guards, canonical pins). `npm run build`: clean.
- Live E2E: real container sampled (running + CPU), alerts fired (unverified-image), diagnostics leak-free, scheduled restart FIRED and succeeded, metrics accumulated, full delete cycle.

## Exit criteria (spec §25 Phase 6)

- Unsupported metrics shown as null, never zero (pinned). Scheduler single catch-up, no duplicates (pinned). Storage bounded (7d + 50k, pinned with 60k-row test).

## TODOs for Phase 7

Login throttling + `Secure` cookie (still open), lifecycle runner for backup/restore task types (dedicated worker for now), download streaming, metric aggregation for >7d history, per-policy disk alerts.
