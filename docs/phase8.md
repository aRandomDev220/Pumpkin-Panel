# Phase 8 — Release candidate

## Scope

Security review, release documentation, reproducible image build, clean install,
upgrade/recovery evidence, benchmark baseline, compatibility matrix, and release
artifacts.

## Security status

- Critical findings: 0.
- High findings: 0.
- The audit and evidence are recorded in `docs/security.md`.
- CI dependency checks and immutable action references are part of the Phase 8
  release gate.

## Verification ledger

| Check | Result |
|---|---|
| `make test` | Passed: 255 tests, 3 ignored; fmt, Clippy across all targets, backend tests, web build. |
| npm offline audit | Passed: 0 vulnerabilities. |
| Docker image build | Passed with pinned base-image digests. |
| Clean Docker install | Passed: fresh bind mount, live/ready health, first-owner setup, and `/auth/me`. |
| Browser E2E | Passed: 6 Playwright tests covering setup, login, create, online, stop, and delete. |
| Manager DB recovery | Passed in the release image: download, staged multipart restore, restart, ready, setup remains complete. |
| Migration preservation | Covered by `db::tests::migration_0008_preserves_existing_rows`. |
| Backup/restore/recovery | Covered by backend tests and Phase 7 live staged-restore evidence. |
| Update/rollback | Covered by backend tests and Phase 7 live E2E evidence. |
| HTTP baseline | 100 local health requests: p50 1.198 ms, p95 1.748 ms; details in `docs/performance.md`. |

> Phase 8 evidence predates migrations 0010-0011. Re-run the install,
> upgrade, backup/restore, and interrupted-recovery checks before tagging.

## Release checklist

- [x] Run `docker compose config` and a fresh named-volume install.
- [ ] Run the upgrade path from the previous schema/release.
- [ ] Record a real backup, restore, and interrupted-recovery run.
- [x] Record a repeatable performance baseline.
- [x] Build the versioned source artifact and SHA-256 checksum; image digest recorded in `release/pumpkin-manager-0.1.0-manifest.txt`.
- [ ] Review `docs/known-limitations.md` and compatibility evidence.
- [ ] Tag the release and publish the release notes.
