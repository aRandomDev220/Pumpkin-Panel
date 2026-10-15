# Phase 5 — Work Log (backups + restore)

Spec §25 Phase 5. Four parallel implementers + reviews + fix loops, controller wiring, final review + fix wave. Plan at `docs/superpowers/plans/2026-10-15-phase5.md`.

## What shipped

- `backup_service.rs`: full-data tar.gz snapshots, SHA-256 verify-by-relist, pure retention decision fn (floor: never delete the last verified / protected / in-progress), cron validation.
- `routes_backup.rs` + migration `0005_backups.sql`: 7 endpoints (manual backup, list, get, download, delete, restore) with a semaphore-gated worker (concurrency 1) that never touches `desired_state` (stop/resume directly, resume failures recorded honestly). Restore requires stopped, verifies ownership + checksum, auto-creates a protected pre-restore backup (aborts if that fails), stages + validates, keeps a recovery copy until success. Retention floor enforced on delete (`409 BACKUP_PROTECTED`).
- `scheduler.rs`: policy CRUD, dry-run retention preview, 60s `tick_once` (due/overdue catch-up audited, busy skips, next_run recompute). Non-UTC timezones stored with an "interpreted as UTC" note — no silent mis-scheduling.
- UI: Backups section (run with consistency select + live warning, task-polled progress, table with protected locks + errors, download/delete/restore with confirms, stopped-guard) + policies form with preview.
- Wiring: 11 routes with permission layers, `AppState.backup_dir` single source of truth, scheduler loop + boot cleanup (backup root AND server data-dir parents).

## Rulings

- `full`-only mode (`world`/`config` → 422; no verified partial layout exists).
- Single `enqueue_backup`/`enqueue_restore` entry points (no pickup loop).
- Worker prune honors the server's policy via the canonical decision fn (preview and worker share it after the fix wave).
- PUT accepts pre-merged content pattern NOT repeated here — N/A.
- `main.rs` layer addition for Multipart cap accepted as only-possible-location (Phase 4 lineage).

## Verification evidence

- `cargo fmt --check`, `clippy --all-targets -D warnings`: clean. `cargo test`: 113 green (attack, corruption, floor, pre-restore-abort, foreign-backup, 8-way PUT, policy convergence tests). `npm run build`: clean.
- Live curl E2E: backup → checksum verified → mutate → restore (exact bytes, auto-protected safety net) → floor 409s → policy/preview → delete cycle.
- Playwright E2E untouched paths still green (verified post-change).

## Exit criteria (spec §25 Phase 5)

- Real world backed up and restored (live-verified), corrupt archives rejected (`BACKUP_CORRUPT`, data untouched), failure preserves the last verified backup (floor + protected + recovery copies).

## TODOs for Phase 6

Bind-aware ping, login throttling + `Secure` cookie (still open), backup/restore task types in the main lifecycle runner (currently a dedicated worker), download streaming (currently buffered), empty-restore default, busy-skip retry nudge, shared test-helpers module.
