# Development

## Prereqs

- Rust stable (see `rust-toolchain.toml`), Node 24, Docker 24+.

## Backend

```sh
cp deploy/.env.example deploy/.env
# Local development must use writable project paths instead of the container's
# /data paths. The backend creates these directories on startup.
PUMPKIN_INSECURE_COOKIES=1 \
PUMPKIN_MANAGER_DATA_DIR=./dev-data/servers \
PUMPKIN_MANAGER_BACKUP_DIR=./dev-data/backups \
PUMPKIN_MANAGER_DATABASE_URL='sqlite:./dev.db?mode=rwc' \
cargo run -p pumpkin-manager-backend
curl localhost:8080/api/v1/health/live
```

Checks: `cargo fmt --check`, `cargo clippy -p pumpkin-manager-backend -- -D warnings`,
`cargo test -p pumpkin-manager-backend`.

## Frontend

```sh
cd apps/web && npm install && npm run dev  # proxies /api to :8080
npm run build
```

## Docker

```sh
docker build -f deploy/Dockerfile -t pumpkin-manager:0.1.0 .
```

Note: this host needs the `docker compose` plugin for `docker compose` commands;
`docker-compose` standalone is not installed here.
