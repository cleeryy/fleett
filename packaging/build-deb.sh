#!/usr/bin/env bash
# Build the Debian package of the fleett control plane.
#
#   packaging/build-deb.sh <version> [arch]
#   packaging/build-deb.sh 0.3.0
#   packaging/build-deb.sh 0.3.0 arm64
#
# Environment:
#   FLEETT_BINARY   reuse an existing binary instead of building one (it must
#                   have been built with --features embed-web, otherwise the
#                   package serves no web console)
#   OUTPUT_DIR      where to write the .deb (default: <repo>/dist)
#   CARGO, PNPM     tool overrides
#
# The package contains /usr/bin/fleett, the systemd unit, the conffile
# /etc/fleett/fleett.env and the documentation. postinst creates the fleett
# user/group (UID/GID 10000), the data root and the secrets key; it does not
# start the service.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: packaging/build-deb.sh <version> [arch]

  version   package version, with or without a leading "v" (0.3.0, v0.3.0)
  arch      dpkg architecture (default: `dpkg --print-architecture`, else amd64)
EOF
}

if [ "$#" -lt 1 ]; then
  usage
  exit 2
fi

version="${1#v}"
arch="${2:-}"
if [ -z "$arch" ]; then
  if command -v dpkg >/dev/null 2>&1; then
    arch="$(dpkg --print-architecture)"
  else
    arch="amd64"
  fi
fi

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v dpkg-deb >/dev/null 2>&1 || {
  echo "build-deb: dpkg-deb is required (package dpkg-dev / dpkg)" >&2
  exit 1
}

# --- binary -----------------------------------------------------------------
if [ -n "${FLEETT_BINARY:-}" ]; then
  binary="${FLEETT_BINARY}"
  echo "build-deb: reusing ${binary} (must embed the web console)" >&2
else
  pnpm_bin="${PNPM:-pnpm}"
  cargo_bin="${CARGO:-cargo}"

  command -v "$pnpm_bin" >/dev/null 2>&1 || {
    echo "build-deb: $pnpm_bin not found (npm install -g pnpm@9)" >&2
    exit 1
  }
  command -v "$cargo_bin" >/dev/null 2>&1 || {
    echo "build-deb: $cargo_bin not found (https://rustup.rs)" >&2
    exit 1
  }

  echo "build-deb: building the web console" >&2
  ( cd web && "$pnpm_bin" install --frozen-lockfile && "$pnpm_bin" build )

  echo "build-deb: building the control plane" >&2
  "$cargo_bin" build --manifest-path backend/Cargo.toml --release --locked --features embed-web
  binary="backend/target/release/fleett"
fi

if [ ! -f "$binary" ]; then
  echo "build-deb: binary not found: $binary" >&2
  exit 1
fi

# --- staging tree -----------------------------------------------------------
work="$(mktemp -d "${TMPDIR:-/tmp}/fleett-deb.XXXXXX")"
trap 'rm -rf "$work"' EXIT
root="$work/fleett_${version}_${arch}"

install -d "$root/DEBIAN" \
  "$root/usr/bin" \
  "$root/lib/systemd/system" \
  "$root/etc/fleett" \
  "$root/usr/share/doc/fleett"

install -m 0755 "$binary" "$root/usr/bin/fleett"
install -m 0644 packaging/fleett.service "$root/lib/systemd/system/fleett.service"
install -m 0640 packaging/fleett.env "$root/etc/fleett/fleett.env"
install -m 0644 README.md "$root/usr/share/doc/fleett/README.md"
install -m 0644 packaging/copyright "$root/usr/share/doc/fleett/copyright"
for script in postinst prerm postrm; do
  install -m 0755 "packaging/$script" "$root/DEBIAN/$script"
done

# dpkg-deb keeps the ownership of the staging tree; force root:root and let
# postinst hand /etc/fleett to root:fleett (the build host may not have a
# "fleett" group at all).
printf '/etc/fleett/fleett.env\n' > "$root/DEBIAN/conffiles"

installed_size="$(du -sk --exclude=DEBIAN "$root" | cut -f1)"

cat > "$root/DEBIAN/control" <<EOF
Package: fleett
Version: ${version}
Architecture: ${arch}
Section: admin
Priority: optional
Maintainer: cleeryy <clery.arqueferradou@gmail.com>
Installed-Size: ${installed_size}
Depends: docker.io | docker-ce | moby-engine, adduser, ca-certificates
Homepage: https://github.com/cleeryy/fleett
Description: control plane for fleet-managed Hermes agents
 fleett drives one Docker Compose project per Hermes agent: it creates the
 project from a template, starts and stops it, streams logs, stores provider
 keys encrypted at rest and serves a web console.
 .
 This package installs the control plane, its systemd unit and a documented
 environment file. It does not install Docker and does not start the service:
 set FLEETT_TOKEN in /etc/fleett/fleett.env, then
 "systemctl enable --now fleett".
EOF

# --- package ----------------------------------------------------------------
output_dir="${OUTPUT_DIR:-$repo_root/dist}"
install -d "$output_dir"
deb="$output_dir/fleett_${version}_${arch}.deb"

dpkg-deb --root-owner-group --build "$root" "$deb"

if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$output_dir" && sha256sum "$(basename "$deb")" > "$(basename "$deb").sha256" )
fi

echo "build-deb: wrote $deb"
