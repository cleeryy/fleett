#!/usr/bin/env bash
#
# fetch-s6-overlay.sh -- download the three s6-overlay release assets with a
# retry, an optional mirror and a GitHub-API fallback, then verify every file
# with sha256sum -c before it is kept.
#
# Why: on 2026-11-08 every asset served by
# https://github.com/just-containers/s6-overlay/releases/download answered 504
# from the `fra` edge while api.github.com stayed healthy, so the Hermes agent
# image build failed at the s6-overlay step twice in a row. A plain `curl` (or
# Docker `ADD`) has no second source; this helper falls back to the release
# assets API (`Accept: application/octet-stream`) or a configured mirror, and
# never accepts bytes that do not match the pinned checksum.
#
# Usage:
#   S6_OVERLAY_NOARCH_SHA256=<hex> \
#   S6_OVERLAY_ARCH_SHA256=<hex> \
#   S6_OVERLAY_SYMLINKS_SHA256=<hex> \
#   ./docker/fetch-s6-overlay.sh
#
# Required environment:
#   S6_OVERLAY_NOARCH_SHA256       sha256 of s6-overlay-noarch.tar.xz
#   S6_OVERLAY_ARCH_SHA256         sha256 of s6-overlay-<arch>.tar.xz
#   S6_OVERLAY_SYMLINKS_SHA256     sha256 of s6-overlay-symlinks-noarch.tar.xz
#
# Optional environment:
#   S6_OVERLAY_VERSION             default: 3.2.3.0
#   S6_OVERLAY_ARCH                x86_64 or aarch64 (default: TARGETARCH,
#                                  then `uname -m`)
#   S6_OVERLAY_BASE_URL            default: GitHub releases download base.
#                                  Override it to use a mirror or a broken host
#                                  in a test; layout is <base>/v<version>/<asset>.
#   S6_OVERLAY_MIRROR_URL          extra mirror tried after S6_OVERLAY_BASE_URL
#   S6_OVERLAY_API_URL             default: GitHub release API for the repo;
#                                  layout is <api>/tags/v<version> and
#                                  <api>/assets/<id>
#   S6_OVERLAY_DEST                default: /tmp/s6-overlay-fetch
#   GITHUB_TOKEN (or GH_TOKEN)     optional; raises the API rate limit
#
# Fallback order for each asset:
#   1. <S6_OVERLAY_BASE_URL>/v<version>/<asset>   (curl --fail --retry 3)
#   2. <S6_OVERLAY_MIRROR_URL>/v<version>/<asset> (when set and different)
#   3. GitHub API: resolve the asset id from <api>/tags/v<version>, then GET
#      <api>/assets/<id> with Accept: application/octet-stream
#
# A checksum mismatch is fatal: the helper does not try the next source with
# bytes it has already rejected, and it removes the rejected file.
#
# Exit status: 0 when all three assets are downloaded and verified, 1 otherwise.

set -u

readonly SCRIPT_NAME="fetch-s6-overlay"

log() {
  printf '%s: %s\n' "$SCRIPT_NAME" "$*" >&2
}

die() {
  printf '%s: error: %s\n' "$SCRIPT_NAME" "$*" >&2
  exit 1
}

require_sha256() {
  local name="$1" value="$2"
  if [ -z "$value" ]; then
    die "$name is required; pin the s6-overlay checksum in a build ARG"
  fi
  case "$value" in
    *[!0-9a-fA-F]*)
      die "$name is not a hexadecimal sha256: $value"
      ;;
  esac
  if [ "${#value}" -ne 64 ]; then
    die "$name must be 64 hexadecimal characters, got ${#value}: $value"
  fi
}

version="${S6_OVERLAY_VERSION:-3.2.3.0}"
base_url="${S6_OVERLAY_BASE_URL:-https://github.com/just-containers/s6-overlay/releases/download}"
mirror_url="${S6_OVERLAY_MIRROR_URL:-}"
api_url="${S6_OVERLAY_API_URL:-https://api.github.com/repos/just-containers/s6-overlay/releases}"
dest="${S6_OVERLAY_DEST:-/tmp/s6-overlay-fetch}"

