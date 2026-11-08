#!/usr/bin/env bash
# fleett installer — Linux and macOS
#
#   curl -fsSL https://raw.githubusercontent.com/cleeryy/fleett/master/install.sh | bash
#
# Environment overrides (the legacy HERMES_FLEET_* names are still accepted):
#   FLEETT_VERSION      version to install (default: latest release)
#   FLEETT_INSTALL_DIR  where to put the binary (default: /usr/local/bin or ~/.local/bin)
#   FLEETT_ROOT         data root (default: ~/.fleett, or the legacy ~/.hermes-fleet)
#   FLEETT_BASE_URL     alternative download base (mirrors, tests)
#   FLEETT_REPO         GitHub repository (default: cleeryy/fleett)
#
# The script is idempotent: it replaces the binary and keeps data and secrets.

set -euo pipefail

REPO="${FLEETT_REPO:-${HERMES_FLEET_REPO:-cleeryy/fleett}}"
BIN="fleett"
LEGACY_BIN="hermes-fleet"
VERSION="${FLEETT_VERSION:-${HERMES_FLEET_VERSION:-latest}}"
BASE_URL="${FLEETT_BASE_URL:-${HERMES_FLEET_BASE_URL:-}}"
INSTALL_DIR="${FLEETT_INSTALL_DIR:-${HERMES_FLEET_INSTALL_DIR:-}}"

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

# Adopt the legacy data root when the canonical one does not exist yet, so an
# upgrade never starts from an empty directory.
explicit_root="${FLEETT_ROOT:-${HERMES_FLEET_ROOT:-}}"
if [ -n "$explicit_root" ]; then
  ROOT="$explicit_root"
elif [ -d "$HOME/.fleett" ]; then
  ROOT="$HOME/.fleett"
elif [ -d "$HOME/.hermes-fleet" ]; then
  ROOT="$HOME/.hermes-fleet"
  warn "using the legacy data root $ROOT (rename it to ~/.fleett when convenient)"
else
  ROOT="$HOME/.fleett"
fi

os=$(uname -s | tr '[:upper:]' '[:lower:]')
case "$os" in
  linux|darwin) ;;
  *) die "unsupported OS: $os (linux and darwin only)" ;;
esac

arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) die "unsupported architecture: $arch" ;;
esac
target="${os}-${arch}"

VERSION="${VERSION#v}"

if [ "$VERSION" = "latest" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' \
    | head -n1 || true)
  if [ -z "${VERSION:-}" ]; then
    VERSION=$(curl -fsSI "https://github.com/${REPO}/releases/latest" 2>/dev/null \
      | awk 'tolower($1)=="location:"{print $2}' \
      | sed 's|.*/tag/v\{0,1\}||' | tr -d '\r' | head -n1 || true)
  fi
fi
[ -n "${VERSION:-}" ] || die "could not resolve the latest release (set FLEETT_VERSION)"

asset="${BIN}_${VERSION}_${target}.tar.gz"
base="${BASE_URL:-https://github.com/${REPO}/releases/download/v${VERSION}}"
url="${base%/}/${asset}"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t fleett)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading ${asset}"
curl -fsSL "$url" -o "$tmp/$asset" || die "download failed: $url"

if curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256" 2>/dev/null; then
  expected=$(awk '{print $1}' "$tmp/$asset.sha256" | head -n1)
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
  else
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
  fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for ${asset}"
  say "Checksum OK"
else
  warn "no checksum published, skipping verification"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/$BIN"
if [ ! -f "$src" ]; then
  src=$(find "$tmp" -maxdepth 2 -type f -name "$BIN" | head -n1 || true)
fi
[ -n "${src:-}" ] && [ -f "$src" ] || die "binary not found in ${asset}"

if [ -n "$INSTALL_DIR" ]; then
  dir="$INSTALL_DIR"
elif [ -w /usr/local/bin ] 2>/dev/null; then
  dir=/usr/local/bin
else
  dir="$HOME/.local/bin"
fi
mkdir -p "$dir"
if command -v install >/dev/null 2>&1; then
  install -m 0755 "$src" "$dir/$BIN"
else
  cp "$src" "$dir/$BIN" && chmod 0755 "$dir/$BIN"
fi
say "Installed ${BIN} ${VERSION} -> ${dir}/${BIN}"

# Compatibility: scripts and units written against the old name keep working.
if ln -sf "$BIN" "$dir/$LEGACY_BIN" 2>/dev/null; then
  say "Compatibility symlink: ${dir}/${LEGACY_BIN} -> ${BIN}"
else
  warn "could not create the ${LEGACY_BIN} compatibility symlink in ${dir}"
fi

mkdir -p "$ROOT/instances" "$ROOT/shares"
chmod 700 "$ROOT" "$ROOT/instances" "$ROOT/shares" 2>/dev/null || true

# `fleett.env` wins over the legacy `fleet.env`; keep an existing file as-is.
envfile="$ROOT/fleett.env"
if [ ! -f "$envfile" ] && [ -f "$ROOT/fleet.env" ]; then
  envfile="$ROOT/fleet.env"
fi
if [ ! -f "$envfile" ]; then
  if command -v openssl >/dev/null 2>&1; then
    token=$(openssl rand -hex 32)
  else
    token=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
  fi
  ( umask 077; cat > "$envfile" <<EOF
FLEETT_ROOT=$ROOT
FLEETT_BIND=127.0.0.1:8090
FLEETT_DRIVER=compose
FLEETT_TOKEN=$token
EOF
  )
  chmod 600 "$envfile"
  say "Wrote ${envfile} (bearer token inside)"
elif [ "$envfile" = "$ROOT/fleet.env" ]; then
  say "Kept existing ${envfile} (legacy name)"
else
  say "Kept existing ${envfile}"
fi

case ":${PATH}:" in
  *":${dir}:"*) ;;
  *) warn "${dir} is not in PATH" ;;
esac
command -v docker >/dev/null 2>&1 || warn "docker not found; compose driver needs it"

if grep -rqs '^name: hermes-' "$ROOT/instances" 2>/dev/null; then
  say "Legacy hermes-<id> compose projects detected: run \`fleett migrate --from hermes-fleet --apply\`."
fi

cat <<EOF

fleett is installed.

  data root : ${ROOT}
  env file  : ${envfile}
  start     : ${dir}/${BIN}
  legacy    : ${dir}/${LEGACY_BIN} (symlink)
  open      : http://127.0.0.1:8090

The bearer token is FLEETT_TOKEN in ${envfile}.
The fleett Docker network is created on first start if missing.
EOF
