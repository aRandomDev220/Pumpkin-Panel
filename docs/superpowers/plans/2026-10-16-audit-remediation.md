# Audit Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every correctness, security, concurrency, data-safety, and build/CI defect found in the read-only audit of Pumpkin Manager, without adding net-new product features.

**Scope ruling (human-approved):** All bugs + hardening. Do **NOT** implement the missing-vs-plan features (`PATCH /api/v1/servers/{id}`, `GET`/`PATCH /api/v1/settings`, WebSocket `/api/v1/ws/events`, idempotency keys). Those are tracked separately.

**Spec:** `pumpkin-manager-implementation-plan.md` (§5.3 trust boundaries, §8 schema/integrity, §9.4 error model, §9.5 concurrency, §10 lifecycle, §11.4 image flow, §12 auth/security, §13 config, §14 files, §15 backups, §16 tasks).

## Global Constraints

- Rust edition 2021. `cargo fmt --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean (only documented `#[allow]` with a reason comment).
- TypeScript strict, no `any`, `npm run build --prefix apps/web` clean, oxlint clean if configured.
- TDD: write the failing test first, watch it fail, minimal code, then the full suite green. Tests must assert behavior, not implementation detail.
- **Never weaken security or data safety to make a test pass.**
- SQL: bind parameters only. Never interpolate user input into SQL. Never log passwords, tokens, secrets, or file contents.
- Client-facing errors never contain absolute filesystem paths, container ids, or raw subprocess stderr. Log detail server-side; return a stable code + safe message to clients (see §9.4). Keep existing error-code vocabulary.
- Permission changes and destructive actions remain audited. Do not regress audit coverage.
- Commit only the task's own files, one conventional-commit message per task (`fix:`/`refactor:`/`test:`/`chore:`). Do not `git add -A`.
- Implementers must not edit files owned by another task. Shared interfaces are frozen in this plan.
- No subagents may be spawned by implementers. Review is dispatched by the controller after the report.

## Execution model

Tasks run **sequentially** (one implementer at a time, reviewed before the next starts).
Reason: the backend is a single Rust crate — one compilation unit — so two half-applied
tasks cannot both compile. A task may touch any file it needs to keep the crate green
(including callers), but must not regress a task that already completed.
The web app is a separate build and is done as one task (Task 10) to avoid the same issue.

## Frozen shared interfaces (from Task 1, consumed by Tasks 3–9)

Current `AppState.op_locks` is `Arc<tokio::sync::Mutex<HashSet<String>>>` where the `String`
is the server id. Four modules currently define their own `try_lock`/`unlock` helpers over
that type; those helpers are only correct on the success path and leak on early return.

`crates/backend/src/op_lock.rs` (new, created by Task 1) — **keep the `OpLocks` type identical
to the current field type so nothing else breaks**:

```rust
/// Kept identical to `AppState::op_locks`.
pub type OpLocks = std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>;

/// RAII guard: removes the id from the set when dropped (works on every path,
/// including `?` early returns and panics).
pub struct OpGuard { /* holds OpLocks + key */ }
impl Drop for OpGuard {
    fn drop(&mut self) {
        let locks = self.locks.clone();
        let key = std::mem::take(&mut self.key);
        // Drop cannot await; removal is spawned. Callers that need to re-acquire
        // immediately should loop (see `acquire`).
        tokio::spawn(async move { locks.lock().await.remove(&key); });
    }
}

/// Non-blocking. Returns None if another operation holds the id
/// (caller maps to 409 SERVER_BUSY).
pub async fn try_acquire(locks: &OpLocks, server_id: &str) -> Option<OpGuard>;

/// Blocking acquire for workers; retries until it wins.
pub async fn acquire(locks: &OpLocks, server_id: &str) -> OpGuard;
```

`crates/backend/src/task_runner.rs` (Task 1) exposes:

