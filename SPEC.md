# hermes-fleet — specification

Web console to **create, configure, start/stop and delete** Hermes agents, each in its own
Docker container. The existing Hermes instance is **never** touched.

## Architecture decisions

### 1. One Docker Compose project per instance
`instances/<id>/compose.yaml` is written to disk and driven by
`docker compose -f instances/<id>/compose.yaml -p hermes-<id> …`.
Containers are not created on the fly through the Docker API: the compose file is the
source of truth, readable and repairable by hand.

### 2. Full decoupling (hard requirement)
`restart: unless-stopped` on every service. If the web app is stopped, updated or deleted,
**agents keep running**. If the host reboots, Docker brings them back.
The web app is a control plane only: it supervises no process.

### 3. Per-instance data and secrets
```
/opt/hermes-fleet/
├── fleet.db                       # SQLite index (not the source of truth)
├── instances/<id>/
│   ├── compose.yaml
│   ├── data/                      # mounted at /opt/data (full Hermes home)
│   └── secrets.env                # 0600, bot token + gate token
├── shares/<name>/                 # directories shared across instances
└── templates/<template>/          # config.yaml + SOUL.md templates
```
UID/GID are fixed at **10000** for every instance: required so shares are writable by
several containers. Trade-off accepted: isolation between guests rests on the container
and on one mount per instance — never on the UID.

### 4. Shares (shared mounts)
A share is a named directory mounted into N instances:
`shares/<name>` → `/opt/data/shared/<name>`, `rw` or `ro` per link.
Adding or removing a share rewrites the compose file and runs `docker compose up -d`
(container recreated, data preserved).

### 5. Templates
`templates/<id>/{config.yaml,SOUL.md}`. Rendering substitutes only explicit variables
(`{{MODEL}}`, `{{BASE_URL}}`, `{{AGENT_NAME}}`, `{{API_KEY_ENV}}`). The rest of the file is
valid Hermes YAML, derived from `cli-config.yaml.example` (local reference, not in git).
No config generation from scratch: a template is reviewable and versioned.

### 6. Driver
`FLEET_DRIVER=compose` drives Docker; `FLEET_DRIVER=mock` (default for dev/tests) simulates
states without touching Docker. Tests never call Docker.

## Default isolation for an instance

- dedicated bridge network `hermes-fleet` (never `network_mode: host`)
- no Docker socket mount
- a single data mount: `./data:/opt/data`
- `security_opt: no-new-privileges`, `cap_drop: NET_RAW`, `tmpfs: /tmp`
- `pids_limit: 256`, configurable `mem_limit` (default 1536m)
- capped json-file logs (10m × 3)

## Lifecycle

| Action | Effect | Container |
|---|---|---|
| create | writes compose + data + secrets, `up -d` | running |
| stop | `compose stop` | stopped (does not restart) |
| start | `compose start` | running |
| restart | `compose restart` | running |
| update | rewrite compose, `up -d` | recreated |
| delete | `compose down` | removed, data preserved |
| delete `?purge=true` | `compose down` + `rm -rf data/` | removed, data erased |

## Out of scope for v1

- multi-host (single Docker daemon)
- fine-grained Hermes config editing (the Hermes dashboard already does that)
- chat / conversations in the console
- automatic `hermes-gate` integration (fields exist, token is pasted manually)
- SSO (a bearer token is enough for v1)
