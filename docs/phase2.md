# Phase 2 — Work Log (workstream C: frontend + docs)

## What was built (frontend only, no backend changes)

- `apps/web/src/lib/auth.tsx` — `AuthProvider`, `useAuth`, token store
  (`pm_token` in memory + `localStorage` fallback), `authFetch` attaching
  `Authorization: Bearer`, `fetchSetupStatus` / `fetchMe` / `postLogin` /
  `postSetup` / `postLogout`. Setup wizard gated on `needs_setup`.
- `apps/web/src/lib/servers.ts` — typed `ServerSummary` / `ServerDetail` /
  `TaskStatus`, CRUD + task helpers, `digestWarning`, `validateCreateServer`.
- `apps/web/src/pages/Login.tsx` — login form, redirects to `/servers`.
- `apps/web/src/pages/Setup.tsx` — one-time admin form (min 8 chars, confirm
  match); renders "Setup complete → login" when `needs_setup === false`.
- `apps/web/src/pages/Servers.tsx` — list with `desired:` / `observed:` badges,
  create form (`name`, `game_port`, `image_reference` + digest warning),
  delete with retain-data checkbox (`?retain_data=`).
- `apps/web/src/pages/ServerDetail.tsx` — overview + desired/observed status +
  task lookup via `GET /api/v1/tasks/:id`.
- `apps/web/src/routes.tsx` — exports `<AppRoutes/>`; `RequireAuth` redirects
  to `/setup` when `needs_setup`, else `/login` when no token.

## API contracts (frontend assumptions)

| Method | Path | Auth | Body / Params |
|---|---|---|---|
| GET | /api/v1/health/ready | none | — |
| GET | /api/v1/auth/setup-status | none | → `{ needs_setup: boolean }` |
| POST | /api/v1/auth/setup | none (first run) | `{ username, password }` → `{ token, user }` |
| POST | /api/v1/auth/login | none | `{ username, password }` → `{ token, user }` |
| POST | /api/v1/auth/logout | Bearer | — (401 tolerated, client clears token anyway) |
| GET | /api/v1/auth/me | Bearer | → `{ id, username, role }` |
| GET | /api/v1/servers | Bearer | → `{ servers: ServerSummary[] }` |
| POST | /api/v1/servers | Bearer | `{ name, game_port, image_reference }` |
| GET | /api/v1/servers/:id | Bearer | → `ServerDetail` |
| DELETE | /api/v1/servers/:id?retain_data=bool | Bearer | 204 on success |
| GET | /api/v1/tasks/:id | Bearer | → `{ id, kind, state, server_id?, error? }` |

## RBAC matrix (enforced server-side; frontend mirrors via guards)

| Capability | Unauthenticated | Authenticated user | Admin |
|---|---|---|---|
| setup-status / setup (first run) | allow (only while `needs_setup`) | n/a | n/a |
| login | allow | allow (re-login) | allow |
| me / logout | deny | allow | allow |
| list/get servers | deny (→ /login) | allow | allow |
| create/delete servers | deny | deny (backend 403) | allow |

## Validation rules (client-side, backend re-validates)

- `name`: required, `/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/`.
- `game_port`: integer 1–65535.
- `image_reference`: required; digest warning (non-blocking) unless it contains
  `@sha256:`.
- Setup: password ≥ 8 chars, confirm must match.

## Reconcile behavior (documented expectation)

- `desired_state` = intent (e.g. `running` / `stopped`); `observed_state` =
  last reconciled reality. List/detail show both badges; divergence means a
  reconcile is pending or failed (`last_error` shown on detail).
- Start/stop/reconcile ops return a task id; `ServerDetail → Task lookup`
  polls `GET /api/v1/tasks/:id` (`pending → running → done/error`).

## Test checklist

- [ ] Fresh DB: `/` → `/setup`; setup creates admin → `/servers`.
- [ ] Second visit: `/setup` shows "Setup complete".
- [ ] Bad login shows error; good login persists `pm_token` across reload.
- [ ] No token + `/servers` → `/login`.
- [ ] Create with `latest` tag shows digest warning; with `@sha256:` does not.
- [ ] Invalid name/port shows client validation before any request.
- [ ] Delete asks confirm + retain-data checkbox; list refreshes.
- [ ] `npm run build --prefix apps/web` passes strict TS.
- [ ] Detail page loads server + task lookup round-trips.

## Open TODOs for wiring

- Apply the `App.tsx` patch below (wrap with `AuthProvider`, render
  `<AppRoutes/>`; keep `HealthBadge` + `QueryClientProvider`).
- Confirm `DELETE` query param name (`retain_data`) and `POST /servers`
  request/response shapes with backend workstream; adjust `lib/servers.ts`.
