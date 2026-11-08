# fleett — specification

Web console to **create, configure, start/stop and delete** Hermes agents, each in its own
Docker container. The existing Hermes instance is **never** touched.

`fleett` is the control plane's name; the managed workloads are Hermes agents and the image,
`/opt/data` and `HERMES_UID`/`HERMES_GID` keep their names.

## Architecture decisions

### 1. One Docker Compose project per instance
`instances/<id>/compose.yaml` is written to disk and driven by
`docker compose -f instances/<id>/compose.yaml -p <project> …`.
Containers are not created on the fly through the Docker API: the compose file is the
source of truth, readable and repairable by hand.

The project name comes from the `name:` field of that file — `fleett-<id>` for instances
created by this build, `hermes-<id>` for instances created by releases before v0.2.0. Using
the declared name (instead of recomputing a prefix) is what makes legacy projects
start/stop/logs-able after an upgrade with no recreation, and updates write that same name
back so a legacy instance is never silently duplicated under the new prefix.

### 2. Full decoupling (hard requirement)
`restart: unless-stopped` on every service. If the web app is stopped, updated or deleted,
**agents keep running**. If the host reboots, Docker brings them back.
The web app is a control plane only: it supervises no process.

### 3. Per-instance data and secrets
```
<root>/                            # ~/.fleett, or /opt/fleett for the system service
├── fleet.db                       # SQLite index (not the source of truth)
├── fleett.env                     # optional environment file (legacy: fleet.env)
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
`FLEETT_DRIVER=compose` drives Docker; `FLEETT_DRIVER=mock` (default for dev/tests)
simulates states without touching Docker. Tests never call Docker.

### 7. Configuration and rename compatibility
- canonical prefix `FLEETT_*`; legacy `FLEET_*` accepted, second in priority
- installer overrides `FLEETT_VERSION|INSTALL_DIR|ROOT|BASE_URL` and the legacy
  `HERMES_FLEET_*` names
- data root `~/.fleett`; when it is absent, an existing `~/.hermes-fleet` is adopted and
  logged once
- environment file `fleett.env` first, then `fleet.env`
- `fleett migrate --from hermes-fleet [--apply]` renames legacy compose projects through a
  `serde_yaml` round-trip (project, container and external network names)

## Default isolation for an instance

- dedicated bridge network `fleett` (never `network_mode: host`)
- no Docker socket mount
- a single data mount: `./data:/opt/data`
- `security_opt: no-new-privileges`, `cap_drop: NET_RAW`, `tmpfs: /tmp`
- `pids_limit: 256`, configurable `mem_limit` (default 1536m) and `cpus` (default 1.0)
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
| migrate | `compose down`, rewrite names, `up -d` if it was running | recreated once |

## Out of scope for v1

- multi-host (single Docker daemon)
- fine-grained Hermes config editing (the Hermes dashboard already does that)
- chat / conversations in the console
- automatic `hermes-gate` integration (fields exist, token is pasted manually)
- SSO (a bearer token is enough for v1)
