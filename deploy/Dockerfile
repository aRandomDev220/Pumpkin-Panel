# Single-image manager: Rust API serves Vite build + /api/v1.
# Phase 1 scaffold. Child Pumpkin containers are created at runtime via /var/run/docker.sock.

FROM node:24-alpine@sha256:44fce696f38ba8c987c4db89954051000bbd05ffd8d8659ba15ed4a8602e937f AS webbuild
WORKDIR /web
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci
COPY apps/web/ ./
RUN npm run build

FROM rust:1.90-alpine@sha256:b4b54b176a74db7e5c68fdfe6029be39a02ccbcfe72b6e5a3e18e2c61b57ae26 AS apibuild
RUN apk add --no-cache musl-dev sqlite-dev pkgconfig
WORKDIR /app
COPY Cargo.toml Cargo.lock* rust-toolchain.toml ./
COPY crates/ ./crates/
COPY migrations/ ./migrations/
RUN cargo build --release -p pumpkin-manager-backend

FROM alpine:3.21@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507
RUN apk add --no-cache ca-certificates sqlite-libs su-exec \
 && addgroup -S app && adduser -S app -G app
WORKDIR /app
COPY --from=apibuild /app/target/release/pumpkin-manager /usr/local/bin/pumpkin-manager
COPY --from=webbuild /web/dist /app/web
COPY deploy/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh \
 && mkdir -p /data/servers /data/backups \
 && chown -R app:app /app /data
# NOTE: /var/run/docker.sock mount makes this container privileged-equivalent.
# Backend only — never expose the socket to the browser.
ENV PUMPKIN_MANAGER_HTTP_PORT=8080 \
    PUMPKIN_MANAGER_DATA_DIR=/data/servers \
    PUMPKIN_MANAGER_BACKUP_DIR=/data/backups \
    PUMPKIN_MANAGER_LOG_LEVEL=info
EXPOSE 8080
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD wget -qO- http://127.0.0.1:8080/api/v1/health/live || exit 1
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