- Confirm `user.role` values and hide create/delete for non-admins.
- Add start/stop buttons once backend task-issuing endpoints land.
- Replace task manual lookup with polling (`refetchInterval`) after contract freeze.

## App.tsx patch (apply exactly, 10 lines)

```diff
--- a/apps/web/src/App.tsx
+++ b/apps/web/src/App.tsx
@@
-import { BrowserRouter, Link, Route, Routes } from "react-router-dom";
-import { fetchHealth, fetchServers } from "./lib/api";
+import { BrowserRouter, Link } from "react-router-dom";
+import { fetchHealth } from "./lib/api";
+import { AuthProvider } from "./lib/auth";
+import AppRoutes from "./routes";
@@
-      <BrowserRouter>
+      <BrowserRouter><AuthProvider>
         <div className="shell">
@@
-            <Routes>
-              <Route path="/" element={<Dashboard />} />
-            </Routes>
+            <AppRoutes />
           </main>
         </div>
-      </BrowserRouter>
+      </AuthProvider></BrowserRouter>
```

---

# Phase 2 — Integration log (backend wiring, 2026-10-15)

Subagents drafted `auth.rs`/`routes_auth.rs` (A), `runtime.rs`/`task_runner.rs`
(B), frontend pages (C). Integration done inline: `audit.rs`,
`routes_servers.rs`, `routes_admin.rs`, `main.rs` router/state/startup,
`tests_phase2.rs`, contract fixes, `App.tsx` patch.

## Rulings

- **Backend adapts to frontend contracts** (frontend built first): `setup-status`
  returns `{needs_setup}` (was `{setup_required}`); setup/login return
  `{token, user}` (was `{token}`); task JSON includes `kind` alias for `type`.
  Cost if wrong: none — strictly more compatible.
- **No-Docker create persists the row** (`container_id=""`, observed `created`)
  instead of failing. Rationale: inventory-first lets owners stage servers on
  hosts whose socket appears later; lifecycle then 503s honestly. Cost: a
  `start` on a containerless row 409s (`MISSING_CONTAINER`) until recreated.
- **DELETE is synchronous** (204) in Phase 2; task-based delete deferred to the
  Phase 7 update flow. Cost: a slow daemon removal blocks one request (with
  body cap + timeouts this is acceptable for single-node).
- **Cancel only `queued` tasks**; `running` Docker ops 409 (`TASK_CANCEL_UNSAFE`)
  rather than being abandoned mid-write. Runner cancel-polling is a Phase 3 TODO.
- **Login throttling deferred**: `routes_auth` notes the rate-limit layer; no
  governor wired yet ( hardening gate in Phase 8 + `security-audit` skill).
- **UID 2613 data-dir ownership unsolved in-process**: `create` does
  `create_dir_all` only; recursive chown needs root the backend may not have.
  Fix belongs to the image entrypoint (root chown → drop privs), Phase 3/7.
  Verified live: Pumpkin fails `Permission denied` on shivesh-owned dirs,
  boots clean after `chown -R 2613:2613`.

## Verification evidence (all in-session)

- `cargo fmt --check`, `cargo clippy -p pumpkin-manager-backend --all-targets
  -- -D warnings`: clean (documented allows only: later-phase enum variants,
  `result_large_err` on `Response` errors, audit arg count).
- `cargo test -p pumpkin-manager-backend`: **31/31 pass** (6 auth unit + 2 config
  + 6 runtime/task unit from workstream B + 17 verified there + 6 new
  integration: setup_flow, unauthenticated_is_rejected,
  server_crud_without_docker, rbac_matrix, tasks_and_reconcile, last_owner_guard).
- `npm run build --prefix apps/web`: clean, 77 modules (routes bundled).
- Live E2E vs real daemon (`:18081`, image
  `ghcr.io/pumpkin-mc/pumpkin:master`): setup → login → me → create (real
  container `a57ebe8a`, digest warning) → start task `succeeded` → status
  `running/running` → logs tail → stop task `succeeded` → status
  `exited/stopped` → audit 5 events → reconcile `[]` → delete 204 →
  reconcile `[]`. Idempotency (`already`), 409 busy/missing, 503 no-runtime,
  403 viewer-mutates, 404 unknown task, last-owner 409 all exercised.

## Exit criteria (spec §25 Phase 2)

- Unauthenticated/unauthorized rejected: covered by integration tests.
- Records ↔ Docker reconcile after restart: `requeue_interrupted` +
  `sync_observed` + read-only `reconcile` at boot; on-demand
  `POST /admin/reconcile`.
- Only validated specs reach Docker: `validate_spec` allowlist + `deny_unknown_fields`.

## TODOs for Phase 3

Runner cancel-polling, per-server lock table → DB-backed (multi-process),
observed-state sync on task success (currently lazy via status + boot),
login rate limiting, `Secure` cookie flag behind HTTPS detection.
