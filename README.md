# hermes-fleet

Web console to create, configure, start/stop and delete Hermes agents, each in its own
Docker container. Your existing Hermes instance is never touched.

## What it does

- create an instance from a template (`guest`, `guest-discord`, `solo`) with model, base URL,
  provider token, chat channel and shared mounts
- start, stop, restart and delete instances (data kept or purged)
- read logs, the generated Compose file and the runtime state
- share a directory between instances (`rw` or `ro`) for findings and knowledge
- agents keep running when the console is stopped, updated or deleted: Docker owns them

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/cleeryy/hermes-fleet/master/install.sh | bash
hermes-fleet                           # reads ~/.hermes-fleet/fleet.env
# open http://127.0.0.1:8090
```

Linux (static musl, x86_64/aarch64) and macOS (arm64/x86_64). Details: `docs/INSTALL.md`,
deployment as a service: `docs/DEPLOY.md`.

## Architecture

One instance = one Docker Compose project written to disk
(`instances/<id>/compose.yaml`), driven by `docker compose -p hermes-<id>`. The control plane
only writes files and calls Compose; it supervises no process. `restart: unless-stopped`
means agents survive a console restart, an update, or the console being deleted.

The SQLite database is a rebuildable index; `instances/` and `shares/` are the source of
truth. See `SPEC.md` for the decisions and `API.md` for the HTTP contract.

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
