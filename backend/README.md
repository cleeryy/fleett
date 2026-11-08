# fleett — backend

Control plane for fleet-managed Hermes agents. One instance is one Docker
Compose project (`fleett-<id>`, or the `name:` declared in the instance file);
the compose file is written on disk and the control plane only drives it, so
agents keep running when this process is down, restarts, or is deleted.

The HTTP contract lives in `../API.md`, the architecture in `../SPEC.md`.

## Requirements

- Rust 1.99 (`cargo`, `rustc`)
- Docker Engine with the Compose v2 plugin (only for `FLEETT_DRIVER=compose`)
- Linux (the secret file is written with Unix mode `0600` and data directories
  are chowned to UID/GID 10000)

## Configuration

Environment variables (a `fleett.env` file in `FLEETT_ROOT`, or the legacy
`fleet.env`, fills in whatever the process environment does not define):

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_ROOT` | `~/.fleett` (or `/opt/fleett` without `HOME`) | Fleet directory: `fleet.db`, `instances/`, `shares/`, `templates/` |
| `FLEETT_BIND` | `127.0.0.1:8090` | Listen address |
| `FLEETT_TOKEN` | — | Bearer token for every `/api/*` route except `/api/health` |
| `FLEETT_ALLOW_NO_AUTH` | `0` | `1` disables authentication entirely (loud warning at startup) |
| `FLEETT_DRIVER` | `mock` | `mock` (in-memory, no Docker) or `compose` |
| `FLEETT_IMAGE` | `hermes-agent:0.22.2-local` | Image used for every agent container |
| `FLEETT_NETWORK` | `fleett` | Existing external Docker network all agents join |
| `FLEETT_MAX_TOTAL_CPU` | `0` | Optional fleet-wide CPU cap (sum of `cpu_limit`); `0` disables it |
| `FLEETT_MAX_TOTAL_MEM_MB` | `0` | Optional fleet-wide memory cap in MB; `0` disables it |
| `FLEETT_WEB_DIST` | auto-detect | Optional path to a built frontend (see “Serving the console”) |

`FLEETT_TOKEN` is required unless `FLEETT_ALLOW_NO_AUTH=1`. There is no default
token: the process refuses to start rather than run unauthenticated by accident.

### `fleett.env`

If `$FLEETT_ROOT/fleett.env` exists, it is read at startup and any variable it
defines is used **only when the process environment does not already define
it**. Format: `KEY=VALUE`, one per line; blank lines and `#` comments are
ignored; a matching pair of surrounding quotes is stripped; nothing is expanded
or executed (no `$VAR`, no backticks, no `source`). Process environment always
wins.

```
# /opt/fleett/fleett.env
FLEETT_TOKEN=change-me
FLEETT_DRIVER=compose
FLEETT_BIND=127.0.0.1:8090
```

`FLEETT_ROOT` is resolved from the process environment first. When it is unset, the
loader looks for `fleett.env` then `fleet.env` in `$HOME/.fleett`; when that directory
does not exist it adopts `$HOME/.hermes-fleet` (logged once); without `HOME` it falls
back to `/opt/fleett`.

The legacy `FLEET_*` prefix is still read, second to `FLEETT_*`, and a legacy env file
named `fleet.env` is used when `fleett.env` is absent.

## Run

```bash
cd backend
cargo run                       # FLEETT_DRIVER=mock, needs FLEETT_TOKEN
FLEETT_TOKEN=dev cargo run      # same, with a token
FLEETT_DRIVER=compose FLEETT_TOKEN=secret cargo run
cargo run -- --version          # prints "fleett 0.2.0"
cargo run -- migrate --from hermes-fleet   # dry-run the project rename
```

Release build:

```bash
cargo build --release
# binary: backend/target/release/fleett
```

### Serving the console

Two mutually exclusive modes:

- **Default (no feature).** At startup the binary looks for a built frontend in
  `$FLEETT_WEB_DIST`, then `./web/dist`, then `../web/dist` (the last one matches
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
$FLEETT_ROOT/
├── fleet.db                  # SQLite index (cache, not the source of truth); name unchanged
├── fleett.env                # optional defaults (legacy: fleet.env)
├── instances/<id>/
│   ├── compose.yaml          # generated, one compose project per agent
│   ├── data/                 # mounted at /opt/data, still there after delete
│   │   ├── config.yaml       # generated from the template
│   │   ├── healthcheck.py    # generated probe used by the Docker health check
│   │   └── SOUL.md           # generated from the template (create only)
│   └── secrets.env           # legacy only: plaintext, removed by `fleett secrets migrate --apply`
├── shares/<name>/            # mounted at /opt/data/shared/<name>
└── templates/<id>/           # optional overrides: config.yaml, SOUL.md, template.yaml
```

The database is **not** the source of truth: it is an index reconciled with the
filesystem at every startup.

Rebuildable from disk (`instances/*/compose.yaml` plus `instances/*/data/`):

- the instance id (directory name) and its presence in the fleet;
- `mem_limit_mb` (from `mem_limit`) and `cpu_limit` (from `cpus`);
- shares and their mode (from the `../../shares/<name>:…` volumes);
- `model`, `base_url`, `api_key_env` and `channel` (from `data/config.yaml`);
- the template is **not** recovered: a directory with a `compose.yaml` that is
  not indexed is adopted with the template `custom`, and a later `PATCH` reuses
  that `data/config.yaml` as its template.

DB-only (lost if `fleet.db` is deleted, or if a migration fails and is rolled
back):

- `name`, `notes`, `created_at` and `updated_at`;
- the cached `status` / `last_error`, which are recomputed from Docker on the
  next read;
- nothing that is needed to run an agent: containers keep running and are
  re-adopted from their compose file.

`shares/<name>` directories are re-indexed as share rows; a share row whose
directory is gone and that is not attached anywhere is dropped. An index row
whose `compose.yaml` disappeared is dropped (the data directory, if any, is left
untouched and is reused by a later create with the same id).

### Schema migrations

The SQLite file carries a schema version in `PRAGMA user_version`. At startup
`Db::open` applies every migration above that version, one transaction per
migration (`MIGRATIONS` in `src/db.rs`):

- a v0 file — anything written before schema versioning, including a database
  created by v0.1.0 — is upgraded in place, rows preserved;
- the version bump happens in the same transaction as the DDL, so a migration
  that fails rolls back completely and leaves the database at the last good
  version; the control plane refuses to start on a half-migrated file;
- a file whose `user_version` is newer than the binary is refused instead of
  being touched (downgrades are not supported);
- migrations are append-only: a released entry is never edited or reordered.

## Legacy `hermes-<id>` projects

The compose driver reads the project name from the instance `compose.yaml`
instead of recomputing a prefix. A project created by an older release
(declared as `hermes-<id>`) is therefore still started, stopped, restarted,
logged and patched without recreating its container under another name; the DB
row is keyed by instance id and does not change.

Legacy projects are detected read-only at startup and reported with a single
warning pointing at:

```bash
fleett migrate --from hermes-fleet            # print the plan
fleett migrate --from hermes-fleet --apply    # down, rewrite, up
```

`--apply` records whether the project was running, runs
`docker compose -p hermes-<id> down`, rewrites `name`, `container_name` and the
legacy external network name through `serde_yaml` (never string replacement),
writes the file back, then runs `up -d` only for instances that were running.
The DB status is updated and the index reconciled. The planner and the rewrite
are covered by Docker-free unit tests.

## Security notes

- **Bearer token** on every route except `GET /api/health`. Comparison is
  constant time (`subtle`), no cookies, no sessions. The token is never logged
  and never returned by the API.
- **`FLEETT_ALLOW_NO_AUTH=1` is dangerous.** It disables authentication for
  everyone who can reach the port; the process prints a multi-line warning.
  Bind it to localhost or put it behind an authenticating proxy.
- **Secrets are encrypted at rest.** Provider keys and bot tokens are stored as
  one AES-256-GCM blob per instance in `instance_secrets` (`fleet.db`), with a
  key kept outside `FLEETT_ROOT` (`/etc/fleett/secrets.key`, `0640`). The
  compose file declares them as `${KEY}` and the driver injects the decrypted
  values into the `docker compose` child environment, so a `tar` of the root
  contains no usable token. See [docs/SECRETS.md](../docs/SECRETS.md). The API
  only exposes key names (`env_keys`), never values.
  Instances that still have a plaintext `secrets.env` keep working in legacy
  mode (with a warning) until `fleett secrets migrate --apply`.
- **Container hardening** (written into every compose file): dedicated bridge
  network (never `network_mode: host`), no Docker socket mount, single data
  bind mount plus explicit share mounts, `security_opt:
  no-new-privileges:true`, `cap_drop: NET_RAW`, `tmpfs: /tmp`,
  `pids_limit: 256`, `mem_limit` between 256 and 8192 MB, `memswap_limit` equal
  to `mem_limit` (no swap spill-over), `cpus` from `cpu_limit` (0.1–8.0),
  `json-file` logs capped at 10 MB × 3.
- **Health check**: every container runs `python3 /opt/data/healthcheck.py`
  every 30 s (timeout 5 s, 3 retries, 60 s start period). The probe speaks the
  Hermes gateway control-socket protocol — one JSON line
  `{"verb":"identify","id":1,"protocol":1}` on `/opt/data/gateway.sock` — and
  exits 0 only when the gateway answers `ok: true`. `GET /api/instances`
  reports `health` (`healthy|unhealthy|starting|none`); an unhealthy container
  keeps its `running` status and gets a short `last_error`, so a crashlooping
  or deadlocked gateway is visible without restarting it.
- **Fleet caps**: optional `FLEETT_MAX_TOTAL_CPU` / `FLEETT_MAX_TOTAL_MEM_MB`
  bound the sum of every instance's limits. A create or update that would
  exceed one is refused with `409` and `error.details` `{used, requested,
  cap}`; an instance being patched is never counted twice.
- **UID/GID 10000 for every agent.** Data directories and share directories are
  chowned to `10000:10000` when the process runs as root (best effort
  otherwise). Shares are writable by every instance that mounts them, so
  isolation between guests relies on the container, not on the UID.
- **Input validation**: `id` and share names are `[a-z0-9-]`, 2–32 characters,
  must start with a letter or digit (Docker Compose project names cannot start
  with `-`); `mem_limit_mb` is 256..=8192; `cpu_limit` is 0.1..=8.0; `model` and
  `base_url` must be non-empty; `channel.type` is `telegram`, `discord` or
  `none`; a share must exist before it can be attached; `logs?tail` is
  1..=2000.
- **Template rendering** parses the rendered YAML before writing it, so a broken
  template fails with `422` instead of writing an invalid config. For instances
  created from a template, `data/config.yaml` is generated and manual edits are
  overwritten by the next `PATCH`. Instances adopted from disk (`template:
  "custom"`) use their own `data/config.yaml` as the template, so their edits
  survive a `PATCH`. `data/SOUL.md` is only written at create time.
- **Docker failures** are surfaced as `502` with the compose stderr truncated to
  2 KB.

## What is mocked

`FLEETT_DRIVER=mock` (the default) never calls Docker:

- instance state lives in memory (running/stopped/error/unknown), `up`, `stop`,
  `start`, `restart` and `down` only mutate that state;
- `ps` returns a synthetic container id (UUID) and image, `uptime_s` counts
  from the simulated start;
- `logs` returns a fixed set of `[mock]` lines, bounded by `tail`;
- `ping` always reports Docker as healthy.

Everything else is real: compose files, `config.yaml`, `SOUL.md`, the encrypted
secrets table, shares on disk, validation, auth, SQLite index and reconciliation.
The test suite uses `MockDriver` exclusively and never needs a Docker daemon.

## Tests

```bash
cargo fmt --all --check
cargo test --locked         # 54 unit + 31 integration tests
cargo clippy --all-targets  # must stay warning-free
```

Integration tests (`tests/api.rs`) build the real router with a temporary
`FLEETT_ROOT` and a `MockDriver`, then call it through
`tower::ServiceExt::oneshot`. They cover auth (401/204), health, templates,
create (files on disk, YAML validity, `0600` secrets, generated
`data/healthcheck.py`), duplicate ids, invalid ids and payloads (`mem_limit_mb`
and `cpu_limit` bounds), get/list (including `health` and `last_error` for an
unhealthy container), patch (compose rewrite, data preserved, token rotation,
CPU accounting that excludes the patched instance), fleet-wide CPU/memory caps
with `409` and `error.details`, start/stop/restart, logs, share
create/attach/detach/delete with the 409-while-attached rule, delete keep vs
purge, the generated compose shape (restart policy, external network, no host
networking, no Docker socket, share mounts, `cpus`, `memswap_limit`,
`healthcheck`) and driver failures (502 with truncated stderr). Unit tests also
run the health probe against a fake Unix socket, parse both `docker compose ps`
output shapes with `Health`, migrate a v0 database (including a deliberately
failing migration that must roll back), resolve the compose project name from
the instance file, and plan/rewrite the legacy `hermes-<id>` projects without
Docker.

## API notes

`API.md` is implemented literally. Additions (all optional, none required by the
contract):

- `GET /api/instances` now also returns `health`
  (`healthy|unhealthy|starting|none`), `cpu_limit` and `last_error`;
  `GET /api/instances/{id}` returns the same `health`/`cpu_limit`.
- `POST /api/instances` and `PATCH /api/instances/{id}` accept `cpu_limit`
  (default `1.0`, accepted range `0.1..=8.0`).
- Exceeding `FLEETT_MAX_TOTAL_CPU` or `FLEETT_MAX_TOTAL_MEM_MB` returns `409`
  (`cpu_cap_exceeded` / `mem_cap_exceeded`) with
  `error.details: {used, requested, cap}`.
- `POST /api/instances` accepts `gate_token` (stored as
  `HERMES_GATE_PROFILE_TOKEN`, and also under `api_key_env` when the two
  differ) and `env` (extra `KEY=VALUE` secrets, value names validated,
  newlines rejected). Secrets are encrypted; without a key configured a create
  that carries one returns `409 secrets_key_missing`.
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
