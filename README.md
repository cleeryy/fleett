# fleett

Web console to create, configure, start/stop and delete Hermes agents, each in its own
Docker container. Your existing Hermes instance is never touched.

`fleett` is the control plane; the agents it manages are still Hermes agents. The project
was renamed from `hermes-fleet` in v0.2.0 — see [Upgrading from `hermes-fleet`](#upgrading-from-hermes-fleet).

## What it does

- create an instance from a template (`guest`, `guest-discord`, `solo`) with model, base URL,
  provider token, chat channel and shared mounts
- start, stop, restart and delete instances (data kept or purged)
- read logs, the generated Compose file and the runtime state
- share a directory between instances (`rw` or `ro`) for findings and knowledge
- agents keep running when the console is stopped, updated or deleted: Docker owns them
- sign in with SSO (OIDC/Authentik) or a bearer token, with `admin`/`viewer` roles, CSRF
  protection, named revocable API tokens and an audit log — see `docs/OIDC.md`
- export one instance as a portable `.tgz` and import it under another id (same data, no
  secrets), and run rotating host backups with `fleett backup` — see `docs/BACKUP.md`
- drive everything from a terminal: `fleett ls`, `ps`, `logs`, `create`, `start`/`stop`/
  `restart`, `delete`, `usage`, `doctor` — see `docs/CLI.md`

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/cleeryy/fleett/master/install.sh | bash
fleett                           # reads ~/.fleett/fleett.env
# open http://127.0.0.1:8090
```

Linux (static musl, x86_64/aarch64) and macOS (arm64/x86_64). The installer also creates a
`hermes-fleet` symlink for scripts written against the old name. Details: `docs/INSTALL.md`,
deployment as a service: `docs/DEPLOY.md`, encrypted secrets: `docs/SECRETS.md`,
backups and instance archives: `docs/BACKUP.md`.

## Architecture

One instance = one Docker Compose project written to disk
(`instances/<id>/compose.yaml`), driven by `docker compose -p fleett-<id>`. The project name
is read back from the compose file, so projects created by the previous name
(`hermes-<id>`) stay manageable without being recreated. The control plane only writes files
and calls Compose; it supervises no process. `restart: unless-stopped` means agents survive a
console restart, an update, or the console being deleted.

The SQLite database is a rebuildable index; `instances/` and `shares/` are the source of
truth. See `SPEC.md` for the decisions and `API.md` for the HTTP contract.

## Upgrading from `hermes-fleet`

v0.2.0 keeps the old installation working:

- environment variables: `FLEETT_*` is read first, `FLEET_*` is still accepted, and the
  installer also accepts `HERMES_FLEET_*`
- data root: `~/.fleett`; when it does not exist, an existing `~/.hermes-fleet` is adopted
  (and logged once)
- environment file: `fleett.env` first, then `fleet.env`
- compose projects: the driver uses the `name:` declared in each `compose.yaml`, and updates
  write that same name back, so `hermes-<id>` instances keep starting, stopping, serving logs
  and accepting configuration changes without ever being duplicated
- the database file is still `fleet.db`, and `/opt/data`, `HERMES_UID`/`HERMES_GID` and the
  `hermes-agent` image are unchanged

Renaming the projects themselves is opt-in:

```bash
fleett migrate --from hermes-fleet            # print the plan, change nothing
fleett migrate --from hermes-fleet --apply    # down, rewrite, up (running instances only)
```

The rewrite goes through `serde_yaml`, not string replacement, and the plan is also available
without Docker. Running instances are restarted as `fleett-<id>`; stopped ones are only
rewritten. While legacy projects exist, a single warning at startup points at this command.

## Development

```bash
cd backend && cargo run          # mock driver by default, no Docker calls
cd web && pnpm install && pnpm dev
```

Build a single binary with the UI embedded:

```bash
cd web && pnpm build
cd ../backend && cargo build --release --features embed-web
```
