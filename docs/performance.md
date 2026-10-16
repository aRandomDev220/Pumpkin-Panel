# Phase 8 performance baseline

Date: 2026-10-16  
Workload: 100 sequential `GET /api/v1/health/live` requests against a local
release-mode-compatible `cargo run` process, SQLite database, no active game
servers, Docker available.

## Result

| Metric | Result |
|---|---:|
| Requests | 100 |
| Minimum | 0.595 ms |
| Median (p50) | 1.198 ms |
| p95 | 1.748 ms |
| Maximum | 40.919 ms |

This is a process-health baseline, not a capacity claim. It excludes Docker
image pulls, Minecraft protocol ping latency, archive throughput, and concurrent
multi-user load. Future changes should rerun the same workload and record the
machine, commit, build profile, and database size alongside the result.
