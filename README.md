# 🎃 Pumpkin Manager

[![CI](https://github.com/aRandomDev220/Pumpkin-Panel/actions/workflows/ci.yml/badge.svg)](https://github.com/aRandomDev220/Pumpkin-Panel/actions)

**Self-hosted control plane for [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) Minecraft servers.**
Create, start, configure, back up, monitor, and update Pumpkin servers from a clean web UI —
one Docker container per game server, one SQLite database, zero cloud dependencies.

**Release candidate:** `0.1.0` — Phase 8 verification is documented in
[`docs/phase8.md`](docs/phase8.md). The source artifact and SHA-256 checksum are in
[`release/`](release/).

> **Truthful by design:** the dashboard reports a server as *online* only after a real
> Minecraft status ping answers — never just because a container exists.

---

## Features

**Lifecycle & dashboard**
- Create/start/stop/restart game servers; per-server desired vs. observed state
- Readiness-gated status (`online` requires a live protocol ping) with player counts and latency
- Live logs, CPU/memory stats, task history with progress

**Configuration (`pumpkin.toml`)**
- Guided form (verified fields only) + raw TOML editor that preserves unknown keys
- Syntax validation, optimistic-concurrency revisions, one-click revert
- External-edit detection (file changed outside the panel → conflict, never silent overwrite)

**Files**
- Sandboxed browser: list, upload (streamed, 1 GiB cap), download, mkdir, rename, delete
- Safe world import/export as `tar.gz` (traversal, symlink-escape, and device entries rejected)
- Path-traversal attacks rejected at the API boundary with stable error codes

**Backups & restore**
- Full-data snapshots with SHA-256 checksums, verified by re-listing after every write
- Stop-before-backup consistency (or explicit live-with-warning mode)
- Restore requires a stopped server, auto-creates a protected pre-restore backup, keeps a recovery copy
- Scheduled policies (cron) with retention floors — the last verified backup is never deleted
- Manager's own database: one-click download + staged, restart-applied restore

**Monitoring & automation**
- Persisted per-server metrics (7-day / 50k-row bound), history charts with honest gaps (no zero-filling)
- Stateless alerts: disk pressure, crash loops, failed backups, unpinned images, missing backups
- Scheduled restarts, redacted diagnostics export for troubleshooting

**Updates & administration**
- Image updates with compatibility warnings, protected pre-update backup, and one-click rollback
  (with an explicit warning that world-data format changes are one-way)
- Owner/admin/viewer roles, per-endpoint permissions, full audit trail
- Responsive, keyboard-navigable, reduced-motion-respecting dark UI

---

## How it works

```
Browser ──► Pumpkin Manager (single image: Rust Axum API + embedded Vite UI)
                    │  ▲ SQLite (users, servers, tasks, backups, metrics, audit)
                    │  │ /var/run/docker.sock (backend only — never the browser)
                    ▼  │
             pumpkin-<slug> × N (one container per game server, own data mount)
```

- The manager is a **control plane**, not a game server: a crash in one world never takes down the panel or your other worlds.
- Long operations (start, backup, restore, update) run as **persisted tasks** — refresh the page or restart the manager and they resume or fail safe, never vanish.
- Every sensitive action writes an **audit event** (who, what, target, outcome).

---

## Quick start (Docker Compose — recommended)

```sh
git clone https://github.com/aRandomDev220/Pumpkin-Panel.git
cd Pumpkin-Panel
cp deploy/.env.example deploy/.env   # no secrets in defaults; review ports/paths
docker compose -f deploy/docker-compose.yml up -d --build
```

Open `http://<host>:8080`, create the owner account on first launch, then create your first
server. Only the ports you allocate per server (e.g. `25565/tcp` + `19132/udp`) need
forwarding — never the Docker socket. See [`docs/installation-casaos.md`](docs/installation-casaos.md)
for CasaOS, [`docs/compatibility/pumpkin-runtime.md`](docs/compatibility/pumpkin-runtime.md)
for the pinned Pumpkin image + capability matrix, and [`docs/security.md`](docs/security.md)
for the trust model.

> **Heads-up for bind mounts:** Pumpkin runs as UID/GID `2613` inside its container.
> The manager chowns per-server data dirs where it can; otherwise see the CasaOS guide.

## Configuration

Non-secret defaults live in `deploy/.env` (see `deploy/.env.example`):

| Variable | Default | Purpose |
|---|---|---|
| `PUMPKIN_MANAGER_HTTP_PORT` | `8080` | Web UI + API port |
| `PUMPKIN_MANAGER_DATA_DIR` | `/data/servers` | Per-server world/config data |
| `PUMPKIN_MANAGER_BACKUP_DIR` | `/data/backups` | Snapshot archives |
| `PUMPKIN_MANAGER_DATABASE_URL` | `sqlite:/data/…` | Manager database |
| `PUMPKIN_MANAGER_LOG_LEVEL` | `info` | Structured JSON log level |

Secrets are created at first-run setup (owner password) — never shipped or committed.

---

## API overview

All routes live under `/api/v1` and speak JSON with stable `error_code`s. Highlights:

| Area | Endpoints |
|---|---|
| Auth | `setup-status`, `setup`, `login`, `logout`, `me`, `change-password` |
| Servers | CRUD, `start`/`stop`/`restart` (202 + task), `status`, `logs`, `capabilities` |
| Config | `config` GET/PUT (revision-checked), `validate`, `revisions`, `revert` |
| Files | `files` list/upload/download/mkdir/rename/delete, `import`, `export` |
| Backups | `backups` create/list/download/delete/restore, `backup-policies` + retention preview |
| Metrics | `metrics/summary`, `servers/:id/metrics`, `alerts` |
| Admin | `users`, `audit`, `scheduled-tasks`, `admin/diagnostics`, `admin/database/backup|restore`, server `update`/`rollback` |

Long operations return `202 Accepted` with a `task_id` to poll; state conflicts are `409`,
bad config `422`, unauthenticated `401`, unauthorized `403`.

---

## Development

Prerequisites: the Rust toolchain pinned in `rust-toolchain.toml`, Node 24 (`.nvmrc`),
Docker Engine.

```sh
PUMPKIN_INSECURE_COOKIES=1 \
PUMPKIN_MANAGER_DATA_DIR=./dev-data/servers \
PUMPKIN_MANAGER_BACKUP_DIR=./dev-data/backups \
PUMPKIN_MANAGER_DATABASE_URL='sqlite:./dev.db?mode=rwc' \
cargo run -p pumpkin-manager-backend        # API on :8080
npm --prefix apps/web run dev               # UI dev server (proxies /api)
```

Verify (offline after first fetch — formatting, strict lints, backend suite, frontend build):

```sh
make test
```

End-to-end (needs Docker + Playwright Chromium, ~1 min, fully self-cleaning):

```sh
npm --prefix apps/web run test:e2e         # setup → login → create → online → stop → delete
```

```
pumpkin-manager/
├── apps/web/          # React 19 + TS + TanStack Query UI
├── crates/backend/    # Axum API: auth, runtime, config, files, backups, metrics
├── migrations/        # Numbered SQLite migrations (never edit released ones)
├── deploy/            # Single-image Dockerfile, Compose files, .env.example
├── docs/              # phase logs, plans, guides, compatibility, and release evidence
├── release/           # versioned source artifact, manifest, and SHA-256 checksum
└── pumpkin-manager-implementation-plan.md   # the full engineering spec
```

### Docs index

- Spec: [`pumpkin-manager-implementation-plan.md`](pumpkin-manager-implementation-plan.md)
- Build logs: [`docs/phase2.md`](docs/phase2.md) … [`docs/phase8.md`](docs/phase8.md) (+ [`docs/superpowers/plans/`](docs/superpowers/plans/) for sub-plans)
- Ops: [`docs/backup-restore.md`](docs/backup-restore.md), [`docs/operations.md`](docs/operations.md), [`docs/security.md`](docs/security.md), [`docs/development.md`](docs/development.md), [`docs/installation-casaos.md`](docs/installation-casaos.md)
- Release: [`docs/architecture.md`](docs/architecture.md), [`docs/api.md`](docs/api.md), [`docs/upgrade.md`](docs/upgrade.md), [`docs/performance.md`](docs/performance.md), [`docs/known-limitations.md`](docs/known-limitations.md), [`docs/compatibility/pumpkin-manager.md`](docs/compatibility/pumpkin-manager.md)

---

## License

This project is licensed under the **GNU Affero General Public License v3.0 or later**.
See [`LICENSE`](LICENSE) for the full license text.
