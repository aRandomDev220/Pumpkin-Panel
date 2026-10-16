# API reference

All application routes use `/api/v1`. JSON errors have a stable `error_code`
and safe `message`; long operations return a persisted `task_id`.

## Authentication

| Method | Route | Purpose |
|---|---|---|
| GET | `/auth/setup-status` | Check whether first-owner setup is needed. |
| POST | `/auth/setup` | Create the first owner, once. |
| POST | `/auth/login` | Start a session. |
| POST | `/auth/logout` | Revoke the current session. |
| GET | `/auth/me` | Return the current user. |
| POST | `/auth/change-password` | Change the current password and revoke sessions. |

Browser sessions use an HttpOnly `pm_session` cookie plus a readable `pm_csrf`
cookie. Unsafe cookie-authenticated requests must send `X-CSRF-Token`. Bearer
tokens remain supported for non-browser API clients.

## Servers and tasks

| Method | Route | Purpose |
|---|---|---|
| GET/POST | `/servers` | List or create servers. |
| GET/PATCH/DELETE | `/servers/:id` | Read, update, or delete a server. |
| POST | `/servers/:id/start`, `/stop`, `/restart` | Queue lifecycle work. |
| GET | `/servers/:id/status`, `/logs`, `/tasks` | Read runtime state and history. |
| GET | `/tasks/:id` | Read task progress/result. |
| POST | `/tasks/:id/cancel` | Request cooperative cancellation. |
| POST | `/servers/:id/update/preview` | Validate an image update without mutation. |
| POST | `/servers/:id/update` | Queue a protected image update. |
| POST | `/servers/:id/rollback` | Queue image rollback with data-format warning. |

## Configuration, files, backups, and monitoring

The API provides version-aware config read/validate/save/revisions/revert,
sandboxed file CRUD/upload/import/export/download, verified backup and restore
tasks, backup policies, metrics history/summary, alerts, capabilities, and
scheduled tasks. See route registration in `crates/backend/src/main.rs` for the
complete authoritative list.

## Administration

| Method | Route | Permission |
|---|---|---|
| GET/POST | `/users` | `users.manage` |
| DELETE | `/users/:id` | `users.manage` |
| GET | `/audit` | `audit.read` |
| GET | `/admin/diagnostics` | `settings.manage` |
| GET | `/admin/database/backup` | `settings.manage` |
| POST | `/admin/database/restore` | `settings.manage` |

Manager database restore is staged and applied on the next boot; it never swaps
the live database in an HTTP handler.
