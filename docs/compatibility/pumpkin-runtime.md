# Pumpkin Runtime Compatibility — Phase 0 recon (2026-10-15)

Source: https://github.com/Pumpkin-MC/Pumpkin, docs https://docs.pumpkinmc.org/
Tested image: `ghcr.io/pumpkin-mc/pumpkin:master`
Digest: `sha256:675adcf4f351ed8656385346bf514584b0bb34826f06b553cd16094196d79099`
Arch tested: `linux/amd64`. ARM64 claimed by Dockerfile `TARGETARCH` case, not tested here.
Reported version at boot: `Pumpkin 0.2.0+26.3-26.51`, Java proto 777, Bedrock 2193.

## Verified facts

- Startup command: `ENTRYPOINT ["pumpkin"]`, `WORKDIR /pumpkin`, user `pumpkin:pumpkin (2613:2613)`.
- Persistent mount: `/pumpkin`. Generates `pumpkin.toml` (233 lines), `world/`, `data/`, `logs/latest.log`, `plugins/`, `nethernet-key.der` on first boot.
- Ports observed: `25565/tcp` Java, `19132/udp` Bedrock (+19133 IPv6, 7551 LAN discovery in logs).
- Logs: stdout/stderr usable (JSON-ish console). File log at `logs/latest.log` with rotation (`*.log.gz`).
- Shutdown: `docker stop -t 30` → graceful, ~1s, exit 0, "Saving chunks… Completed save! The server has stopped."
- Healthcheck in image: `nc -z 127.0.0.1 25565` — TCP only, NOT game-readiness.
- Minecraft status ping (handshake+status on 25565): **verified** — returned version/players/description/favicon.
- Recreate with same `/pumpkin` mount: world + config persist. Restart works.
- Default `stdin_open/tty`: image has `OpenStdin=false`. Upstream compose sets `stdin_open:true tty:true`. Manager must set these for console input.
- `use_console=true use_tty=true` defaults in generated `pumpkin.toml`; without TTY log warns "falling back to simple logger".
- Telemetry `enabled=true` by default — surface opt-out in UI.

## Capability matrix (for `pumpkin-adapter` profile)

| capability | status | evidence |
|---|---|---|
| stdout_logs | verified | `docker logs` captured boot + save lines |
| minecraft_status_ping | verified | raw socket ping returned JSON |
| toml_config | verified | 233-line generated file, round-trips |
| graceful_shutdown | verified-with-limitations | SIGTERM ~1s OK; signal name/timeout per-version TBD |
| query_protocol | unknown | config present `enabled=false`, not exercised |
| remote_console / stdin | unknown | `use_console` exists but no attach test; default container has stdin closed |
| rcon | unknown | documented at /config/rcon but **absent** from generated config; live file has `[networking.management]` instead — do not advertise |
| player_count | verified-with-limitations | present in status-ping JSON (`online:0`), not via separate API |
| health_endpoint | not-supported | only TCP `nc` check, no HTTP |
| invalid_config_behavior | unknown | trailing-garbage append still booted — needs rigorous retest |

## Implications for manager

1. Pin image by digest, not `:master`. Surface configured reference + digest per server.
2. Readiness = MC status ping success, not container `running` or `healthy`.
3. Child spec must set `stdin_open:true tty:true`, `stop_grace ~35s`, explicit port pair (Java+Bedrock), `/pumpkin` volume, labels `io.pumpkin-manager.*`.
4. UID 2613 owns files in volume — manager file-service runs as host root or same UID mapping, else `Permission denied`. Prefer named volumes by default; document `chown -R 2613:2613` for bind mounts (CasaOS guide).
5. Set `color=false` or strip ANSI when ingesting logs; keep `latest.log` as fallback source.
6. Do not add RCON UI until a build emits `[networking.rcon]` and it is exercised with password auth + command round-trip.