base_url="${base_url%/}"
mirror_url="${mirror_url%/}"
api_url="${api_url%/}"

case "$version" in
  "" | *[!0-9A-Za-z._-]*)
    die "S6_OVERLAY_VERSION is empty or invalid: $version"
    ;;
esac
[ -n "$base_url" ] || die "S6_OVERLAY_BASE_URL is empty"
[ -n "$api_url" ] || die "S6_OVERLAY_API_URL is empty"

if [ -z "${S6_OVERLAY_ARCH:-}" ]; then
  case "${TARGETARCH:-}" in
    amd64)
      S6_OVERLAY_ARCH="x86_64"
      ;;
    arm64)
      S6_OVERLAY_ARCH="aarch64"
      ;;
    "")
      case "$(uname -m)" in
        x86_64 | amd64)
          S6_OVERLAY_ARCH="x86_64"
          ;;
        aarch64 | arm64)
          S6_OVERLAY_ARCH="aarch64"
          ;;
        *)
          die "unsupported machine architecture $(uname -m); set S6_OVERLAY_ARCH"
          ;;
      esac
      ;;
    *)
      die "unsupported TARGETARCH=$TARGETARCH; set S6_OVERLAY_ARCH"
      ;;
  esac
fi

case "${S6_OVERLAY_ARCH}" in
  x86_64 | aarch64) ;;
  *) die "unsupported S6_OVERLAY_ARCH=${S6_OVERLAY_ARCH} (expected x86_64 or aarch64)" ;;
esac

noarch_sha="${S6_OVERLAY_NOARCH_SHA256:-}"
arch_sha="${S6_OVERLAY_ARCH_SHA256:-}"
symlinks_sha="${S6_OVERLAY_SYMLINKS_SHA256:-}"

if [ -z "$arch_sha" ]; then
  case "${S6_OVERLAY_ARCH}" in
    x86_64) arch_sha="${S6_OVERLAY_X86_64_SHA256:-}" ;;
    aarch64) arch_sha="${S6_OVERLAY_AARCH64_SHA256:-}" ;;
  esac
fi

require_sha256 S6_OVERLAY_NOARCH_SHA256 "$noarch_sha"
require_sha256 S6_OVERLAY_ARCH_SHA256 "$arch_sha"
require_sha256 S6_OVERLAY_SYMLINKS_SHA256 "$symlinks_sha"

# Optional bearer token for the API (60 requests/hour unauthenticated).
api_auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  api_auth=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
elif [ -n "${GH_TOKEN:-}" ]; then
  api_auth=(-H "Authorization: Bearer ${GH_TOKEN}")
fi

workdir=""
release_json=""

cleanup() {
  if [ -n "${workdir:-}" ] && [ -d "$workdir" ]; then
    rm -rf "$workdir"
  fi
}
trap cleanup EXIT

fetch_url() {
  local url="$1" out="$2"
  curl --fail --silent --show-error --location \
    --retry 3 --retry-delay 2 \
    --connect-timeout 20 --max-time 300 \
    -o "$out" "$url"
}

verify_sha256() {
  local file="$1" expected="$2" label="$3" sumfile actual
  sumfile="$workdir/verify.sha256"
  printf '%s  %s\n' "$expected" "$file" > "$sumfile" ||
    die "cannot write the checksum file $sumfile"

  if ! sha256sum -c "$sumfile" > /dev/null 2>&1; then
    actual="$(sha256sum "$file" | cut -d' ' -f1)"
    die "checksum mismatch for $label: expected $expected, got ${actual:-<unreadable>} -- refusing to continue"
  fi

  log "verified $label ($expected)"
}

