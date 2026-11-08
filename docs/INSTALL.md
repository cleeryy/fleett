# Install — fleett

## One-liner (Linux and macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/cleeryy/fleett/master/install.sh | bash
```

Linux builds are static (musl): they run on any distribution, regardless of glibc.
macOS builds cover Apple Silicon and Intel.

The script:

1. detects OS and architecture (`linux`/`darwin`, `x86_64`/`aarch64`);
2. resolves the latest GitHub release;
3. downloads `fleett_<version>_<target>.tar.gz` and verifies its SHA-256;
4. installs the `fleett` binary to `/usr/local/bin` when writable, otherwise `~/.local/bin`;
5. creates a `hermes-fleet` symlink next to it, so scripts and units written against the
   old name keep working;
6. creates `~/.fleett/{instances,shares}` (mode 700) — or reuses `~/.hermes-fleet` when it
   already exists;
7. writes `~/.fleett/fleett.env` (mode 600) with `FLEETT_ROOT`, `FLEETT_BIND`,
   `FLEETT_DRIVER=compose` and a generated `FLEETT_TOKEN`;
8. prints the start command and the dashboard URL.

It is idempotent: re-running it replaces the binary and **keeps** data, environment file and
secrets. An existing `fleet.env` is kept and used when `fleett.env` is absent.

## First run

```bash
fleett                                  # reads ~/.fleett/fleett.env
# the fleett Docker network is created on first start if missing
# open http://127.0.0.1:8090
```

The Hermes image is not built by the installer. Build it once from your fork:

```bash
docker build -t hermes-agent:0.22.2-local /path/to/hermes-official-v0.22.2
```

Set `FLEETT_IMAGE` in `fleett.env` if you use another tag. The image name, `/opt/data`,
`HERMES_UID` and `HERMES_GID` are unchanged by the rename.

## Overrides

```bash
# pin a version, install elsewhere, keep data in a custom root
curl -fsSL https://raw.githubusercontent.com/cleeryy/fleett/master/install.sh \
  | FLEETT_VERSION=0.2.0 FLEETT_INSTALL_DIR="$HOME/bin" \
    FLEETT_ROOT=/srv/fleett bash
```

| Variable | Legacy alias | Default |
|---|---|---|
| `FLEETT_VERSION` | `HERMES_FLEET_VERSION` | latest release |
| `FLEETT_INSTALL_DIR` | `HERMES_FLEET_INSTALL_DIR` | `/usr/local/bin` or `~/.local/bin` |
| `FLEETT_ROOT` | `HERMES_FLEET_ROOT` | `~/.fleett` (or the existing `~/.hermes-fleet`) |
| `FLEETT_BASE_URL` | `HERMES_FLEET_BASE_URL` | GitHub release URL (mirrors, tests) |
| `FLEETT_REPO` | `HERMES_FLEET_REPO` | `cleeryy/fleett` |

## Manual install

```bash
version=0.2.0
target=linux-x86_64          # or linux-aarch64, darwin-aarch64, darwin-x86_64
curl -fsSLO "https://github.com/cleeryy/fleett/releases/download/v${version}/fleett_${version}_${target}.tar.gz"
curl -fsSLO "https://github.com/cleeryy/fleett/releases/download/v${version}/fleett_${version}_${target}.tar.gz.sha256"
sha256sum -c "fleett_${version}_${target}.tar.gz.sha256"
tar -xzf "fleett_${version}_${target}.tar.gz"
install -m 0755 fleett /usr/local/bin/fleett
ln -sf fleett /usr/local/bin/hermes-fleet    # optional compatibility
```

## Upgrade from `hermes-fleet`

Re-run the one-liner. The installer:

- replaces `hermes-fleet` with a symlink to `fleett`, so an existing unit or script that
  calls the old path keeps working;
- keeps the data root: `~/.fleett` is used when it exists, otherwise an existing
  `~/.hermes-fleet` is adopted;
- keeps `fleet.env` when `fleett.env` is absent; no token is rotated.

Instances and agents keep running during the upgrade — the control plane is disposable by
design. The compose projects still declared as `hermes-<id>` are fully manageable without
any migration. To rename them:

```bash
fleett migrate --from hermes-fleet            # dry run: prints the plan
fleett migrate --from hermes-fleet --apply    # rewrites and restarts running instances
```

The migration reads and rewrites the compose documents with `serde_yaml`; it never string-
replaces. Stopped instances are rewritten but not started.

## Update

Re-run the one-liner. The binary is replaced; `fleett.env`, instances and shares are kept.
Agents keep running during the update — the control plane is disposable by design.

## Uninstall

```bash
rm -f /usr/local/bin/fleett /usr/local/bin/hermes-fleet   # or ~/.local/bin/…
rm -rf ~/.fleett                                          # legacy root: ~/.hermes-fleet
# containers are not removed automatically:
docker ps --filter label=com.docker.compose.project --format '{{.Names}}'
```

Use the UI to delete instances first if you want their containers removed.

## macOS notes

The binary is unsigned but not quarantined when downloaded with `curl`. If macOS still
blocks it:

```bash
xattr -d com.apple.quarantine ~/.local/bin/fleett
```

Docker Desktop must be running for the `compose` driver.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `could not resolve the latest release` | set `FLEETT_VERSION`; the GitHub API may be rate-limited |
| `checksum mismatch` | retry; if persistent, download manually and inspect |
| `docker not found` warning | install Docker; the app starts but cannot manage instances |
| `502` on start/stop | `docker compose` failed; the UI shows the stderr |
| app starts but no UI | `web/dist` was not embedded; use a release binary or build with `--features embed-web` |
| legacy warning at startup | run `fleett migrate --from hermes-fleet --apply` |