```rust
/// Atomically claim a queued task for this worker. Returns Some(task) only for
/// the single caller whose conditional UPDATE matched; None if already claimed
/// or cancelled. Implemented with `UPDATE ... WHERE status='queued'`.
pub async fn claim_task(pool: &SqlitePool, task_id: &str) -> Result<Option<TaskRecord>, sqlx::Error>;
```

**Rule:** Tasks 3–9 replace their local `try_lock`/`unlock` helpers with `op_lock::try_acquire`
and hold the returned guard in a variable for the scope of the operation (bind it as `_guard`,
not `_`). Do not re-implement locking.

## Review Focus

- A server locked by a crashed/panicked operation must never stay locked forever (RAII guard test).
- Two workers must never run the same task (atomic claim test); a cancel must never resurrect a finished task or double-run a running one.
- An interrupted restore must never have its pre-restore prior-data copies silently deleted at boot.
- A partial/failed multi-step write (config save, user delete, backup delete, archive import) leaves the DB consistent.
- No unauthenticated request can read a file outside the web root (traversal).
- Client errors never leak filesystem paths / raw stderr.
- Deleting a user who owns servers/backups/tasks/audit rows succeeds and nulls provenance (no silent 204 that leaves rows).

---

## Task 1: Foundation — schema integrity, op-lock RAII, atomic task claim

**Files:**
- Create: `migrations/0008_integrity.sql`, `crates/backend/src/op_lock.rs`
- Modify: `crates/backend/src/db.rs` (migration list/version if referenced), `crates/backend/src/task_runner.rs`, `crates/backend/src/main.rs` (only: `mod op_lock;`, and the SPA static fallback traversal fix), plus schema-dependent inline DDL inside any `#[cfg(test)]` module in `crates/backend/src/*.rs` **only if the test suite fails to compile/run** because of the schema change.

