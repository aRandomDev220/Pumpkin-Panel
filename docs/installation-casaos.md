# CasaOS install

1. Install Docker + CasaOS, ensure `docker compose version` works.
2. Copy this repo to the host, `cp deploy/.env.example deploy/.env`, set
   `PUMPKIN_MANAGER_HTTP_PORT` (default 8080), and choose an absolute
   `PUMPKIN_MANAGER_DATA_ROOT` that is available on the CasaOS host (the
   default is `/data`). The same
   path is mounted into the manager and child containers because child
   containers are created through the host Docker socket. The Compose example uses
   `PUMPKIN_INSECURE_COOKIES=1` because CasaOS commonly serves the panel over
   plain HTTP on a trusted LAN. Set it to `0` only when HTTPS is configured.
3. `docker build -f deploy/Dockerfile -t pumpkin-manager:0.1.0 .`.
4. Create the shared data root before starting (for the default root):
   `sudo mkdir -p /data/servers /data/backups`. If you set a custom
   `PUMPKIN_MANAGER_DATA_ROOT`, create its `servers` and `backups`
   subdirectories instead.
   The entrypoint owns the root as `app:app` (100:101), and child containers
   use the same non-root UID/GID via `PUMPKIN_RUNTIME_USER`.
5. Start: `docker compose -f deploy/docker-compose.yml up -d`.
6. Open `http://<host>:8080`, complete first-run owner setup, then sign in.
7. Forward only the game ports you allocate (e.g. 25565/tcp, 19132/udp)
   in router/firewall — never the Docker socket.

See `docs/backup-restore.md`, `docs/upgrade.md`, and `docs/operations.md` for
backup, update, restore, and recovery procedures.
