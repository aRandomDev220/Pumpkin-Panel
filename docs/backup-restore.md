# Manager database backup, restore & recovery

The manager database (`pumpkin-manager.db`, configured via
`database_url`, default `sqlite:/data/pumpkin-manager.db`) holds users,
servers, tasks, audit events, and scheduling state. This page covers
backing it up, restoring it, and recovering when things go wrong.

## Download a backup

As an **owner** (`settings.manage` permission):

```
GET /api/v1/admin/database/backup
```

Returns a consistent snapshot as an attachment download named
`pumpkin-manager-<schema>-<ts>.db`. The snapshot is taken with
`VACUUM INTO`, so it is consistent even while the manager is running
(no need to stop anything first). Store the file somewhere safe —
losing it means rebuilding users and server inventory by hand.

## Restore (staged + restart)

Restores are **staged, never live-swapped**:

1. Upload a previously downloaded backup file (multipart file field):
   ```
   POST /api/v1/admin/database/restore
   ```
2. The manager validates the file **before touching anything**:
   - it must be a real SQLite file (`SQLite format 3` magic),
   - its `user_version` must be within the range this manager
     understands (a file from an unknown *newer* manager is rejected),
   - it must contain the required `users` and `servers` tables.
3. On success the manager copies the **current** database aside as
   `<live>.pre-restore-<ts>` (a safety net) and writes the upload to
   `<live>.pending-restore`, then answers:
   ```json
   { "staged": true, "restart_required": true }
   ```
4. **Restart the manager.** On boot it swaps the staged file into
   place and comes up on the restored data. Until the restart,
   everything keeps running on the current database.

If validation fails (HTTP 422), the live database is untouched and no
pending file is staged — fix the file (see below) and upload again.

## Pre-restore copies

Every staged restore leaves a `<live>.pre-restore-<ts>` copy next to
the live database. If a restore makes things worse, stop the manager,
move the pre-restore copy back over the live file, and start again.
Clean up old pre-restore copies once you are confident in the current
state.

## When locked or corrupt

- **Database locked / manager won't start:** make sure only one
  manager process is using the file, then restore the latest backup
  via the staged flow above (or by hand while stopped).
- **Corrupt file / failed validation:** the 422 response tells you
  why (not a SQLite file vs. unsupported schema). Re-download from a
  known-good backup; never hand-edit the binary file.
- **Lost all owners / locked out:** restoring a backup that contains
  a working owner account recovers access (the last-owner guard only
  blocks in-app deletes, not a file-level restore).

## Server world data

Server *world* data (blocks, players, `pumpkin.toml`) lives in each
server's data directory, **not** in the manager database — restoring
the manager DB does not roll back worlds, and rolling back a world
backup does not touch the manager DB. The two are versioned
independently; after a manager-DB restore, verify the server list
matches the data directories on disk and reconcile any drift.
