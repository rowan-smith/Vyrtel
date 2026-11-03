# Running in Docker

```bash
docker run -d --name vyrtel -p 8080:8080 \
  -v vyrtel-data:/data \
  -e VYRTEL_AUTH_ADMIN_PASSWORD=change-me \
  ghcr.io/rowan-smith/vyrtel:0.1
```

The image is `ghcr.io/rowan-smith/vyrtel` for `linux/amd64` and `linux/arm64`. It runs as the
non-root user `65532` and keeps all data in `/data`.

## Tags

| Tag | Follows |
|---|---|
| `0.1.2` | Exactly that release |
| `0.1` | The newest `0.1.x` release |
| `latest` | The newest stable release |

Pin `0.1` or an exact version in production so that recreating the container never upgrades
it by surprise. Prereleases are only tagged with their exact version.

## Keep your data

Use a **named volume** (`-v vyrtel-data:/data`). The data lives in the volume, so you can stop,
remove and recreate the container, for example to change a setting or upgrade, without losing
anything. Without `-v`, every new container starts empty.

For a **bind mount** on Linux, give the directory to uid 65532 first:

```bash
mkdir -p vyrtel-data && sudo chown -R 65532:65532 vyrtel-data
docker run -d --name vyrtel -p 8080:8080 -v ./vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:0.1
```

If the directory isn't writable, Vyrtel stops with a hint that shows the `chown` to run. On
SELinux hosts, add `:Z` to the mount. Docker Desktop on macOS and Windows needs no `chown`.

## Docker Compose

```yaml
services:
  vyrtel:
    image: ghcr.io/rowan-smith/vyrtel:0.1
    ports:
      - "8080:8080"
    volumes:
      - vyrtel-data:/data
    environment:
      VYRTEL_AUTH_ADMIN_PASSWORD: change-me
      VYRTEL_RETENTION_LOGS: 14d
    restart: unless-stopped
    stop_grace_period: 15s

volumes:
  vyrtel-data:
```

`stop_grace_period` is longer than Vyrtel's 8-second shutdown drain, so accepted events are
written before Docker stops the container.

## Health checks

The image has a built-in `HEALTHCHECK` (`vyrtel healthcheck`). Load balancers can use
`GET /ready`, which returns `503` as soon as Vyrtel starts draining for a shutdown.

More detail, including the exact error messages, is in
[configuration: Docker and bind mounts](../configuration.md#docker-and-bind-mounts).
