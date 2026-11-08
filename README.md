# hermes-fleet

Web console to create, configure, start/stop and delete Hermes agents, each in its own
Docker container — without ever touching the existing Hermes instance.

**Status: under construction.** See `SPEC.md` (architecture) and `API.md` (HTTP contract).

## Layout

```
backend/        Rust control plane (axum, SQLite, Compose driver)
web/            React console (Vite + Tailwind), built into web/dist
docs/DEPLOY.md  deployment runbook
```

## Development

```bash
# backend — mock driver by default, no Docker calls
cd backend && cargo run

# frontend — mock API by default
cd web && pnpm install && pnpm dev
```

The backend serves `web/dist` when it exists, so a production build is a single process.
