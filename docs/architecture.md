# Architecture

Pumpkin Manager is a single-node control plane for one or more Pumpkin Minecraft
servers on the same Docker host.

## Components

```text
Browser ── HTTP/JSON + cookies/CSRF ──> Axum API ──> SQLite
                                          │
                                          ├── file/archive services ──> /data/servers/<server>
                                          ├── backup service ────────> /data/backups
                                          ├── task runner/scheduler
                                          └── narrow Bollard adapter ──> Docker Engine
                                                                        │
                                                                        └── Pumpkin containers
```

- **Axum backend** serves `/api/v1` and the compiled SPA.
- **SQLite** stores users, sessions, server inventory, tasks, audits, backups,
  metrics, schedules, and configuration revisions.
- **Task runner** persists long-running lifecycle, backup, restore, update, and
  rollback operations so the UI can recover after a refresh or restart.
- **Runtime adapter** builds Docker specs from validated domain models. Client
  requests never submit arbitrary Docker JSON.
- **Per-server operation locks** prevent conflicting lifecycle, file, backup,
  restore, and update operations.
- **Readiness** is separate from Docker container existence: a server is online
  only after the container is running and Minecraft status ping succeeds.

## Trust boundaries

1. The browser is untrusted and receives only API responses and static assets.
2. The API is the authorization and validation boundary.
3. The Docker adapter is privileged because the mounted socket is equivalent to
   host control; it is never exposed to the browser.
4. Pumpkin containers are treated as untrusted workloads.
5. Server data and backups are sensitive filesystem state and are kept outside
   the embedded web root.

## Data flow

1. An authenticated request passes authentication, CSRF checks where applicable,
   permission checks, validation, and an audit decision.
2. Short operations update SQLite directly; long operations create a persisted
   task and return `202 {task_id}`.
3. Workers claim tasks atomically, acquire the server operation lock, perform
   the runtime/filesystem work, record progress and audit events, then finish in
   a terminal state.
4. Startup reconciles interrupted tasks and reports interrupted restore copies
   instead of deleting recovery data.

## Deployment boundary

The packaged image contains the API and built frontend. `deploy/docker-compose.yml`
mounts `/var/run/docker.sock` for backend-only child-container management and a
named `/data` volume for persistent state. Put TLS and external access control
in front of the manager; the application intentionally does not terminate TLS.
