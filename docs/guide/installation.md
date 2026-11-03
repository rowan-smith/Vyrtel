# Installation

Every release ships as standalone binaries and a container image. Both include the web UI.

## Binaries

| Platform | File |
|---|---|
| Linux x86_64 | `vyrtel-linux-x86_64` |
| Linux ARM64 | `vyrtel-linux-arm64` |
| macOS (Apple silicon) | `vyrtel-macos-arm64` |
| Windows x64 | `vyrtel-windows-x86_64.exe` |

Download from the [latest release](https://github.com/rowan-smith/Vyrtel/releases/latest), then:

```bash
chmod +x vyrtel-linux-x86_64
./vyrtel-linux-x86_64
```

Data goes to `./data` unless you pass `--data-dir <path>`. Check downloads against
`SHA256SUMS` from the same release:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

## Docker

```bash
docker run -d --name vyrtel -p 8080:8080 -v vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:latest
```

The image is multi-arch (`linux/amd64`, `linux/arm64`), distroless, and runs as a non-root user.
`latest` is the newest stable release. In production, pin a version such as `0.1` instead.
See [Running in Docker](docker.md) for volumes, bind mounts and Compose.

## From source

You need stable Rust, a C compiler and Node.js 22 or later.

```bash
git clone https://github.com/rowan-smith/Vyrtel.git vyrtel && cd vyrtel
cd web && npm ci && npm run build && cd ..
cargo run --release -p server
```

The binary is written to `target/release/vyrtel`.

## Check it is running

| Endpoint | Answers |
|---|---|
| `GET /health` | `200` while the process is up |
| `GET /ready` | `200` while it accepts traffic; `503` once it starts shutting down |
| `vyrtel --version` | The installed version |

## Configure it

Vyrtel runs without any configuration. To change a setting, use a `vyrtel.toml` file,
`VYRTEL_*` environment variables or command-line flags. Common ones:

| Setting | Flag | Environment | Default |
|---|---|---|---|
| Data directory | `--data-dir` | `VYRTEL_STORAGE_PATH` | `./data` |
| Listen address | `--bind` | `VYRTEL_SERVER_BIND` | `0.0.0.0:8080` |
| Memory limit | `--memory-limit` | `VYRTEL_STORAGE_MAX_MEMORY` | `512MB` |
| Admin password | | `VYRTEL_AUTH_ADMIN_PASSWORD` | generated on first start |
| Log retention | | `VYRTEL_RETENTION_LOGS` | `30d` |

Every setting is listed in the [configuration reference](../configuration.md).
