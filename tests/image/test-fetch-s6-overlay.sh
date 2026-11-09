#!/usr/bin/env bash
#
# Exercise docker/fetch-s6-overlay.sh against a local HTTP server. No Docker,
# no github.com: the primary path can answer 504, the API path only accepts
# `Accept: application/octet-stream`, and the checksums are fixture values.
#
#   bash tests/image/test-fetch-s6-overlay.sh
#
# Cases:
#   1. primary works                                   -> success
#   2. primary answers 504, API works                  -> success (the incident)
#   3. primary answers 504, mirror works, API broken   -> success
#   4. primary returns 200 with the wrong bytes        -> fatal, no fallback
#   5. every source fails                              -> fatal, clear error
#   6. a pinned checksum is missing                    -> fatal, usage error

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo_root/docker/fetch-s6-overlay.sh"
server_py="$repo_root/tests/image/mock_server.py"

work="$(mktemp -d "${TMPDIR:-/tmp}/s6-overlay-test.XXXXXX")"
server_pid=""

cleanup() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2> /dev/null || true
    wait "$server_pid" 2> /dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

pass() {
  printf 'PASS: %s\n' "$*"
}

sha_of() {
  sha256sum "$1" | cut -d' ' -f1
}

version="3.2.3.0"
assets=(
  "s6-overlay-noarch.tar.xz"
  "s6-overlay-x86_64.tar.xz"
  "s6-overlay-symlinks-noarch.tar.xz"
)

www="$work/www"
release_dir="$www/releases/download/v$version"
mirror_dir="$www/mirror/v$version"
corrupt_dir="$www/corrupt/v$version"
api_dir="$www/api/repos/just-containers/s6-overlay/releases"
asset_dir="$api_dir/assets"

mkdir -p "$release_dir" "$mirror_dir" "$corrupt_dir" "$asset_dir" "$api_dir/tags"

for asset in "${assets[@]}"; do
  printf 'fixture payload: %s\n' "$asset" > "$release_dir/$asset"
  printf 'corrupt payload: %s\n' "$asset" > "$corrupt_dir/$asset"
  cp "$release_dir/$asset" "$mirror_dir/$asset"
done

cp "$release_dir/s6-overlay-noarch.tar.xz" "$asset_dir/101"
cp "$release_dir/s6-overlay-x86_64.tar.xz" "$asset_dir/102"
cp "$release_dir/s6-overlay-symlinks-noarch.tar.xz" "$asset_dir/103"

cat > "$api_dir/tags/v$version" <<'JSON'
{
  "tag_name": "v3.2.3.0",
  "assets": [
    {
      "url": "http://127.0.0.1/assets/101",
      "id": 101,
      "name": "s6-overlay-noarch.tar.xz",
      "browser_download_url": "http://127.0.0.1/releases/download/v3.2.3.0/s6-overlay-noarch.tar.xz"
    },
    {
      "url": "http://127.0.0.1/assets/102",
      "id": 102,
      "name": "s6-overlay-x86_64.tar.xz",
      "browser_download_url": "http://127.0.0.1/releases/download/v3.2.3.0/s6-overlay-x86_64.tar.xz"
    },
    {
      "url": "http://127.0.0.1/assets/103",
      "id": 103,
      "name": "s6-overlay-symlinks-noarch.tar.xz",
      "browser_download_url": "http://127.0.0.1/releases/download/v3.2.3.0/s6-overlay-symlinks-noarch.tar.xz"
    }
  ]
}
JSON

noarch_sha="$(sha_of "$release_dir/s6-overlay-noarch.tar.xz")"
arch_sha="$(sha_of "$release_dir/s6-overlay-x86_64.tar.xz")"
symlinks_sha="$(sha_of "$release_dir/s6-overlay-symlinks-noarch.tar.xz")"

python3 "$server_py" "$www" "$work/port" &
server_pid=$!

for _ in $(seq 1 100); do
  if [ -s "$work/port" ]; then
    break
  fi
  sleep 0.05
done
[ -s "$work/port" ] || fail "the mock server did not start"
port="$(cat "$work/port")"
url="http://127.0.0.1:$port"
api="$url/api/repos/just-containers/s6-overlay/releases"

