# Operations runbook

## Health and logs

```sh
curl http://127.0.0.1:8080/api/v1/health/live
curl http://127.0.0.1:8080/api/v1/health/ready
docker compose -f deploy/docker-compose.yml logs -f pumpkin-manager
```

`live` means the process is responding. `ready` includes database and Docker
availability. A game server is not shown online until Minecraft status ping
passes.

## Start and stop

Use the UI or the server lifecycle endpoints. Do not remove a managed Pumpkin
container manually while the manager is running; the next reconciliation will
report the observed mismatch. Repeated start/stop requests are idempotent and
conflicting operations return `SERVER_BUSY`.

## Backups

Create and verify a server backup before image updates or risky world changes.
Keep at least one verified backup. The manager database backup is separate from
server-world backups; use `docs/backup-restore.md` for manager recovery.

## Common failures

- `RUNTIME_UNAVAILABLE`: Docker is unavailable or the socket is not mounted.
- `SERVER_BUSY`: wait for the current task, then inspect `/tasks/:id`.
- `UPDATE_NO_BACKUP`: create a completed verified backup before updating.
- `PATH_OUTSIDE_SERVER_ROOT`: use a path relative to the server data root.
- `CAPABILITY_UNAVAILABLE`: the selected Pumpkin build has not verified that
  capability; do not infer support from a config key alone.

## Safe shutdown and recovery

```sh
docker compose -f deploy/docker-compose.yml stop
docker compose -f deploy/docker-compose.yml up -d
```

The manager reconciles interrupted tasks on boot. Never delete `.recovery-*`,
`.pre-restore-*`, or staged restore files until the data has been inspected and
the recovery decision is complete.

## Diagnostics

Owners/admins can download the diagnostics export. It includes version, schema,
runtime reachability, server state, capabilities, recent failure summaries, and
config validation results. It intentionally excludes passwords, session hashes,
tokens, environment dumps, and file contents.
