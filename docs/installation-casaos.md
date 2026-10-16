# CasaOS install

1. Install Docker + CasaOS, ensure `docker compose version` works.
2. Copy this repo to the host, `cp deploy/.env.example deploy/.env`, set
   `PUMPKIN_MANAGER_HTTP_PORT` (default 8080). The Compose example uses
   `PUMPKIN_INSECURE_COOKIES=1` because CasaOS commonly serves the panel over
   plain HTTP on a trusted LAN. Set it to `0` only when HTTPS is configured.
3. `docker build -f deploy/Dockerfile -t pumpkin-manager:0.1.0 .`.
4. Create + own data dirs if using bind mounts instead of the
   `manager-data` volume. Pumpkin children run as UID/GID 2613:
   `mkdir -p data/servers && sudo chown -R 2613:2613 data/servers`.
5. Start: `docker compose -f deploy/docker-compose.yml up -d`.
6. Open `http://<host>:8080`, complete first-run owner setup, then sign in.
7. Forward only the game ports you allocate (e.g. 25565/tcp, 19132/udp)
   in router/firewall — never the Docker socket.

See `docs/backup-restore.md`, `docs/upgrade.md`, and `docs/operations.md` for
backup, update, restore, and recovery procedures.
