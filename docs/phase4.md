# Phase 4 — Work Log (config + files)

Spec §25 Phase 4. Five parallel implementers + reviews + fix loops, controller wiring, final review + fix wave. Full SDD ledger in git history; plan at `docs/superpowers/plans/2026-10-15-phase4.md`.

## What shipped

- `config_profile.rs`: version-aware profiles (`known` for 26.x), `toml_edit`-based parse/validate/merge preserving unknown keys + formatting, SHA-256 content hashes. Guided fields grounded in the real generated `pumpkin.toml` (dumped from the image, not memory).
- `routes_config.rs` + migration `0003_config_revisions.sql`: GET/PUT/validate/revisions/revert with the §13.2 transaction (rev-check → validate → pre-change copy → tempfile+rename → re-read → bump → audit, `restart_required: true`). Stale writes → `409 CONFIG_CONFLICT`; garbage → `422`; divergence (file newer than DB) surfaced as additive `diverged`/`diverged_reason` on GET, never auto-healed. Saves take the per-server op lock (`409 SERVER_BUSY`), no mutex held across I/O.
- `file_service.rs`: sandboxed resolve (lexical + canonical re-check, symlink-escape refused), atomic writes, safe tar.gz import (validate-before-write, 100k files / 10 GiB caps) and export.
- `routes_files.rs`: list/download/upload/mkdir/rename/delete/import/export with `ServerFilesRead/Write` gates, busy guards, audit, streaming multipart (axum extractor, 1 GiB cap, `DefaultBodyLimit::disable()` on the uploads router — axum enforces its own 2 MiB default independent of tower layers).
- UI: Config page (guided form + raw TOML + validate + save/409/422 handling + revision history with content preview + revert + restart notice + unknown-profile warning + dual-write "Java + Bedrock" labels) and Files page (browser, upload/download, mkdir/rename/delete, stopped-gated import, export).

## Rulings

- Dual-write java+bedrock kept (brief shape; per-edition fields would cascade 3 tasks) with UI disclosure. Cost if wrong: hand-diverged editions unify on guided save (raw TOML unaffected).
- PUT accepts pre-merged content; guided merge lives client-side, server re-validates. `apply_guided` reserved for a future server-side endpoint.
- Backend adapts: `list_revisions` entries carry full `content`; GET-invalid returns 200 + `valid:false` (PUT garbage stays 422); rename auto-creates parents.
- Controller wired all routes + split body limits; `main.rs` one-line layer addition accepted as only-possible-location for the Multipart cap fix.

## Verification evidence

- `cargo fmt --check`, `clippy --all-targets -D warnings`: clean. `cargo test`: 74+ green (incl. attack tests, 8-way PUT single-winner, malicious-tar 400 pin). `npm run build`: clean.
- Live curl E2E: full config transaction cycle (revert restores exact hash), traversal→400, upload/download round trip, delete cycle.
- Playwright E2E suite from the prior plan still green (untouched paths).

## Exit criteria (spec §25 Phase 4)

- Config preserved across restarts (SQLite revisions + file on disk, reconciled via `diverged` flag).
- External edits trigger conflict handling (revision check + divergence detection).
- Path/archive traversal tests pass (5 attack tests + route-level pins).

## TODOs for Phase 5

Bind-aware ping, login throttling + `Secure` cookie (still open), backup/restore task types in the runner, shared test-helpers module (duplicated pool/migration splitters), route-level 413 via real 1 GiB body (impractical — unit + 8 MiB route test stand in).
