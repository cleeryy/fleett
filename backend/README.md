# hermes-fleet — backend

Control plane for fleet-managed Hermes agents. One instance is one Docker
Compose project (`hermes-<id>`); the compose file is written on disk and the
control plane only drives it, so agents keep running when this process is down,
restarts, or is deleted.

The HTTP contract lives in `../API.md`, the architecture in `../SPEC.md`.

## Requirements

- Rust 1.99 (`cargo`, `rustc`)
- Docker Engine with the Compose v2 plugin (only for `FLEET_DRIVER=compose`)
- Linux (the secret file is written with Unix mode `0600` and data directories
  are chowned to UID/GID 10000)

## Configuration

Environment variables (a `fleet.env` file in `FLEET_ROOT` fills in whatever the
process environment does not define):

| Variable | Default | Meaning |
|---|---|---|
| `FLEET_ROOT` | `/opt/hermes-fleet` | Fleet directory: `fleet.db`, `instances/`, `shares/`, `templates/` |
| `FLEET_BIND` | `127.0.0.1:8090` | Listen address |
| `FLEET_TOKEN` | — | Bearer token for every `/api/*` route except `/api/health` |
| `FLEET_ALLOW_NO_AUTH` | `0` | `1` disables authentication entirely (loud warning at startup) |
| `FLEET_DRIVER` | `mock` | `mock` (in-memory, no Docker) or `compose` |
| `FLEET_IMAGE` | `hermes-agent:0.22.2-local` | Image used for every agent container |
| `FLEET_NETWORK` | `hermes-fleet` | Existing external Docker network all agents join |
| `FLEET_WEB_DIST` | auto-detect | Optional path to a built frontend (see “Serving the console”) |

`FLEET_TOKEN` is required unless `FLEET_ALLOW_NO_AUTH=1`. There is no default
token: the process refuses to start rather than run unauthenticated by accident.

### `fleet.env`

If `$FLEET_ROOT/fleet.env` exists, it is read at startup and any variable it
defines is used **only when the process environment does not already define
it**. Format: `KEY=VALUE`, one per line; blank lines and `#` comments are
ignored; a matching pair of surrounding quotes is stripped; nothing is expanded
or executed (no `$VAR`, no backticks, no `source`). Process environment always
wins.

```
# /opt/hermes-fleet/fleet.env
FLEET_TOKEN=change-me
FLEET_DRIVER=compose
FLEET_BIND=127.0.0.1:8090
```

`FLEET_ROOT` is resolved from the process environment first. When it is unset, the
loader reads `$HOME/.hermes-fleet/fleet.env` if that file exists (the location used by
`install.sh`), then falls back to `/opt/hermes-fleet/fleet.env`.

## Run

```bash
cd backend
cargo run                       # FLEET_DRIVER=mock, needs FLEET_TOKEN
FLEET_TOKEN=dev cargo run       # same, with a token
FLEET_DRIVER=compose FLEET_TOKEN=secret cargo run
cargo run -- --version          # prints "hermes-fleet 0.1.0"
```

Release build:

```bash
cargo build --release
# binary: backend/target/release/hermes-fleet
```

### Serving the console

Two mutually exclusive modes:

- **Default (no feature).** At startup the binary looks for a built frontend in
  `$FLEET_WEB_DIST`, then `./web/dist`, then `../web/dist` (the last one matches
  `cargo run` from `backend/`). If `index.html` is found it is served with
  `tower-http` `ServeDir` and an SPA fallback to `index.html`; otherwise `/`
  answers with a JSON hint. The directory is resolved once at startup.
- **`embed-web` feature.** The frontend is compiled into the binary with
  `rust-embed` (`../web/dist`) and served with the same SPA fallback; nothing is
  read from disk. This is what the one-file installer uses:

  ```bash
  cd backend
  cargo build --release --features embed-web
  ```

  Caveat: `../web/dist` must exist **at compile time**. Without it the
  `rust-embed` macro fails the build, so the feature is never enabled by
  default and `cargo test` / `cargo clippy` work without a frontend checkout.

## Layout

```
$FLEET_ROOT/
├── fleet.db                  # SQLite index (cache, not the source of truth)
├── fleet.env                 # optional defaults (see above)
├── instances/<id>/
│   ├── compose.yaml          # generated, one compose project per agent
│   ├── data/                 # mounted at /opt/data, still there after delete
│   │   ├── config.yaml       # generated from the template
│   │   └── SOUL.md           # generated from the template (create only)
│   └── secrets.env           # mode 0600, bot token + gate token
├── shares/<name>/            # mounted at /opt/data/shared/<name>
└── templates/<id>/           # optional overrides: config.yaml, SOUL.md, template.yaml
```

The database is rebuilt from `instances/*/compose.yaml` and `shares/*` at every
startup:

- a directory with a `compose.yaml` that is not indexed is adopted (model,
  base URL, API-key variable, channel, memory limit and shares are recovered
  from the compose file and from `data/config.yaml`; the template is reported
  as `custom`, and a later `PATCH` reuses that config file as its template);
- an index row whose `compose.yaml` disappeared is dropped (the data directory,
  if any, is left untouched and is reused by a later create with the same id).

