# syntax=docker/dockerfile:1.7
# Vyrtel production image: one static-ish binary on a minimal runtime.
#
#   docker build -t vyrtel .
#   docker run -p 8080:8080 -v ./vyrtel-data:/data vyrtel

ARG RUST_VERSION=1.96
ARG NODE_VERSION=22

# ---- web UI ----------------------------------------------------------------
FROM node:${NODE_VERSION}-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---- server ----------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
COPY tools ./tools
COPY tests ./tests
COPY --from=web /src/web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p server \
    && cp target/release/vyrtel /vyrtel \
    && mkdir -p /data

# ---- runtime ---------------------------------------------------------------
# distroless/cc: glibc + libgcc only, no shell, runs as an unprivileged user.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /vyrtel /vyrtel
# Pre-create the data directory owned by the runtime user so named volumes
# inherit the right ownership. Bind mounts: see docs/configuration.md.
COPY --from=build --chown=nonroot:nonroot /data /data
ENV VYRTEL_STORAGE_PATH=/data \
    VYRTEL_SERVER_BIND=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080
USER nonroot:nonroot
# Uses the binary's own health probe (no curl in the image).
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["/vyrtel", "healthcheck"]
ENTRYPOINT ["/vyrtel"]
