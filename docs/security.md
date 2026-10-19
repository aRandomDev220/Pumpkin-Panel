# Pumpkin Manager security review

Review date: 2026-10-16
Scope: backend API, embedded SPA, Docker boundary, file/archive handling,
authentication, backups, restore, dependencies, and CI.

Trust boundaries (§5.3 of plan):

1. Browser = untrusted. Authenticated `/api/v1` only.
2. API enforces authZ, validation, rate limits, audit.
3. Runtime adapter = privileged. Only allowlisted image refs, ports, mounts.
4. Docker daemon = host-root equivalent.
5. Server containers = untrusted workloads.
6. Backups = sensitive, capacity-controlled.

## Controls verified

- `/var/run/docker.sock` is mounted for backend-only Engine API access and is
  accessed only from backend Rust code. The adapter needs read/write access to
  create and manage child containers; it is never proxied to the frontend.
- No arbitrary Docker JSON from clients — container specs are built from
  validated `servers` rows.
- Request limits are 5 MiB for normal routes, 64 KiB for login/setup, and 1 GiB
  for streamed file/archive uploads. Upload handlers enforce their own running
  byte cap.
- SQLite separate from world dirs (`/data/*.db` vs `/data/servers/*`).
- Session cookies are `HttpOnly` and `SameSite=Lax`; `Secure` is enabled by
  default. `PUMPKIN_INSECURE_COOKIES=1` is an explicit local-HTTP override —
  keep it unset whenever TLS terminates at a reverse proxy, and set
  `PUMPKIN_TRUST_PROXY=1` so `X-Forwarded-Proto` can be trusted.
- Unsafe cookie-authenticated mutations require the readable CSRF cookie to be
  echoed in `X-CSRF-Token`; bearer-token mutations remain supported for API use.
- The API is same-origin only: there is no CORS layer and cross-origin API
  access is not supported. `PUMPKIN_ALLOWED_ORIGINS` only relaxes the
  login/setup same-origin check for explicitly listed origins. Set
  `PUMPKIN_ALLOWED_HOSTS` to pin the public host(s); when unset, Host
  validation is disabled. Set `PUMPKIN_TRUST_PROXY=1` only behind a proxy you
  control so `X-Forwarded-*` is honored.
- Login and setup use IP and username token-bucket limits, dummy Argon2 work for
  unknown users, bounded usernames/passwords, and an atomic one-shot setup flag.
- Security headers cover CSP, frame denial, MIME sniffing, referrer policy,
  permissions policy, and HSTS when secure cookies are enabled.
- File paths and archive entries are canonicalized and checked against the
  server root; traversal, symlink escape, device entries, and archive limits
  are rejected.
- Runtime images are restricted to the Pumpkin allowlist; Docker specs cannot
  express privileged mode, arbitrary mounts, host networking, or capabilities.
- Backup checksums, retention floors, free-space checks, staged manager DB
  restore, and interrupted-restore recovery copies are verified by tests.
- The manager image runs as an unprivileged `app` user after its entrypoint
  prepares the data volume. The Docker socket remains an explicit host-root
  trust boundary.

## Findings

| Severity | Count | Result |
|---|---:|---|
| Critical | 0 | No unresolved critical finding identified. |
| High | 0 | No unresolved high finding identified. |
| Medium | 2 | See residual risks below. |
| Low | 2 | See residual risks below. |

### Residual risks

1. **Medium — Docker socket is host-root equivalent.** A compromised manager
   or authenticated administrator can control the Docker host. Deploy behind a
   trusted reverse proxy, restrict manager access, and do not expose the Docker
   socket over TCP.
2. **Medium — mutable Pumpkin tags remain selectable.** The allowlist prevents
   typosquatted images, while the UI warns about unpinned tags. Production
   operators should use the tested digest in
   `docs/compatibility/pumpkin-runtime.md`.
3. **Low — CSP allows inline styles.** React scripts remain same-origin and
   `unsafe-eval` is not enabled; inline styles are currently required by the
   existing UI styling approach.
 4. **Low — dependency audit exception.** CI runs `cargo audit`, and the local
    npm audit completed with zero high/critical vulnerabilities. The only RustSec
    exception is `RUSTSEC-2023-0071`, confined to SQLx's unused optional MySQL
    dependency metadata; this project activates SQLite only and SQLx has no
    fixed upstream upgrade for the advisory.

## Evidence

- `make test`: 255 passed, 3 ignored; formatting, Clippy across all targets,
  backend tests, and the frontend build all passed.
- `docker build -f deploy/Dockerfile -t pumpkin-manager:0.1.0 .`: passed with
  pinned base-image digests.
- `npm --prefix apps/web audit --audit-level=high --offline`: 0 vulnerabilities.
- Secret-pattern scan of tracked source found no credentials, private keys, or
  production tokens.
- Security regression coverage includes CSRF, cookies, rate limiting, setup
  races, security headers, traversal, archive attacks, Docker error redaction,
  task recovery, backup retention, and staged DB restore validation.