Deleting `fleet.db` therefore loses notes, names and timestamps, never an agent.

## Security notes

- **Bearer token** on every route except `GET /api/health`. Comparison is
  constant time (`subtle`), no cookies, no sessions. The token is never logged
  and never returned by the API.
- **`FLEET_ALLOW_NO_AUTH=1` is dangerous.** It disables authentication for
  everyone who can reach the port; the process prints a multi-line warning.
  Bind it to localhost or put it behind an authenticating proxy.
- **Secrets stay on the host.** `secrets.env` is `0600`, owned by the control
  plane user. It is passed to the container as an `env_file`, never as
  `environment:` values, so tokens do not appear in `docker inspect` output.
  The API only exposes key names (`env_keys`), never values.
- **Container hardening** (written into every compose file): dedicated bridge
  network (never `network_mode: host`), no Docker socket mount, single data
  bind mount plus explicit share mounts, `security_opt:
  no-new-privileges:true`, `cap_drop: NET_RAW`, `tmpfs: /tmp`,
  `pids_limit: 256`, `mem_limit` between 256 and 8192 MB, `json-file` logs
  capped at 10 MB × 3.
- **UID/GID 10000 for every agent.** Data directories and share directories are
  chowned to `10000:10000` when the process runs as root (best effort
  otherwise). Shares are writable by every instance that mounts them, so
  isolation between guests relies on the container, not on the UID.
- **Input validation**: `id` and share names are `[a-z0-9-]`, 2–32 characters,
  must start with a letter or digit (Docker Compose project names cannot start
  with `-`); `mem_limit_mb` is 256..=8192; `model` and `base_url` must be
  non-empty; `channel.type` is `telegram`, `discord` or `none`; a share must
  exist before it can be attached; `logs?tail` is 1..=2000.
- **Template rendering** parses the rendered YAML before writing it, so a broken
  template fails with `422` instead of writing an invalid config. For instances
  created from a template, `data/config.yaml` is generated and manual edits are
  overwritten by the next `PATCH`. Instances adopted from disk (`template:
  "custom"`) use their own `data/config.yaml` as the template, so their edits
  survive a `PATCH`. `data/SOUL.md` is only written at create time.
- **Docker failures** are surfaced as `502` with the compose stderr truncated to
  2 KB.

## What is mocked

`FLEET_DRIVER=mock` (the default) never calls Docker:

- instance state lives in memory (running/stopped/error/unknown), `up`, `stop`,
  `start`, `restart` and `down` only mutate that state;
- `ps` returns a synthetic container id (UUID) and image, `uptime_s` counts
  from the simulated start;
- `logs` returns a fixed set of `[mock]` lines, bounded by `tail`;
- `ping` always reports Docker as healthy.

Everything else is real: compose files, `config.yaml`, `SOUL.md`, `secrets.env`
(with `0600`), shares on disk, validation, auth, SQLite index and reconciliation.
The test suite uses `MockDriver` exclusively and never needs a Docker daemon.

## Tests

```bash
cargo fmt --all --check
cargo test                  # 23 unit + 24 integration tests
cargo clippy --all-targets  # must stay warning-free
```

Integration tests (`tests/api.rs`) build the real router with a temporary
`FLEET_ROOT` and a `MockDriver`, then call it through
`tower::ServiceExt::oneshot`. They cover auth (401/204), health, templates,
create (files on disk, YAML validity, `0600` secrets), duplicate ids, invalid
ids and payloads, get/list, patch (compose rewrite, data preserved, token
rotation), start/stop/restart, logs, share create/attach/detach/delete with the
409-while-attached rule, delete keep vs purge, the generated compose shape
(restart policy, external network, no host networking, no Docker socket, share
mounts) and driver failures (502 with truncated stderr).

## API notes

`API.md` is implemented literally. Additions (all optional, none required by the
contract):

- `POST /api/instances` accepts `gate_token` (stored as
  `HERMES_GATE_PROFILE_TOKEN`, and also under `api_key_env` when the two
  differ) and `env` (extra `KEY=VALUE` pairs for `secrets.env`, value names
  validated, newlines rejected).
- `PATCH /api/instances/{id}` also accepts `gate_token` to rotate the gate
  token; `channel.token` remains the documented way to rotate a bot token.
- `GET /api/health` adds `uptime_s` (control-plane uptime in seconds).
- Ids or share names starting with `-` are rejected with `400 invalid_id` /
  `400 invalid_share`, even though `^[a-z0-9-]{2,32}$` allows it: Docker Compose
  refuses project and directory names that do not start with a letter or digit.
- `DELETE /api/shares/{name}` returns `409 share_attached` while any instance
  still mounts the share.
- Attach and detach are idempotent (`204` when already in the requested state).
- Unknown `/api/*` routes return `404` with the standard error envelope instead
  of the SPA.
- In the `compose` driver `container.uptime_s` is `null` (Docker Compose does
  not expose the start time portably); the mock driver reports it.
- On a failed create the instance directory, the compose file and the index row
  are kept in `status: "error"` with `last_error` set, so the failure is visible
  and fixable; the request still returns `502`.