**Findings to fix:**
1. **FK cascade / delete_user silent failure (High).** `audit_events`, `tasks`, `backups`, `servers` (and any other table with a `*_user_id`/`requested_by`/`created_by` FK) currently reference `users(id)` with no `ON DELETE` action, so deleting a user fails at the FK. Rebuild those tables so every provenance FK is `ON DELETE SET NULL` (spec §8.12). SQLite cannot alter FK actions in place, so use the 12-step table-rebuild procedure. Note sqlx wraps migrations in a transaction; `PRAGMA foreign_keys` cannot be toggled inside it — use `PRAGMA legacy_alter_table=ON` + rebuild, or split with `PRAGMA foreign_keys=OFF` handled correctly. **Verify with a test that has FK enforcement on** (`PRAGMA foreign_keys=ON`) that deleting a user with referencing rows succeeds and nulls the FKs.
2. **Soft-delete UNIQUE blocks reuse (Medium).** `servers.name`, `servers.slug`, and `servers.game_port` are table-level UNIQUE, so a soft-deleted server keeps the name/port and a new server cannot reuse them. Replace with **partial unique indexes** `... WHERE deleted_at IS NULL` (rebuild the `servers` table as in #1). Keep a non-unique index on the full name/slug columns.
3. **Missing indexes (Medium).** Add indexes the queries rely on: `sessions(user_id)`, `sessions(expires_at)`, `tasks(server_id, status)`, `tasks(status, created_at)`, `backups(server_id, created_at)`, `metrics_samples(server_id, sampled_at)`, `audit_events(created_at)`, `config_revisions(server_id, created_at)`. Confirm against actual query predicates.
4. **Orphan queued tasks wedge a server (High).** In `task_runner`, when a task is enqueued but submission/spawn fails (or the manager restarts with tasks stuck in a non-terminal state), the server becomes permanently `SERVER_BUSY`. Add: atomic `claim_task` (frozen interface), rollback of the enqueue when spawn fails (mark the task `failed` in the same transaction/step that enqueued it), and a startup reconciliation that marks non-terminal tasks (`queued`/`running`/`cancel_requested`) from a previous boot as `failed` with a `interrupted` reason.
5. **Non-atomic claim → cancel resurrection / double run (High).** `mark_running` must be a compare-and-set: `UPDATE tasks SET status='running', ... WHERE id=? AND status='queued'`; only the worker whose `rows_affected()==1` runs it. `request_cancel` on a `queued` task sets `cancelled` directly (CAS); on a `running` task sets `cancel_requested`; a `queued`→`cancelled` CAS must not be undone by a late `mark_running` (covered by the CAS).
6. **`unreachable!()` in task dispatch (Low).** Replace panics on unexpected task/state with a structured failure that marks the task `failed` and audits it.
7. **Raw subprocess/error text stored as task `error_summary` (Medium).** Sanitize to a safe summary; store detail only in server logs.
8. **Unauthenticated path traversal in SPA fallback (Critical).** In `main.rs` the static-file fallback joins the caller path and checks `Path::starts_with`, which does not resolve `..` (confirmed: `Path::new("/a/b").starts_with("/a")` is true, but the fallback also fails to canonicalize `..`). Fix: reject any request path containing a `..` component, percent-decoded, before joining; then canonicalize the joined path and require it to be within the canonicalized static root; return 404 otherwise. Add a regression test using `..%2f`/`..%2F`/raw `..` that asserts 404 and no file read.
9. Create `op_lock.rs` with the frozen API and a `Drop` test (acquire → drop → re-acquire succeeds; concurrent try_acquire returns None).

**Tests (Step 1, write before code):**
- `delete_user_nulls_fk_and_succeeds` (FK ON; user owns a server, a task, an audit row → delete → row gone, FKs NULL, referencing rows still present).
- `soft_deleted_server_frees_name_slug_port` (soft-delete a server, create a new one with same name/slug/port → 201).
- `claim_task_is_single_winner` (two concurrent `claim_task` on one queued task → exactly one `Some`, other `None`).
- `cancel_queued_then_late_claim_does_not_run` (request_cancel on queued → claim returns None → status `cancelled`).
- `orphan_queued_task_reconciled_on_boot` (insert `running`/`queued` row, run reconcile → `failed`).
- `spa_fallback_rejects_traversal` (GET `/../Cargo.toml`, `/..%2fCargo.toml`, `/%2e%2e/Cargo.toml` → 404, body does not contain file contents).
- `op_lock_released_on_drop` / `op_lock_mutual_exclusion`.

- [ ] Step 1: read `pumpkin-manager-implementation-plan.md` §8.12 + §16, and the current `migrations/*`, `db.rs`, `task_runner.rs`, `main.rs` static handler. Write the failing tests above.
- [ ] Step 2: run; confirm each new test fails for the expected reason.
- [ ] Step 3: implement migration + op_lock + task_runner CAS/reconcile + traversal fix.
- [ ] Step 4: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green.
- [ ] Step 5: commit: `git add migrations/0008_integrity.sql crates/backend/src/op_lock.rs crates/backend/src/db.rs crates/backend/src/task_runner.rs crates/backend/src/main.rs` (+ any test-DDL-only edits) → `git commit -m "fix: schema integrity, RAII op locks, atomic task claim, SPA traversal"`.

---

## Task 2: Auth, session transport, CSRF, router hardening (runs AFTER Task 1; owns `main.rs`)

**Files:**
- Create: `crates/backend/src/csrf.rs`, `migrations/0009_app_settings.sql` (persistent setup-complete + session TTL override flag)
- Modify: `crates/backend/src/auth.rs`, `crates/backend/src/routes_auth.rs`, `crates/backend/src/config.rs`, `crates/backend/src/main.rs` (auth middleware, security headers, module wiring), `crates/backend/Cargo.toml` (add rate-limit dep only)

**Findings to fix:**
1. **Token in localStorage / weak cookie (High).** Make the **HttpOnly `pm_session` cookie the sole session carrier**: `Set-Cookie: pm_session=...; HttpOnly; SameSite=Lax; Path=/; Secure` (Secure always on; if dev over http is needed, gate via explicit `PUMPKIN_INSECURE_COOKIES=1` env with a warning log). Stop returning the session token in the login/setup response body. Use the `__Host-` prefix when not insecure.
2. **No CSRF protection (High).** Add double-submit CSRF: on login issue a random `pm_csrf` cookie (not HttpOnly so JS can read it) and require `X-CSRF-Token` to equal it on all unsafe methods (POST/PUT/PATCH/DELETE). Enforce in middleware for cookie-authenticated requests. Exempt: `POST /api/v1/auth/login` and `POST /api/v1/auth/setup` (no session yet) — but still require a same-origin `Origin`/`Referer` check on those. Reject with `403 CSRF_FAILED`. Add tests: missing/incorrect header → 403; correct → 200.
3. **No rate limiting on login/setup (High).** Add per-IP (and per-username) rate limiting to `POST /auth/login` and `POST /auth/setup` (e.g. token-bucket: 5 attempts/min/IP, lockout window). Return `429 RATE_LIMITED` with `Retry-After`. The existing comments claim this exists — make it true. Add a test that N+1 rapid failures yield 429.
4. **Setup race creates multiple owners (High).** Make first-user creation atomic and one-shot: `INSERT INTO users(...) SELECT ... WHERE NOT EXISTS (SELECT 1 FROM users)` inside a single transaction, checking `rows_affected()==1`, else `409 SETUP_COMPLETE`. Persist a `setup_complete` flag in `app_settings` (migration 0009) and refuse re-setup once set even if the users table later becomes empty (fixes "setup re-enabled when users empty"). Add a concurrent-setup test asserting exactly one owner and one 201.
5. **Unknown-user timing side channel (Medium).** On login for a non-existent username, still run Argon2id against a fixed dummy hash before returning the generic invalid-credentials error, so response time does not distinguish unknown user.
6. **Weak input validation (Medium).** Enforce a password max length (e.g. 1024) before hashing (DoS), reject control characters, and cap username length/character set. Reject over-long JSON bodies.
7. **Session TTL / expiry (Medium).** Make session TTL configurable via `config.rs` (env with a sane default), and ensure logout deletes the server-side session row and clears both cookies.
8. **Security headers (High, part of missing headers).** Add a headers layer: `Content-Security-Policy` (no `unsafe-eval`; allow `'self'`; for the SPA allow what it needs without `unsafe-inline` scripts), `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Permissions-Policy` minimal, and HSTS when TLS-terminated (behind a config flag). Add a test asserting the headers are present on a normal response.
9. Ensure the SPA reads CSRF from the cookie and sends `X-CSRF-Token` (frontend Task 10 consumes this contract).

**Tests:** `login_does_not_return_token_in_body`, `cookie_is_httponly_and_samesite`, `mutation_without_csrf_403`, `mutation_with_csrf_ok`, `login_rate_limited`, `concurrent_setup_single_owner`, `setup_disabled_after_complete`, `unknown_user_and_wrong_password_have_similar_timing` (assert dummy-hash path executed via a seam, not wall-clock flakiness), `security_headers_present`.

- [ ] Step 1: write the failing tests.
- [ ] Step 2: run; confirm expected failures.
- [ ] Step 3: implement.
- [ ] Step 4: fmt/clippy/test green.
- [ ] Step 5: commit: `git add` the owned files → `git commit -m "fix: cookie sessions, CSRF, rate limiting, atomic setup, security headers"`.

---

## Task 3: Server lifecycle + runtime + readiness (owns `routes_servers.rs`, `runtime.rs`, `readiness.rs`)

**Findings to fix:**
1. **`image_digest` never applied to the container (High).** When creating/recreating a container, use the pinned digest reference (`image@sha256:...`) if present, not just the tag. Add a test asserting the created container's image reference includes the digest when one is set.
2. **Image allowlist accepts `:master` / mutable tags as pinned, and validation is cosmetic (High).** Reject empty/unpinned references that are not in the allowlist; if `image_digest` is provided it must be a valid `sha256:` digest; when an update is requested with a digest, verify the pulled image's digest matches and fail otherwise. Keep the Docker trust-boundary rule (§5.3): never accept arbitrary images.
3. **Delete server does not check busy state (Medium).** Refuse `DELETE /servers/:id` (409 SERVER_BUSY) when an operation holds the op-lock / a non-terminal task exists; ensure container removal + volume handling is ordered and audited.
4. **Start is not idempotent vs observed state (Medium).** Base the start/stop decision on observed container state (§10.1) and make repeated start/stop return the current state rather than blindly queueing.
5. **Create server without a runtime adapter returns a confusing error instead of 503 (Medium).** Return `503 RUNTIME_UNAVAILABLE` when no Docker adapter is configured.
6. **`readiness_for` checks order is wrong (Medium).** Check container-present/running before TCP/Java ping so a missing container reports the right state.
7. **Runtime error strings leak (Medium).** Map bollard/subprocess errors to safe client messages; log detail.
8. **Input structs missing `#[serde(deny_unknown_fields)]` (Low).** Add where appropriate so typos in API payloads are rejected.
9. **Use `op_lock::try_acquire` RAII guard** for all lifecycle entry points (replace manual insert/remove that leaks on error).

**Tests:** digest applied; unpinned/typosquat rejected before any Docker call; busy delete 409; double-start idempotent; no-adapter 503; readiness ordering; error sanitization (no `bollard` detail in body); unknown field rejected.

- [ ] Steps 1–5 as in Task 1. Commit: `git commit -m "fix: server lifecycle, image digest pinning, readiness ordering"`.

---

## Task 4: Backups + scheduler (owns `routes_backup.rs`, `backup_service.rs`, `scheduler.rs`)

**Findings to fix:**
1. **`startup_cleanup` deletes `.recovery-*` prior-data copies at boot (Critical).** Never delete recovery copies automatically. On boot, if an interrupted restore left staged/prior-data files, **report** them (audit event + surfaced state) and leave them in place for manual recovery. Fix the cleanup to only remove clearly-expired temporary scratch files (documented pattern), never `*.recovery-*`/prior-data.
2. **Op-lock leaks on error (High).** Replace manual lock insert/remove with the `op_lock::acquire` RAII guard in all backup/scheduler paths.
3. **`delete_backup` TOCTOU / can delete the last verified backup (High).** Do the existence + retention check and the delete in one transaction/CAS; refuse deleting the last verified backup for a server unless an explicit force flag, and return the same result under concurrency.
4. **Unbounded in-memory buffering / no free-space check (High).** Stream archive creation/restore; check free disk space before starting; cap sizes; never read a whole world into a `Vec` (this also applies to Task 5 and is shared — coordinate: backup_service streams).
5. **Retention preview vs worker disagree (Medium).** Use one shared retention-selection function for both preview and execution.
6. **Path leak in responses/audit (Medium).** Strip absolute paths from client responses and audit metadata.
7. **Scheduler cron parsing limited to 5 fields but validation loose (Medium).** Validate 5-field crontab properly and reject unsupported syntax at save time; add tests for `*/5 * * * *`, ranges, invalid input.
8. **Scheduler double-enqueue / races (Medium).** Use atomic claim + idempotent enqueue; skip if the server is busy instead of erroring the schedule into a bad state; record the outcome.
9. **Blocking filesystem IO in async handlers (Medium).** Wrap blocking fs/archive work in `tokio::task::spawn_blocking`.
10. **Audit de-duplication / noise (Low).** Remove runtime DDL from `scheduler.rs` (schema comes from migrations); drop duplicate/misleading audit events.
11. **Symlink handling in export must match Task 5's file-service rules (Medium).** Do not follow symlinks out of the data dir.

**Tests:** interrupted restore leaves recovery copy after boot; op-lock released on error path; concurrent delete_backup → single winner, never removes last verified; free-space rejection; retention preview == worker selection; cron validation; scheduler busy-skip.

- [ ] Steps 1–5. Commit: `git commit -m "fix: backup recovery safety, retention, scheduler integrity"`.

---

## Task 5: File manager (owns `routes_files.rs`, `file_service.rs`)

**Findings to fix:**
1. **Hardlink escape (High).** `check_link_target` validates against a different base than `hard_link` uses; align them so a target outside the data dir (including via a hardlink whose kept path is inside) is rejected. Re-check after resolving; reject cross-directory and special files.
2. **Unbounded archive/import buffering + no limits (High).** Stream export/import; enforce max depth, max part count, max total uncompressed size, and free-space checks; reject zip-slip/`..` entries.
3. **`create_directory` / mutations not guarded by server-busy state (Medium).** Refuse while an operation holds the op-lock.
4. **Import/export not using observed state (Medium).** Ensure the server is stopped/readiness-gated per §10 before touching world files.
5. **TOCTOU on file ops (Medium).** Re-validate the resolved path immediately before the operation; use no-follow opens where possible.
6. **`Content-Disposition` filename not sanitized (Medium).** Sanitize/encode the filename.
7. **Tempfile leak on error (Low).** Clean up temp files on all error paths.
8. **Blocking IO in async (Medium).** `spawn_blocking`.
9. **`FileError` leaks internal messages / wrong status (Low).** Map to safe messages + correct status codes.

**Tests:** hardlink/symlink escape rejected (symlink to `/etc/passwd`), zip-slip entry rejected, oversize import rejected before decompress, busy guard 409, filename sanitized, temp files removed on error.

- [ ] Steps 1–5. Commit: `git commit -m "fix: file manager path safety, streaming, limits"`.

---

## Task 6: Config editor (owns `routes_config.rs`, `config_profile.rs`)

**Findings to fix:**
1. **Key handling / injection (Medium).** Reject keys with newlines/quotes/section separators; validate against the profile schema; escape values when writing TOML.
2. **Empty `data_path` config (Medium).** Reject/stabilize an empty `data_path`; never operate on `.`.
3. **Multi-step write not transactional across IO+DB (Medium).** Write to temp, fsync, write revision row and swap atomically; roll back file on DB failure.
4. **Op-lock leaks (Medium).** Use the RAII guard.
5. **Symlink following (Medium).** Refuse writing through a symlink escaping the data dir; no-follow.
6. **`restart_required` correctness (Medium).** Compute it from what actually changed, not blanket-true.
7. **DB errors / `expect()` in non-test code (Medium).** Remove `expect()`; propagate errors.
8. **Error masking (Low).** Don't return 500 for user input errors; return 422 with field info.

**Tests:** key injection rejected; empty data_path rejected; file/DB rollback on induced failure; symlink escape rejected; restart_required reflects changes; no `expect` panic paths.

- [ ] Steps 1–5. Commit: `git commit -m "fix: config editor validation, atomicity, path safety"`.

---

## Task 7: Metrics (owns `routes_metrics.rs`, `metrics.rs`, `metrics_collector.rs`)

**Findings to fix:**
1. **Daemon-unreachable reported as running/zeroed (Medium).** Distinguish "daemon unreachable" from "server stopped"; surface an explicit status.
2. **Repeated-crashes filter wrong (Medium).** Fix the window/aggregation so crash loops are detected and reported correctly.
3. **Alerts swallow DB errors (Medium).** Propagate/log DB errors instead of silently returning empty.
4. **Blocking IO in collector (Medium).** `spawn_blocking`.
5. Ensure samples are bounded/pruned so the table cannot grow unbounded (retention).

**Tests:** unreachable vs stopped distinct; crash-loop detected; DB error surfaced; retention prunes.

- [ ] Steps 1–5. Commit: `git commit -m "fix: metrics status accuracy, collector robustness"`.

---

## Task 8: Update + rollback service (owns `update_service.rs`)

**Findings to fix:**
1. **Orphan queued task on update/rollback (High).** Use atomic enqueue with rollback (Task 1 infra) so a failed spawn cannot wedge the server.
2. **Worker op-lock leak (High).** RAII guard.
3. **`is_daemon_down` brittleness (Medium).** Determine from structured error kinds, not string matching; add tests.
4. **Blocking IO in worker (Medium).** `spawn_blocking`.
5. **Error sanitization (Medium).** No raw docker/stderr in client messages.
6. **Digest verification** consistent with Task 3 (reject mismatch, keep the previous reference for rollback exactly as §11.4).

**Tests:** failed spawn does not leave a queued task; op-lock released after failure; digest mismatch fails the update; no path/detail leak.

- [ ] Steps 1–5. Commit: `git commit -m "fix: update/rollback task safety and digest verification"`.

---

## Task 9: User admin (owns `routes_admin.rs`)

**Findings to fix:**
1. **`delete_user` silent FK failure returns 204 + audit ok (High).** Wrap the delete in a transaction, check `rows_affected()`, return `409`/`422` on failure; with Task 1's `ON DELETE SET NULL` the delete now succeeds and provenance nulls. Audit the true outcome.
2. **Last-owner TOCTOU (High).** Perform the "count active owners" check and the delete/demote in one serialized transaction (or a guarded UPDATE) so two concurrent demotions cannot remove all owners.
3. **`update_user` not transactional / unchecked (Medium).** Check `rows_affected`, validate role, single transaction.
4. **Restore validation gaps (High).** Validate SQLite magic, `user_version` within supported range, required tables present, and reject newer schemas before staging; never swap live (already staged — keep). Add tests.
5. **DB backup/restore buffering + file copy of WAL DB (Medium).** Use `VACUUM INTO` for a consistent snapshot and stream; free-space check.
6. **Diagnostics/error path leaks (Medium).** Sanitize.
7. **Blocking IO (Medium).** `spawn_blocking`.
8. **`create_user` username validation (Low).** Validate characters/length; rely on UNIQUE for the 409.

**Tests:** delete_user with owned rows succeeds and audits ok (regression for #1); concurrent demote of last two owners → one 409; update_user rows_affected; restore rejects non-SQLite/unknown-schema/incomplete; no path leak.

- [ ] Steps 1–5. Commit: `git commit -m "fix: user admin transactions, last-owner guard, restore validation"`.

---

## Task 10: Frontend auth + app shell (owns `apps/web/src/lib/auth.tsx`, `api.ts`, `csrf.ts` (new), `routes.tsx`, `App.tsx`, `apps/web/index.html`)

**Findings to fix:**
1. **Token in localStorage (High).** Stop reading/writing the token in JS. Rely on the HttpOnly cookie; send `credentials: 'include'`. Read the non-HttpOnly `pm_csrf` cookie and send `X-CSRF-Token` on unsafe methods. `/auth/login` + `/auth/setup` responses no longer carry a token.
2. **No 401 handling (High).** On any 401 from the API, clear cached session state/queries and redirect to /login.
3. **Cache not cleared on logout (High).** On logout, clear TanStack Query cache and any in-memory user state before navigating.
4. **Route gating uses stale/local state (Medium).** Gate routes on the `me` query result (RequireAuth), and hide viewer-inappropriate controls (Tasks/Update/Danger) in the shell/routes.
5. **CSP meta/unused `Redis`? (Low).** Add a restrictive CSP `<meta>` consistent with Task 2's header (no `unsafe-eval`), and remove any dev-only inline scripts from `index.html`.
6. Ensure `api.ts` centralizes the base URL, `credentials`, CSRF header, and 401 handling so pages don't each re-implement it.

**Verification (no unit runner for web):** `npx tsc --noEmit -p apps/web/tsconfig.app.json`, `npm run build --prefix apps/web`, and code inspection against the checklist. Add a tiny pure-function test for the CSRF-cookie reader if a runner is configured; otherwise document inspection.

- [ ] Steps: implement → typecheck → build → commit `git commit -m "fix: cookie/CSRF auth, 401 handling, cache clearing, route gating"`.

---

## Task 11: Frontend pages + types (owns `apps/web/src/pages/*`, `apps/web/src/lib/{servers,backups,config,files,monitoring,admin}.ts`, `apps/web/tsconfig*.json`)

**Findings to fix:**
1. **Cross-server config bleed (High).** `Config.tsx` (and any per-server page) must key local state on the server id (`key={id}` on the routed element or reset state on id change) so switching servers cannot save server A's config to server B. Add a guard: refuse save if the loaded revision/server id doesn't match.
2. **TOML/JSON value escaping (Medium).** Escape values per the backend contract; never send unescaped user text into TOML.
3. **`ServerDetail` / `Files` stale state on id change (Medium).** Reset query-derived local state when the id changes.
4. **Unsafe casts / `as any` (Medium).** Remove `as any` casts (e.g. `taskProgress`); type the API responses; make the API types match the backend JSON exactly (frozen keys). No `any`.
5. **tsconfig strictness (Medium).** Ensure `strict`, `noUncheckedIndexedAccess` (if feasible without a huge cascade — otherwise document), and `noImplicitAny` are on; no `skipLibCheck` masking.
6. **Monitoring chart a11y/casts (Low).** Add text alternatives/labels; remove casts.
7. **Error message surfaces raw errors (Low).** Show safe messages.
8. **Dead code / unused exports (Low).** Remove unused components/exports (`retry`/`stale` helpers referenced by audit).
9. **Audit list keys (Low).** Use stable keys.
10. **Viewer gating in pages (Medium).** Hide/disable controls a viewer cannot use; reconcile with Task 10's shell gating.

**Verification:** typecheck + build + inspection checklist.

- [ ] Steps: implement → typecheck → build → commit `git commit -m "fix: frontend per-server state, types, a11y, gating"`.

---

## Task 12: Infra — Docker, compose, CI, docs, workspace lints (final; owns `deploy/*`, `.github/*`, `docs/security.md`, `Cargo.toml` workspace-lints section)

**Findings to fix:**
1. **Docker volume ownership (High).** The container runs as non-root but `/data` is root-owned, so it cannot write. Fix the Dockerfile to `chown`/prepare `/data` (and the DB path) for the runtime user, or use an entrypoint that fixes ownership; verify with the documented run. Update `docker-compose.yml` accordingly.
2. **CI uses mutable action tags (Medium).** Pin all GitHub Actions to commit SHAs; add `permissions:` least-privilege; add `cargo audit` and `npm audit --omit=dev` (fail on high/critical); add a Dependabot config.
3. **No `cargo audit` / dependency scanning (Medium).** Wire it in CI.
4. **`docs/security.md` inaccurate claims (Low).** Correct statements to match the fixed behavior (cookie/CSRF, rate limiting).
5. **Workspace lints (Medium).** Add `[workspace.lints]` denying `unsafe_code`; deny `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented` in non-test code (tests may `allow`), and reference it from the crate `Cargo.toml`s. Then fix all violations in `crates/backend/src`. If a denial cannot be satisfied without changing behavior, add a narrowly-scoped `#[allow(...)]` with a reason comment. This task is allowed to touch `crates/backend/Cargo.toml` and `crates/backend/src/*` **only** for lint fixes after Tasks 1–9 are committed.
6. **`.env.example` / docs for new env vars** introduced in Task 2 (`PUMPKIN_INSECURE_COOKIES`, session TTL).

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean under new lints; `cargo test --workspace` green; `docker compose config` valid if available; `npm audit` clean.

- [ ] Steps: implement → verify → commit `git commit -m "chore: infra hardening, CI pinning, workspace lints, docs"`.

---

## Final verification (controller, after all tasks)

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (full)
- `npx tsc --noEmit -p apps/web/tsconfig.app.json`
- `npm run build --prefix apps/web`
- `cargo audit` (if the tool/network is available; else note)
- Whole-branch code review.
