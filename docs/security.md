# Threat model (Phase 1 stub)

Trust boundaries (§5.3 of plan):

1. Browser = untrusted. Authenticated `/api/v1` only.
2. API enforces authZ, validation, rate limits, audit.
3. Runtime adapter = privileged. Only allowlisted image refs, ports, mounts.
4. Docker daemon = host-root equivalent.
5. Server containers = untrusted workloads.
6. Backups = sensitive, capacity-controlled.

Rules enforced in this scaffold:

- `/var/run/docker.sock` mounted `:ro` into manager container, accessed only
  from backend Rust code. Never proxied to the frontend.
- No arbitrary Docker JSON from clients — container specs are built from
  validated `servers` rows (Phase 2).
- 5 MB request body cap, JSON structured logs without secrets.
- SQLite separate from world dirs (`/data/*.db` vs `/data/servers/*`).
- Cookies `HttpOnly/Secure/SameSite` when auth lands (Phase 2).

Full review lands in Phase 8.
