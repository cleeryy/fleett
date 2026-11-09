# syntax=docker/dockerfile:1
#
# fleett — control-plane image.
#
# The final image carries the single binary (frontend embedded, so no `web/dist`
# and no working directory to arrange), the Docker CLI and the Compose plugin:
# `FLEETT_DRIVER=compose` shells out to `docker compose`, so the image is only
# useful with both, and with `/var/run/docker.sock` mounted.
#
#   docker build -t fleett:test .
#
# See docs/PACKAGING.md for the run command (socket mount, socket GID, data
# root) and for the Debian package.

# --- stage 1: web console ---------------------------------------------------
FROM node:22-alpine AS web

RUN npm install --global pnpm@9

WORKDIR /src/web

# Dependency layer first: a source-only change keeps the store cache.
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile

COPY web/ ./
RUN pnpm build

# --- stage 2: control plane -------------------------------------------------
FROM rust:1-slim-trixie AS backend

WORKDIR /src

# cmake: libgit2 is vendored and built from source by `git2` (instance history),
# so the build stage needs a C toolchain; the runtime image stays untouched.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*

COPY backend/Cargo.toml backend/Cargo.lock ./backend/
COPY backend/src ./backend/src
# `include_str!("../../../openapi.json")` embeds the static contract.
COPY openapi.json ./openapi.json

# rust-embed reads `../web/dist` relative to the backend manifest.
COPY --from=web /src/web/dist ./web/dist

RUN cargo build --manifest-path backend/Cargo.toml --release --locked --features embed-web

# --- stage 3: runtime -------------------------------------------------------
FROM debian:trixie-slim AS runtime

# ca-certificates: outbound TLS (gateway probes, OIDC discovery).
# curl: HEALTHCHECK below.
# docker-cli + docker-compose: the compose driver runs `docker compose`.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      docker-cli \
      docker-compose \
 && rm -rf /var/lib/apt/lists/*

# Same UID/GID as the agent containers (HERMES_UID/HERMES_GID) so the bind
# mounts under FLEETT_ROOT stay readable on both sides.
RUN groupadd --system --gid 10000 fleett \
 && useradd --system --uid 10000 --gid 10000 --home-dir /data \
      --shell /usr/sbin/nologin fleett \
 && install -d -m 0700 -o 10000 -g 10000 /data

COPY --from=backend /src/backend/target/release/fleett /usr/local/bin/fleett

ENV FLEETT_ROOT=/data

VOLUME /data
EXPOSE 8090

# The control plane answers as soon as it listens, even when the Docker daemon
# is unreachable (the payload then reports "docker": {"ok": false}).
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8090/api/health || exit 1

USER 10000:10000
ENTRYPOINT ["fleett"]
