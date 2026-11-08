# Deployment — fleett

Nothing here is applied yet. Target: a dedicated Docker host (CT/VM), not CT125.

## Why agents survive the app

Every instance is a Compose project on disk with `restart: unless-stopped`.
Docker (the daemon) supervises containers, not fleett. Therefore:

| Event | Agents |
|---|---|
| `systemctl stop fleett` | keep running |
| binary / UI update | keep running |
| app crash | keep running |
| host reboot | restarted by Docker |
| `docker compose stop` (from the UI) | stopped, will not restart |

This is the central property of the design: the control plane is disposable.

## Prerequisites

```bash
docker network create fleett                # optional: the app creates it
docker image ls | grep hermes-agent         # image built from the fork
sudo fleett egress install                  # LAN egress firewall (docs/EGRESS.md)
```

The image is not built by this app. Build once, tag per version:

```bash
docker build -t hermes-agent:0.22.2-local /path/to/hermes-official-v0.22.2
```

Then create the encryption key for instance secrets (once per host, outside
`FLEETT_ROOT`; see [SECRETS.md](SECRETS.md)):

```bash
sudo fleett secrets init                    # /etc/fleett/secrets.key, mode 0640
fleett secrets migrate                      # dry run: what would be encrypted
```

The image name, `/opt/data`, `HERMES_UID` and `HERMES_GID` are unchanged by the rename.

## Install

The control plane and every agent container share **UID/GID 10000**, so bind-mounted
data stays readable and writable by both without running the app as root.

```bash
groupadd --system --gid 10000 fleett 2>/dev/null || true
useradd --system --uid 10000 --gid 10000 \
  --home /opt/fleett --shell /usr/sbin/nologin fleett 2>/dev/null || true
usermod -aG docker fleett

install -d -m 700 -o 10000 -g 10000 /opt/fleett
install -d -m 700 -o 10000 -g 10000 /opt/fleett/instances /opt/fleett/shares
```

If you run the app as root instead, it still works (it can chown the instance data), but
the UID 10000 model is the intended one.

`/etc/fleett.env` (0600):

```dotenv
FLEETT_ROOT=/opt/fleett
FLEETT_BIND=127.0.0.1:8090
FLEETT_DRIVER=compose
FLEETT_IMAGE=hermes-agent:0.22.2-local
FLEETT_NETWORK=fleett
FLEETT_EGRESS=restrict                      # off = no firewall check, with a warning
FLEETT_SECRETS_KEY_FILE=/etc/fleett/secrets.key  # AES-256 key, outside FLEETT_ROOT
FLEETT_TOKEN=<long token, generate with: openssl rand -hex 32>
```

`/etc/systemd/system/fleett.service`:

```ini
[Unit]
Description=fleett control plane
After=docker.service network-online.target
Requires=docker.service

[Service]
EnvironmentFile=/etc/fleett.env
ExecStart=/usr/local/bin/fleett
Restart=on-failure
RestartSec=2
# same UID as HERMES_UID/GID inside the containers; docker group for the socket
User=fleett
Group=fleett
SupplementaryGroups=docker

[Install]
WantedBy=multi-user.target
```

The binary and `web/dist` are served by the same process (the backend serves `../web/dist`).
If the binary lives elsewhere, set `WorkingDirectory` so `web/dist` is found, or copy
`web/dist` next to the binary.

```bash
systemctl daemon-reload
systemctl enable --now fleett
curl -s http://127.0.0.1:8090/api/health
```

## Upgrading from `hermes-fleet`

The old unit keeps working through the `hermes-fleet` compatibility symlink, but the canonical
name is `fleett`:

```bash
systemctl disable --now hermes-fleet          # agents keep running
install -m 755 fleett /usr/local/bin/fleett
ln -sf fleett /usr/local/bin/hermes-fleet     # done by install.sh
mv /etc/hermes-fleet.env /etc/fleett.env      # FLEET_* keys keep working
systemctl enable --now fleett
```

Data compatibility: `FLEETT_ROOT` is read first, then `FLEET_ROOT`; the environment file
`fleett.env` wins over `fleet.env`; an existing `~/.hermes-fleet` is adopted when `~/.fleett`
does not exist. Compose projects declared as `hermes-<id>` keep working untouched and stay
attached to the legacy `hermes-fleet` Docker network until they are migrated. To rename
them:

```bash
fleett migrate --from hermes-fleet            # plan only
fleett migrate --from hermes-fleet --apply    # down, rewrite, up when it was running
```

## Exposure

V1: SSH tunnel or Caddy + Authentik, as for the Hermes dashboard.
The app only has a bearer token; never bind it to `0.0.0.0` without an authenticated
reverse proxy in front.

## Backup

All state lives under `FLEETT_ROOT`, **except the secrets key**, which lives
outside it on purpose (`/etc/fleett/secrets.key` by default):

```bash
tar czf fleett-$(date +%F).tgz -C /opt fleett
```

Containers can be recreated from `instances/*/compose.yaml`; the data
(`instances/*/data`, `shares/*`) is the only irreplaceable part.

Instance secrets are encrypted in `fleet.db` with the key file. Back the key up
**separately** (never in the same archive as the root): without it the
encrypted rows are unreadable, and without `fleet.db` the tokens are gone. See
[SECRETS.md](SECRETS.md).

## Update

```bash
systemctl stop fleett                # agents keep running
install -m 755 fleett /usr/local/bin/fleett
systemctl start fleett
```

Rollback: restore the previous binary. The SQLite database is an index; on startup the app
rescans `instances/*/compose.yaml` and reconciles.

## Known limits

- single Docker daemon (no multi-host)
- the Docker socket is root-equivalent: this app is an admin tool, not a public service
- UID/GID fixed at 10000 for every instance (required for writable shares)
- no automatic `hermes-gate` integration in v1
