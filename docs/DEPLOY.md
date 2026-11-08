# Deployment — hermes-fleet

Nothing here is applied yet. Target: a dedicated Docker host (CT/VM), not CT125.

## Why agents survive the app

Every instance is a Compose project on disk with `restart: unless-stopped`.
Docker (the daemon) supervises containers, not hermes-fleet. Therefore:

| Event | Agents |
|---|---|
| `systemctl stop hermes-fleet` | keep running |
| binary / UI update | keep running |
| app crash | keep running |
| host reboot | restarted by Docker |
| `docker compose stop` (from the UI) | stopped, will not restart |

This is the central property of the design: the control plane is disposable.

## Prerequisites

```bash
docker network create hermes-fleet          # once
docker image ls | grep hermes-agent         # image built from the fork
```

The image is not built by this app. Build once, tag per version:

```bash
docker build -t hermes-agent:0.22.2-local /path/to/hermes-official-v0.22.2
```

## Install

The control plane and every agent container share **UID/GID 10000**, so bind-mounted
data stays readable and writable by both without running the app as root.

```bash
groupadd --system --gid 10000 hermes-fleet 2>/dev/null || true
useradd --system --uid 10000 --gid 10000 \
  --home /opt/hermes-fleet --shell /usr/sbin/nologin hermes-fleet 2>/dev/null || true
usermod -aG docker hermes-fleet

install -d -m 700 -o 10000 -g 10000 /opt/hermes-fleet
install -d -m 700 -o 10000 -g 10000 /opt/hermes-fleet/instances /opt/hermes-fleet/shares
```

If you run the app as root instead, it still works (it can chown the instance data), but
the UID 10000 model is the intended one.

`/etc/hermes-fleet.env` (0600):

```dotenv
FLEET_ROOT=/opt/hermes-fleet
FLEET_BIND=127.0.0.1:8090
FLEET_DRIVER=compose
FLEET_IMAGE=hermes-agent:0.22.2-local
FLEET_NETWORK=hermes-fleet
FLEET_TOKEN=<long token, generate with: openssl rand -hex 32>
```

`/etc/systemd/system/hermes-fleet.service`:

```ini
[Unit]
Description=hermes-fleet control plane
After=docker.service network-online.target
Requires=docker.service

[Service]
EnvironmentFile=/etc/hermes-fleet.env
ExecStart=/usr/local/bin/hermes-fleet
Restart=on-failure
RestartSec=2
# same UID as HERMES_UID/GID inside the containers; docker group for the socket
User=hermes-fleet
Group=hermes-fleet
SupplementaryGroups=docker

[Install]
WantedBy=multi-user.target
```

The binary and `web/dist` are served by the same process (the backend serves `../web/dist`).
If the binary lives elsewhere, set `WorkingDirectory` so `web/dist` is found, or copy
`web/dist` next to the binary.

```bash
systemctl daemon-reload
systemctl enable --now hermes-fleet
curl -s http://127.0.0.1:8090/api/health
```

## Exposure

V1: SSH tunnel or Caddy + Authentik, as for the Hermes dashboard.
The app only has a bearer token; never bind it to `0.0.0.0` without an authenticated
reverse proxy in front.

## Backup

All state lives under `FLEET_ROOT`:

```bash
tar czf fleet-$(date +%F).tgz -C /opt hermes-fleet
```

Containers can be recreated from `instances/*/compose.yaml`; the data
(`instances/*/data`, `shares/*`) is the only irreplaceable part.

## Update

```bash
systemctl stop hermes-fleet          # agents keep running
install -m 755 hermes-fleet /usr/local/bin/hermes-fleet
systemctl start hermes-fleet
```

Rollback: restore the previous binary. The SQLite database is an index; on startup the app
rescans `instances/*/compose.yaml` and reconciles.

## Known limits

- single Docker daemon (no multi-host)
- the Docker socket is root-equivalent: this app is an admin tool, not a public service
- UID/GID fixed at 10000 for every instance (required for writable shares)
- no automatic `hermes-gate` integration in v1
