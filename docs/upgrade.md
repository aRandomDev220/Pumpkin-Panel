# Upgrade and rollback guide

## Before upgrading

1. Read the release notes and compatibility matrix.
2. Download a manager database backup from **Settings → Manager database**.
3. Create a completed verified backup for every important Pumpkin server.
4. Record the current image digest, manager version, and database schema.
5. Confirm free disk space for the image, backup, and migration work.

## Docker Compose upgrade

```sh
git fetch --tags
git checkout <release-tag>
docker compose -f deploy/docker-compose.yml pull
docker compose -f deploy/docker-compose.yml up -d --build
curl http://127.0.0.1:8080/api/v1/health/ready
```

SQLite migrations run during startup and are append-only. Do not edit or remove
an applied migration. Confirm the UI loads, then check server readiness and
recent task/audit history.

## Rollback

If the manager release itself is faulty, stop it and redeploy the previous
manager image/tag. Restore the manager DB only if the newer release changed the
schema and the previous release cannot read it; use the staged restore flow and
restart. Keep the `.pre-restore-*` copy until the older release is confirmed.

Rolling back a Pumpkin image does **not** roll back world data-format changes.
Use a verified pre-update world backup when the newer Pumpkin build migrated
world data.

## Upgrade evidence

The release gate requires a fresh install, migration from the previous schema,
manager DB backup/restore, server-world backup/restore, and recovery after an
interrupted task. Record the exact release tag, image digests, commands, and
health results in `docs/phase8.md`.
