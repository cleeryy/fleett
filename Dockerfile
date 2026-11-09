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

# --- stage 3: s6-overlay assets (opt-in, not in the control plane) ----------
#
# The control-plane image below does not run s6-overlay. This stage mirrors the
# s6-overlay install block of the Hermes agent image (cleeryy/hermes-agent),
# where the 2026-11-08 504 from the GitHub `fra` edge broke the build twice in
# a row. It is not part of the default target: build it explicitly to validate
# the download path or to obtain a verified s6 tree for a derivative image.
#
#   docker build --target s6-overlay -t fleett-s6-overlay .
#
# See docs/PACKAGING.md ("s6-overlay downloads") for the build-arg matrix, the
# fallback order and how to reproduce the 504 locally. The download helper is
# docker/fetch-s6-overlay.sh; tests/image/test-fetch-s6-overlay.sh exercises it
# against a local server, so CI never depends on github.com.
FROM debian:trixie-slim AS s6-overlay

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      xz-utils \
 && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
ARG S6_OVERLAY_VERSION=3.2.3.0
ARG S6_OVERLAY_BASE_URL=https://github.com/just-containers/s6-overlay/releases/download
ARG S6_OVERLAY_MIRROR_URL=
ARG S6_OVERLAY_API_URL=https://api.github.com/repos/just-containers/s6-overlay/releases
ARG S6_OVERLAY_NOARCH_SHA256=b720f9d9340efc8bb07528b9743813c836e4b02f8693d90241f047998b4c53cf
ARG S6_OVERLAY_X86_64_SHA256=a93f02882c6ed46b21e7adb5c0add86154f01236c93cd82c7d682722e8840563
ARG S6_OVERLAY_AARCH64_SHA256=0952056ff913482163cc30e35b2e944b507ba1025d78f5becbb89367bf344581
ARG S6_OVERLAY_SYMLINKS_SHA256=a60dc5235de3ecbcf874b9c1f18d73263ab99b289b9329aa950e8729c4789f0e

COPY docker/fetch-s6-overlay.sh /usr/local/bin/fetch-s6-overlay.sh

RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) s6_arch="x86_64"; s6_sha="$S6_OVERLAY_X86_64_SHA256" ;; \
      arm64) s6_arch="aarch64"; s6_sha="$S6_OVERLAY_AARCH64_SHA256" ;; \
      *) echo "error: unsupported TARGETARCH=$TARGETARCH for s6-overlay" >&2; exit 1 ;; \
    esac; \
    S6_OVERLAY_ARCH="$s6_arch" \
    S6_OVERLAY_ARCH_SHA256="$s6_sha" \
    S6_OVERLAY_NOARCH_SHA256="$S6_OVERLAY_NOARCH_SHA256" \
    S6_OVERLAY_SYMLINKS_SHA256="$S6_OVERLAY_SYMLINKS_SHA256" \
    S6_OVERLAY_VERSION="$S6_OVERLAY_VERSION" \
    S6_OVERLAY_BASE_URL="$S6_OVERLAY_BASE_URL" \
    S6_OVERLAY_MIRROR_URL="$S6_OVERLAY_MIRROR_URL" \
    S6_OVERLAY_API_URL="$S6_OVERLAY_API_URL" \
    S6_OVERLAY_DEST=/tmp/s6-overlay-fetch \
      bash /usr/local/bin/fetch-s6-overlay.sh; \
    mkdir -p /s6-overlay-root; \
    tar -C /s6-overlay-root -Jxpf /tmp/s6-overlay-fetch/s6-overlay-noarch.tar.xz; \
    tar -C /s6-overlay-root -Jxpf "/tmp/s6-overlay-fetch/s6-overlay-${s6_arch}.tar.xz"; \
    tar -C /s6-overlay-root -Jxpf /tmp/s6-overlay-fetch/s6-overlay-symlinks-noarch.tar.xz; \
    test -x /s6-overlay-root/init; \
    rm -rf /tmp/s6-overlay-fetch

# --- stage 4: runtime -------------------------------------------------------
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
