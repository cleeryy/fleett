# Install — hermes-fleet

## One-liner (Linux and macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/cleeryy/hermes-fleet/master/install.sh | bash
```

Linux builds are static (musl): they run on any distribution, regardless of glibc.
macOS builds cover Apple Silicon and Intel.

The script:

1. detects OS and architecture (`linux`/`darwin`, `x86_64`/`aarch64`);
2. resolves the latest GitHub release;
3. downloads the tarball and verifies its SHA-256;
4. installs the binary to `/usr/local/bin` when writable, otherwise `~/.local/bin`;
5. creates `~/.hermes-fleet/{instances,shares}` (mode 700);
6. writes `~/.hermes-fleet/fleet.env` (mode 600) with `FLEET_ROOT`, `FLEET_BIND`,
   `FLEET_DRIVER=compose` and a generated `FLEET_TOKEN`;
7. prints the start command and the dashboard URL.

It is idempotent: re-running it replaces the binary and **keeps** data and secrets.

## First run

```bash
hermes-fleet                            # reads ~/.hermes-fleet/fleet.env
# the hermes-fleet Docker network is created on first start if missing
# open http://127.0.0.1:8090
```

The Hermes image is not built by the installer. Build it once from your fork:

```bash
docker build -t hermes-agent:0.22.2-local /path/to/hermes-official-v0.22.2
```

Set `FLEET_IMAGE` in `fleet.env` if you use another tag.

## Overrides

```bash
# pin a version, install elsewhere, keep data in a custom root
curl -fsSL https://raw.githubusercontent.com/cleeryy/hermes-fleet/master/install.sh \
  | HERMES_FLEET_VERSION=0.1.0 HERMES_FLEET_INSTALL_DIR="$HOME/bin" \
    HERMES_FLEET_ROOT=/srv/hermes-fleet bash
```

| Variable | Default |
|---|---|
| `HERMES_FLEET_VERSION` | latest release |
| `HERMES_FLEET_INSTALL_DIR` | `/usr/local/bin` or `~/.local/bin` |
| `HERMES_FLEET_ROOT` | `~/.hermes-fleet` |
| `HERMES_FLEET_BASE_URL` | GitHub release URL (mirrors, tests) |

## Manual install

```bash
version=0.1.0
target=linux-x86_64          # or linux-aarch64, darwin-aarch64, darwin-x86_64
curl -fsSLO "https://github.com/cleeryy/hermes-fleet/releases/download/v${version}/hermes-fleet_${version}_${target}.tar.gz"
curl -fsSLO "https://github.com/cleeryy/hermes-fleet/releases/download/v${version}/hermes-fleet_${version}_${target}.tar.gz.sha256"
sha256sum -c "hermes-fleet_${version}_${target}.tar.gz.sha256"
tar -xzf "hermes-fleet_${version}_${target}.tar.gz"
install -m 0755 hermes-fleet /usr/local/bin/hermes-fleet
```

## Update

Re-run the one-liner. The binary is replaced; `fleet.env`, instances and shares are kept.
Agents keep running during the update — the control plane is disposable by design.

## Uninstall

```bash
rm -f /usr/local/bin/hermes-fleet          # or ~/.local/bin/hermes-fleet
rm -rf ~/.hermes-fleet
# containers are not removed automatically:
docker ps --filter label=com.docker.compose.project --format '{{.Names}}'
```

Use the UI to delete instances first if you want their containers removed.

## macOS notes

The binary is unsigned but not quarantined when downloaded with `curl`. If macOS still
blocks it:

```bash
xattr -d com.apple.quarantine ~/.local/bin/hermes-fleet
```

Docker Desktop must be running for the `compose` driver.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `could not resolve the latest release` | set `HERMES_FLEET_VERSION`; the GitHub API may be rate-limited |
| `checksum mismatch` | retry; if persistent, download manually and inspect |
| `docker not found` warning | install Docker; the app starts but cannot manage instances |
| `502` on start/stop | `docker compose` failed; the UI shows the stderr |
| app starts but no UI | `web/dist` was not embedded; use a release binary or build with `--features embed-web` |
