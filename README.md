# Pumpkin Manager

Self-hosted control plane for [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin)
Minecraft servers. Single Docker image (Rust Axum API + embedded Vite UI),
one child container per game server, SQLite, Docker Compose + CasaOS friendly.

Status: **Phase 1 scaffold** — health endpoints, config, migrations, Compose.
Lifecycle/auth/config/files/backups land in Phases 2-5 per
`pumpkin-manager-implementation-plan.md`.

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
