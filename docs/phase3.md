# Phase 3 — Work Log (lifecycle, readiness, dashboard)

Spec §25 Phase 3: lifecycle + dashboard. Backend done inline (TDD);
frontend (`Overview`, `ServerDetail` lifecycle UI) by subagent
`ses_ec1fba03bffeKYJsSlPZob1SLk` against frozen contracts below.

## Contracts (frozen both sides)

- `GET /servers/:id/status` → `{id, desired_state, observed_state,
  readiness: {state: online|starting|stopped|unhealthy|unknown, reason,
  ping: {players_online, players_max, version, motd, latency_ms}|null},
  docker_available, container}`. `online` requires a live Java status ping.
- `GET /servers/:id/tasks?limit=&offset=` → `{tasks: [{id, kind (=type),
  type, state, ...}], total}` (server-scoped, newest first).
- `GET /metrics/summary` → `{host: {cpu_percent, memory_total_bytes,
  memory_used_bytes, disk_total_bytes, disk_free_bytes}, servers:
  [{server_id, name, cpu_percent|null, memory_bytes|null,
  memory_limit|null, state|null}]}`. Unavailable = explicit `null`.
- Lifecycle unchanged: `POST :id/start|stop|restart` → `202 {task_id}` or
  `200 {status: "already"}`; frontend polls `GET /tasks/:id` every 2s.

## Backend (TDD, RED→GREEN each)

- `readiness.rs` (new): hand-rolled Java status ping (handshake + status
  request, varint codec, chat-component MOTD extraction). Tests: fake
  status peer (parse/version/players), refused → `Unreachable` (not
  Timeout), garbage → `Malformed`. Plus `#[ignore]`d live test vs real
  Pumpkin, verified passing.
- `server_status`: pings `127.0.0.1:game_port` (3s) when container runs;
  `readiness_for()` maps desired/observed/ping → state+reason.
- `server_tasks`: per-server history endpoint.
- `metrics.rs` (new): `sysinfo` host stats (double CPU refresh, 250ms
  settle, `spawn_blocking`) + one-shot Docker stats per server.
- `task_runner`: `CancelRequested` → `cancelled` before any Docker call
  (`dispatch_task` now generic over `RuntimeAdapter`; `NeverCalled` stub
  in tests proves it); success arms sync `observed_state`
  (start/restart→running, stop→exited, remove→missing).
- `RuntimeError::NameConflict` → `409 CONTAINER_NAME_CONFLICT` (found live:
  stale `pumpkin-live` name collided; was raw 502 daemon text).

## Frontend (subagent, verified `npm run build`, 77→+ modules)

- `pages/Overview.tsx`: state counts, host cards, per-server table with
  readiness badges, alerts (docker down, unpinned image, missing
  container, desired≠observed drift), recent activity via
  `tasks?limit=3`. All queries have loading/empty/error states.
- `ServerDetail.tsx`: start/stop/restart with pending+poll flow,
  readiness + ping details, logs viewer (tail selector, 5s poll, copy),
  task history, metrics line. `routes.tsx`: `/` → Overview.

## Rulings

- Ping target is `127.0.0.1:game_port`, not `bind_address`: covers the
  common `0.0.0.0` case; LAN-IP bindings will read `starting` until a
  bind-aware prober lands (Phase 6). Cost if wrong: confusing badge for
  exotic binds — reason string says what was probed.
- Metrics CPU costs one 250ms settle per call; frontend polls ≥15s.
  Documented on the endpoint.
- `#[ignore]`d live-ping test stays in-tree: explicit command, no CI cost.

## Verification evidence (in-session)

- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`: clean.
- `cargo test`: **39/39 pass** (1 ignored live test, passing when run).
- `npm run build`: clean.
- Live E2E (`:18082`, real image): create → start `succeeded` → status
  `online` (`version 26.3`, `0/1000` players, 2ms) → metrics (real
  CPU 0.08%, mem ~15MB) → stop `succeeded` → `stopped|exited` → task
  history (2 succeeded) → delete 204 → reconcile `[]`.

## Exit criteria (spec §25 Phase 3)

- Full lifecycle from the browser: start/stop/restart buttons → 202 →
  poll → status refresh (backend proven via API; browser click-through is
  manual QA).
- Readiness never inferred from container existence: `online` gated on
  protocol ping; `starting` while the port is silent.
- Failure/recovery testable: task history surfaces failures;
  `requeue_interrupted` + reconcile from Phase 2 unchanged.

## Known dev-environment trap (documented, not code)

Manager-created data dirs inherit the manager UID; Pumpkin (2613) then
fails writes. Production fix = image entrypoint chowns per-server dirs
(Phase 7). Dev workaround: `chown -R 2613:2613 <leaf-dir>` only — never
the servers root, or the manager loses write access (hit twice today).

## TODOs for Phase 4

Bind-aware ping target, `Secure` cookie + login throttling (still open),
runner support for backup/restore task types, DB-backed op locks.