resolve_asset_id() {
  local asset="$1" id

  # The release metadata is cached on disk, not in a variable: this function
  # runs inside a command substitution (a subshell), where a global assignment
  # would be lost and every asset would re-fetch the API document.
  release_json="$workdir/release.json"

  if [ ! -s "$release_json" ]; then
    log "api: reading $api_url/tags/v$version"
    if ! curl --fail --silent --show-error --location \
      --retry 3 --retry-delay 2 \
      --connect-timeout 20 --max-time 60 \
      -H 'Accept: application/vnd.github+json' \
      ${api_auth[@]+"${api_auth[@]}"} \
      -o "$release_json" "$api_url/tags/v$version"; then
      log "api: cannot read $api_url/tags/v$version"
      rm -f "$release_json"
      return 1
    fi
  fi

  # GitHub prints one field per line: the asset `id` precedes its `name`.
  id="$(awk -v target="$asset" '
    /"id": [0-9]+,/ {
      id = $2
      sub(/,/, "", id)
    }
    $1 == "\"name\":" {
      name = $2
      gsub(/[",]/, "", name)
      if (name == target) {
        print id
        exit
      }
    }
  ' "$release_json")"

  if [ -z "$id" ]; then
    log "api: asset $asset is not listed in $api_url/tags/v$version"
    return 1
  fi

  printf '%s\n' "$id"
}

api_download() {
  local asset="$1" out="$2" id

  id="$(resolve_asset_id "$asset")" || return 1

  log "api: GET $api_url/assets/$id ($asset)"
  if ! curl --fail --silent --show-error --location \
    --retry 3 --retry-delay 2 \
    --connect-timeout 20 --max-time 300 \
    -H 'Accept: application/octet-stream' \
    ${api_auth[@]+"${api_auth[@]}"} \
    -o "$out" "$api_url/assets/$id"; then
    log "api: download failed for $asset (asset id $id)"
    return 1
  fi
}

download_asset() {
  local asset="$1" expected="$2" out partial url
  local -a urls

  out="$dest/$asset"
  partial="$workdir/$asset.part"

  urls=("$base_url/v$version/$asset")
  if [ -n "$mirror_url" ] && [ "$mirror_url" != "$base_url" ]; then
    urls+=("$mirror_url/v$version/$asset")
  fi

  log "downloading $asset (s6-overlay v$version)"
  for url in "${urls[@]}"; do
    log "http: GET $url"
    if fetch_url "$url" "$partial"; then
      verify_sha256 "$partial" "$expected" "$asset"
      mv "$partial" "$out" || die "cannot move $partial to $out"
      log "saved $out"
      return 0
    fi
    log "http: failed: $url"
    rm -f "$partial"
  done

  log "http: every direct source failed for $asset; trying the GitHub API"
  if api_download "$asset" "$partial"; then
    verify_sha256 "$partial" "$expected" "$asset"
    mv "$partial" "$out" || die "cannot move $partial to $out"
    log "saved $out (API fallback)"
    return 0
  fi

  rm -f "$partial"
  die "could not download $asset from ${urls[*]} or the GitHub API ($api_url)"
}

main() {
  command -v curl > /dev/null 2>&1 || die "curl is required"
  command -v sha256sum > /dev/null 2>&1 || die "sha256sum is required"
  command -v awk > /dev/null 2>&1 || die "awk is required"

  workdir="$(mktemp -d "${TMPDIR:-/tmp}/s6-overlay.XXXXXX")" ||
    die "cannot create a temporary directory"
  mkdir -p "$dest" || die "cannot create the destination directory $dest"

  download_asset "s6-overlay-noarch.tar.xz" "$noarch_sha"
  download_asset "s6-overlay-${S6_OVERLAY_ARCH}.tar.xz" "$arch_sha"
  download_asset "s6-overlay-symlinks-noarch.tar.xz" "$symlinks_sha"

  log "all three assets are verified in $dest"
}

main "$@"
