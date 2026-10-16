# Pumpkin Manager release compatibility matrix

| Component | Supported/tested target | Notes |
|---|---|---|
| Manager | `0.1.0` / current release candidate | Schema migrations 0001–0009. |
| OS | Linux | Docker Engine required. |
| Architecture | `linux/amd64` | ARM64 image build path exists but is not live-tested. |
| Docker | Engine 24+ and Compose plugin | Docker socket must be mounted backend-only. |
| Node (development) | 24.22.0 | Pinned by `.nvmrc`; not required at runtime. |
| Rust (development/build) | 1.99.0 | Pinned by `rust-toolchain.toml`. |
| Pumpkin | `ghcr.io/pumpkin-mc/pumpkin:master` | Tested digest below; mutable tag is not a production recommendation. |
| Pumpkin digest | `sha256:675adcf4f351ed8656385346bf514584b0bb34826f06b553cd16094196d79099` | Linux/amd64, reported Pumpkin `0.2.0+26.3-26.51`. |

Verified manager/runtime capabilities and limitations are listed in
`docs/compatibility/pumpkin-runtime.md`. Any unlisted capability is unknown or
unsupported until exercised against a pinned image.
