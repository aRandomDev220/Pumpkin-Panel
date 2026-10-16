# Pumpkin Manager

Self-hosted control plane for [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)
Minecraft servers. Single Docker image (Rust Axum API + embedded Vite UI),
one child container per game server, SQLite, Docker Compose + CasaOS friendly.

Status: **Phases 1–7 complete** — auth + RBAC, server lifecycle with
readiness-gated status, version-aware config with revisions, sandboxed
files, verified backups + restore, metrics/alerts/scheduler, image updates
with rollback, user admin, audit trail, and manager DB recovery. See
`docs/phase2.md` … `docs/phase7.md` for per-phase logs, or
`pumpkin-manager-implementation-plan.md` for the full spec.

## Prerequisites

- Rustup with the repository-pinned toolchain from `rust-toolchain.toml`.
- Node.js `24.22.0` from `.nvmrc`.
- Docker Engine and Docker Compose for packaged deployment and E2E tests.

## Setup

```sh
cp deploy/.env.example deploy/.env
cargo build --workspace
npm ci --prefix apps/web
```

## Quick start (dev)

```sh
cp deploy/.env.example deploy/.env
cargo run -p pumpkin-manager-backend
# API: http://127.0.0.1:8080/api/v1/health/live
npm --prefix apps/web run dev
```

## Test

The default test command is offline after setup and runs formatting, strict
linting, the backend test suite, and the frontend production build:

```sh
make test
```

The browser lifecycle suite requires a running Docker daemon and a local
Playwright browser installation:

```sh
npm --prefix apps/web run test:e2e
```

## Deploy with Docker Compose

```sh
cp deploy/.env.example deploy/.env
docker compose -f deploy/docker-compose.yml up -d --build
```

Open `http://<host>:8080`. See `docs/compatibility/pumpkin-runtime.md`
for the pinned Pumpkin image + capability matrix, and `docs/security.md`
for the Docker socket trust boundary.
