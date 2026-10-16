# Threat model (Phase 1 stub)

Trust boundaries (§5.3 of plan):

1. Browser = untrusted. Authenticated `/api/v1` only.
2. API enforces authZ, validation, rate limits, audit.
3. Runtime adapter = privileged. Only allowlisted image refs, ports, mounts.
4. Docker daemon = host-root equivalent.
5. Server containers = untrusted workloads.
6. Backups = sensitive, capacity-controlled.

Rules enforced in this scaffold:

- `/var/run/docker.sock` is mounted for backend-only Engine API access and is
  accessed only from backend Rust code. The adapter needs read/write access to
  create and manage child containers; it is never proxied to the frontend.
- No arbitrary Docker JSON from clients — container specs are built from
  validated `servers` rows (Phase 2).
- 5 MB request body cap, JSON structured logs without secrets.
- SQLite separate from world dirs (`/data/*.db` vs `/data/servers/*`).
- Session cookies are `HttpOnly`, `SameSite=Lax`, and `Secure` by default;
  `PUMPKIN_INSECURE_COOKIES=1` is an explicit local-HTTP development override.
- Unsafe cookie-authenticated mutations require the readable CSRF cookie to be
  echoed in `X-CSRF-Token`; bearer-token mutations remain supported for API use.
- Reverse-proxy browser origins must be explicitly listed in
  `PUMPKIN_ALLOWED_ORIGINS`; arbitrary `Origin` values are never trusted.

Full review lands in Phase 8.