run_helper() {
  local dest="$1" base="$2" mirror="$3" api_url="$4" noarch="${5-$noarch_sha}"

  env -i \
    PATH="$PATH" \
    S6_OVERLAY_VERSION="$version" \
    S6_OVERLAY_ARCH="x86_64" \
    S6_OVERLAY_BASE_URL="$base" \
    S6_OVERLAY_MIRROR_URL="$mirror" \
    S6_OVERLAY_API_URL="$api_url" \
    S6_OVERLAY_DEST="$dest" \
    S6_OVERLAY_NOARCH_SHA256="$noarch" \
    S6_OVERLAY_ARCH_SHA256="$arch_sha" \
    S6_OVERLAY_SYMLINKS_SHA256="$symlinks_sha" \
    bash "$helper"
}

expect_assets() {
  local dir="$1" asset
  for asset in "${assets[@]}"; do
    [ -f "$dir/$asset" ] || fail "missing $dir/$asset"
  done
}

# --- 1. primary works ------------------------------------------------------
out="$work/out-primary"
if run_helper "$out" "$url/releases/download" "" "$api" > "$work/primary.log" 2>&1; then
  expect_assets "$out"
  pass "primary download and checksum verification"
else
  cat "$work/primary.log" >&2
  fail "primary download failed"
fi

# --- 2. primary 504, API fallback ------------------------------------------
out="$work/out-api"
if run_helper "$out" "$url/broken" "" "$api" > "$work/api.log" 2>&1; then
  expect_assets "$out"
  grep -q 'trying the GitHub API' "$work/api.log" ||
    fail "the API fallback was not attempted"
  grep -q 'API fallback' "$work/api.log" ||
    fail "the API fallback did not report success"
  pass "primary 504 -> GitHub API fallback"
else
  cat "$work/api.log" >&2
  fail "the API fallback did not rescue a 504 on the primary path"
fi

# --- 3. primary 504, mirror works ------------------------------------------
out="$work/out-mirror"
api_broken="$url/broken/api/repos/just-containers/s6-overlay/releases"
if run_helper "$out" "$url/broken" "$url/mirror" "$api_broken" > "$work/mirror.log" 2>&1; then
  expect_assets "$out"
  grep -q "$url/mirror/v$version/s6-overlay-noarch.tar.xz" "$work/mirror.log" ||
    fail "the mirror was not used"
  pass "primary 504 -> configured mirror"
else
  cat "$work/mirror.log" >&2
  fail "the mirror did not rescue a 504 on the primary path"
fi

# --- 4. wrong bytes are fatal, never silently replaced ---------------------
out="$work/out-corrupt"
if run_helper "$out" "$url/corrupt" "" "$api" > "$work/corrupt.log" 2>&1; then
  fail "a corrupt primary download was accepted"
fi
grep -q 'checksum mismatch' "$work/corrupt.log" ||
  fail "the corrupt download did not report a checksum mismatch"
if grep -q 'trying the GitHub API' "$work/corrupt.log"; then
  fail "the helper tried another source after rejecting the checksum"
fi
if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then
  fail "the helper kept an asset whose checksum failed"
fi
pass "checksum mismatch is fatal and nothing is kept"

# --- 5. every source fails -------------------------------------------------
if run_helper "$work/out-none" "$url/broken" "" "$api_broken" > "$work/none.log" 2>&1; then
  fail "the helper succeeded with every source broken"
fi
grep -q 'could not download' "$work/none.log" ||
  fail "the all-sources-failed error is not explicit"
pass "all sources broken -> explicit failure"

# --- 6. a missing checksum is a usage error --------------------------------
if run_helper "$work/out-missing" "$url/releases/download" "" "$api" "" > "$work/missing.log" 2>&1; then
  fail "the helper ran without S6_OVERLAY_NOARCH_SHA256"
fi
grep -q 'S6_OVERLAY_NOARCH_SHA256 is required' "$work/missing.log" ||
  fail "the missing-checksum error is not explicit"
pass "missing checksum -> explicit usage error"

printf 'all s6-overlay download tests passed\n'
