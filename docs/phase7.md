# Phase 7 — Work Log (updates + operational polish)

Spec §25 Phase 7. Four tasks (Task 4 sequential after Task 3) + reviews + fix loops, controller wiring, final review + fix wave. Plan at `docs/superpowers/plans/2026-10-17-phase7.md`.

## What shipped

- `update_service.rs` + migration `0007_update.sql` + task arms: preview (warnings, no mutation), guarded enqueue (`UPDATE_NO_BACKUP`/`INVALID_IMAGE`/`NO_ROLLBACK_TARGET`/`SERVER_BUSY`), worker (preflight → protected auto-backup via canonical capture → pull → stop → remove → recreate (previous persisted FIRST) → readiness → audit; rollback on post-remove failure with mandatory one-way data warning; previous ref retained).
- `routes_admin.rs`: user create/delete (last-owner guard, session revoke), manager DB backup (`VACUUM INTO` snapshot download) + staged restore (magic + user_version + tables validation, aside-copy, pending file, applied on next boot — never a live swap).
- UI (design-led, `apps/web/DESIGN.md` written first with self-review): Users, Audit, Settings (DB card), Update section (preview warnings, 409→backup action, task poll, rollback with one-way warning); role-gated nav.
- a11y/responsive pass: focus-visible, live regions, table scroll, 40px targets, reduced-motion kill-switch.
- Wiring: 7 routes with permission layers, GET-server additive `previous_image_*` keys, pending-restore boot swap.

## Rulings

- Release artifacts/checksums → Phase 8 (its purpose).
- Dual-write disclosure carried into Update UI where image fields overlap (N/A — no image fields in guided form).
- Pre-existing `pull_image`-less trait extended with default body (fail-closed).
- Test-pool migration subsets completed per-file (shared helper still deferred — now more urgent).

## Verification evidence

- `cargo fmt --check`, `clippy --all-targets -D warnings`: clean. `cargo test`: 153 green. `npm run build`: clean.
- Live E2E: preview warnings → 409 without backup → stopped-backup → update 202 → succeeded/online → rollback with warning → online; user create/list; DB download (SQLite magic) → garbage 422 → staged 200 → restart applied → ready.

## Exit criteria (spec §25 Phase 7)

- Update failure preserves data + recoverable path (protected auto-backup + rollback, pinned). Permission changes audited. Manager recovery documented (`docs/backup-restore.md`) + tested (staged-restore cycle live-verified).
