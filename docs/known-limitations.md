# Known limitations

- Docker socket access is host-root equivalent. Restrict manager access and do
  not expose the Docker Engine socket over TCP.
- The first release is single-node and SQLite-backed; it is not a clustered
  control plane.
- The application does not terminate TLS. The bundled Compose/CasaOS setup uses
  `PUMPKIN_INSECURE_COOKIES=1` for trusted-LAN HTTP; use a trusted HTTPS reverse
  proxy and set it to `0` before exposing the panel remotely.
- Pumpkin image rollback restores the old image/container but cannot reverse a
  newer build's one-way world-data migration.
- Manager database restore is staged and requires a restart.
- The verified Pumpkin compatibility target is Linux/amd64 and the digest in
  `docs/compatibility/pumpkin-runtime.md`; ARM64 is not yet live-tested.
- RCON, arbitrary console commands, player management, TPS, plugin install, and
  hot reload are not advertised because they are not verified capabilities.
- The tested Pumpkin image exposes a TCP Minecraft health signal rather than an
  HTTP health endpoint; readiness uses the Minecraft status protocol.
- Mutable Pumpkin tags are allowlisted but should not be used for production;
  pin a digest and review the update preview warnings.
- Live Docker/Pumpkin integration tests are environment-dependent and are
  ignored when Docker or the test Pumpkin endpoint is unavailable.
