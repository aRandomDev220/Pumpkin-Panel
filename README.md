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

## Quick start (dev)

```sh
cp deploy/.env.example deploy/.env
cargo run -p pumpkin-manager-backend
# API: http://127.0.0.1:8080/api/v1/health/live
cd apps/web && npm install && npm run dev
```

## Docker (self-host)

```sh
cp deploy/.env.example deploy/.env
docker build -f deploy/Dockerfile -t pumpkin-manager:0.1.0 .
docker run -d --name pumpkin-manager \
  -p 8080:8080 \
  -v pumpkin-data:/data \
  -v /var/run/docker.sock:/var/run/docker.sock:ro \
  pumpkin-manager:0.1.0
```

Open `http://<host>:8080`. See `docs/compatibility/pumpkin-runtime.md`
for the pinned Pumpkin image + capability matrix, and `docs/security.md`
for the Docker socket trust boundary.
