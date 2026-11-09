# MCP server marketplace

`fleett mcp` (see [MCP.md](MCP.md)) exposes **fleett itself** as an MCP server
for an orchestrating agent. This page documents the opposite direction: fleett
**deploys and supervises MCP servers** and attaches them to agents.

An MCP server is a service, not a file. A `streamable-http` (or `sse`) server is
a container fleett owns; a `stdio` server is a command the agent launches
itself and only appears in the agent configuration.

> Distinguish the two commands: `fleett mcp` = fleett as an MCP **server**;
> `fleett mcp-server` = manage MCP servers **for** agents.

## Concepts

| Field | Meaning |
|---|---|
| `id` | Slug (`[a-z0-9-]{2,32}`), the Compose project is `fleett-mcp-<id>`. |
| `kind` | `streamable-http` or `sse` (deployed by fleett), `stdio` (config-only). |
| `image` | Container image of a deployed server; validated, never a registry credential. |
| `command` | Argv: the complete command of a stdio server, or an optional container command. |
| `port` / `path` | Where the MCP endpoint listens in the container (`8000` + `/mcp` by default). |
| `persist` | Mount `./data:/data`; the only allowed volume. |
| `cpu_limit` / `mem_limit_mb` | Quotas; `256 MB` and `0.5` CPU by default. |
| `env` | Variables of the server, encrypted at rest for `secret` rows. |
| `headers` | Optional HTTP authentication headers, encrypted and never returned. |
| `tools` | Discovery cache from the last successful `tools/list`. |

Statuses reuse the instance vocabulary: `created`, `running`, `stopped`,
`error`, `unknown`; health is `healthy`, `unhealthy`, `starting` or `none`.

## Security and network model

- **No published port.** The compose document has no `ports:` field, so the
  server is reachable only from the shared external `fleett` Docker network.
  `curl http://127.0.0.1:<port>` from the host (or from the LAN) is refused.
- **Hardened by construction.** `read_only: true`, `cap_drop: [ALL]`,
  `security_opt: [no-new-privileges:true]`, `tmpfs: [/tmp]`, `pids_limit: 128`,
  a memory swap limit equal to the memory limit, and capped `json-file` logs
  (`10m` × 3). The API has no field for `privileged`, `network_mode`, `pid`,
  `devices` or arbitrary volumes, so a request cannot ask for them.
- **Secrets stay server-side.** Environment values live in `mcp_server_env`
  (AES-256-GCM, one row per key). HTTP headers live in the encrypted
  `headers_nonce/ciphertext` columns of `mcp_servers`. The API returns only
  `{key, secret, set}` views and header **names**. The decrypted values are
  injected into the `docker compose` child process, never written into
  `compose.yaml`, and never logged.
- **Attach by name.** The agent configuration points at
  `http://fleett-mcp-<id>:<port><path>` (Docker DNS). Two agents that attach the
  same server share the one container: no second deployment.

The control plane itself is not on the `fleett` network, so discovery and
supervision probe the container IP from `docker inspect`
(`NetworkSettings.Networks.*.IPAddress`) instead of the Docker DNS name.

## Catalogue

`GET /api/mcp/catalog?q=` merges:

1. **Built-in entries**: `fetch`, `filesystem`, `memory`, `github`, `time`.
2. The **local index** at `$FLEETT_ROOT/marketplace/mcp/index.json`:

```json
{
  "version": 1,
  "entries": [
    {
      "id": "custom",
      "name": "Custom",
      "description": "What it does",
      "kind": "streamable-http",
      "image": "example/custom:1",
      "command": [],
      "port": 8000,
      "path": "/mcp",
      "env": [{"key": "API_KEY", "secret": true, "required": true}],
      "tags": ["custom"]
    }
  ]
}
```

Import a manifest with `POST /api/mcp/catalog/import` (admin) or
`fleett mcp-server import --file manifest.json`. A duplicate id answers
`409 mcp_duplicate`. `DELETE /api/mcp/catalog/{id}` removes an imported entry
only; a built-in answers `400 invalid_mcp_server`. On read, an entry of the
local index whose id shadows a built-in wins (an operator can hand-edit the
index to pin another image), but the import endpoint refuses an id that already
exists, built-in included, and a built-in id can never be deleted.

Deploy a catalogue entry with `{"catalog":"fetch"}`; every field of the entry
can be overridden (`port`, `path`, `persist`, `cpu_limit`, `mem_limit_mb`,
`name`, `host`).

### Guide before deploying

A deployed server is third-party code. The API exposes the image, the command,
the required variables and the discovered tools **before** the attach step.
`POST /api/mcp/servers/{id}/test` runs `initialize` + `tools/list` and returns
the latency, the tool count and the tool names, so a broken image is obvious
before an agent depends on it.

## Deployment

```bash
fleett mcp-server catalog
fleett mcp-server deploy fetch
fleett mcp-server ls
fleett mcp-server test fetch
```

Deployment writes `$FLEETT_ROOT/mcp/<id>/compose.yaml`, ensures the `fleett`
network and the image on the target host, then runs
`docker compose -f <file> -p fleett-mcp-<id> up -d`. The rendered document looks
like:

```yaml
name: fleett-mcp-fetch
services:
  mcp:
    image: mcp/fetch:latest
    container_name: fleett-mcp-fetch
    restart: unless-stopped
    environment: {}
    networks: [fleet]
    security_opt: [no-new-privileges:true]
    cap_drop: [ALL]
    read_only: true
    tmpfs: [/tmp]
    pids_limit: 128
    mem_limit: 256m
    memswap_limit: 256m
    cpus: "0.5"
    logging: {driver: json-file, options: {max-size: 10m, max-file: "3"}}
networks:
  fleet:
    external: true
    name: fleett
```

Lifecycle: `POST /api/mcp/servers/{id}/start|stop|restart` (admin). `DELETE`
removes the project; `?purge=true` also deletes the compose file and the data
directory. A server that is attached to at least one agent cannot be deleted:
`409 mcp_server_in_use` with `details.instances`.

No Compose `healthcheck:` is written: it would need a shell inside an arbitrary
third-party image. The control plane probes the container port itself (see
[Supervision](#supervision)), which is the same signal `docker ps` would report
and works for an image with no shell.

## Discovery

After a deploy or a start, fleett sends:

1. `initialize` (`protocolVersion: 2025-06-18`, `clientInfo: fleett`),
2. `notifications/initialized`,
3. `tools/list` (following `nextCursor`, bounded to 10 pages / 2 000 tools).

The transport is Streamable HTTP: `POST` with
`Accept: application/json, text/event-stream`; the `Mcp-Session-Id` response
header is carried on the next calls, and `MCP-Protocol-Version` is sent after
initialization. Both a plain JSON body and an SSE-framed body are decoded; the
response body is capped at 4 MiB and every call times out after
`FLEETT_MCP_CALL_TIMEOUT_S` (default 15 s).

The result is cached in `tools_json` / `tools_count` / `tools_hash`
(SHA-256 of the canonical JSON) and returned by `GET …/{id}/tools`. A failed
discovery marks the server `error` / `unhealthy` and is reported by
`POST …/{id}/test` as `422 mcp_unreachable` with the upstream detail.

> The client implements the **Streamable-HTTP** transport. A legacy `sse`
> server can still be deployed and supervised (the supervision probe is a TCP
> connect), but `test`/discovery require it to accept a `POST` JSON-RPC
> `initialize` on its `path`; otherwise the endpoint reports
> `422 mcp_unreachable`. Prefer `streamable-http` for new servers.

## Attaching to an agent

```bash
fleett mcp-server attach alice fetch --mode readonly
fleett mcp-server detach alice fetch
```

`PUT /api/instances/{id}/mcp-servers` replaces the whole list;
`POST/DELETE /api/instances/{id}/mcp-servers/{server}` attaches or detaches one.
Attaching rewrites the managed `mcp_servers` block of the instance
`config.yaml`, writes the instance files, and recreates the agent through the
same `apply_compose` path as any other change (the API returns the resulting
status). The audit trail records `instance.mcp.attach` / `instance.mcp.detach`.

Managed keys are prefixed `fleett-<server>` and stripped before every
re-insertion, so an operator-written entry survives a PATCH and a PATCH never
drops the attachments. Without an attachment the block is absent, keeping the
historical bytes of the template.

HTTP entry:

```yaml
mcp_servers:
  fleett-fetch:
    url: http://fleett-mcp-fetch:8000/mcp
    timeout: 15
    connect_timeout: 5
```

`timeout`/`connect_timeout` are seconds; they are set from
`FLEETT_MCP_CALL_TIMEOUT_S` (connect timeout capped at 5 s). The `readonly`
mode is stored in `instance_mcp_servers` and shown by the API; the Hermes
configuration has no per-server mode field, so the enforcement of a read-only
attachment is the server's own tool allowlist (documented limitation).

Stdio entry:

```yaml
mcp_servers:
  fleett-fs-stdio:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/data"]
    env:
      FS_TOKEN: "${FS_TOKEN}"
```

The environment values are referenced as `${KEY}`; the agent process must
already receive those variables (the instance environment, or the server
`env` rows when the same key is exported by the agent). fleett never writes the
value into `config.yaml`.

HTTP authentication headers are kept server-side for probing and discovery.
Hermes' current `mcp_servers` schema has no header map, so they are **not**
written into the agent configuration.

## Supervision

`FLEETT_MCP_SUPERVISE_INTERVAL_S` (default 60, `0` disables the loop) controls a
single background loop started by `fleett serve`. Each tick, for every deployed
server:

1. `docker compose ps` + `docker inspect` (state, restart count, IP);
2. a bounded TCP probe of the container IP on the configured port (3 s);
3. update `status`, `health`, `last_error`, `last_probe_at`;
4. open one `mcp_incidents` row per cause (`unreachable` when the container is
   not running, `unhealthy` when it runs but refuses the port) or close every
   open incident once the probe answers again.

Unlike the watchdog, MCP supervision never restarts a container on its own:
`restart: unless-stopped` covers a crash, and an operator decides whether a
broken third-party image should be restarted.

`GET /api/mcp/servers/{id}/incidents` lists the journal;
`POST /api/mcp-incidents/{id}/close` closes one (admin).

## Metrics

| Series | Labels | Meaning |
|---|---|---|
| `fleett_mcp_up` | `server` | `1` when running and the probe answers. |
| `fleett_mcp_tools` | `server` | Tools found by the last successful `tools/list`. |
| `fleett_mcp_servers` | `status`, `health` | Number of servers per state (seeded series at `0`). |
| `fleett_mcp_probes_total` | `result` (`ok`, `error`) | Supervision probes. |

## CLI

| Command | Effect |
|---|---|
| `fleett mcp-server ls` | List servers, tools, attached agents and endpoint. |
| `fleett mcp-server get <id>` | Detail, env variable names and attachments. |
| `fleett mcp-server catalog [--q TEXT]` | Browse the merged catalogue. |
| `fleett mcp-server import --file manifest.json` | Import one entry. |
| `fleett mcp-server deploy <id> [--catalog ID] [--image TAG] [--env K=V] [--secret K=V] [--header N=V] [--port N] [--persist]` | Create, deploy and discover. |
| `fleett mcp-server start\|stop\|restart <id>` | Lifecycle. |
| `fleett mcp-server rm <id> [--purge]` | Delete (refused while attached). |
| `fleett mcp-server logs <id> [--tail N]` | Container logs. |
| `fleett mcp-server test <id>` | `tools/list`, latency and tools. |
| `fleett mcp-server tools <id>` | Cached tools. |
| `fleett mcp-server attach <instance> <server> [--mode readonly\|rw]` | Attach and recreate. |
| `fleett mcp-server detach <instance> <server>` | Detach and recreate. |

## Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| Deploy answers `502 mcp_upstream` | The image pull or `docker compose up` failed; the upstream stderr is in the message. The server row stays `error`, retry with `POST …/{id}/start`. |
| `test` answers `422 mcp_unreachable` | The container has no IP yet, crashed, or the `port`/`path` do not match what the image serves. Check `GET …/{id}/logs`. |
| `health: unhealthy` but the container runs | The port refuses connections: wrong `port`, or the server needs an authentication header that is missing. `GET …/{id}/tools` shows the last successful discovery. |
| Attach succeeds but the agent sees no tools | Check the agent logs: the managed block is in `data/config.yaml`, the endpoint uses Docker DNS, and the agent must be on the shared `fleett` network (it always is). |
| `409 mcp_server_in_use` | Detach every agent listed in `details.instances` first. |
| Agent config lost a hand-written entry | Only non-managed keys are preserved; a key starting with `fleett-` is owned by the attachment table. |
| `curl` from the LAN answers | The server should not be reachable: check that the compose file has no `ports:` and that the project name is `fleett-mcp-<id>`. |

## Configuration flags

| Variable | Default | Effect |
|---|---|---|
| `FLEETT_MCP_CALL_TIMEOUT_S` | `15` | Per-call timeout of discovery/probe requests (clamped `1..=300`). |
| `FLEETT_MCP_SUPERVISE_INTERVAL_S` | `60` | Supervision period; `0` disables the loop. |

## API summary

All routes are authenticated. Reads accept any role; every mutation requires
`admin`. Errors: `400 invalid_mcp_server`, `404 not_found`,
`409 mcp_server_in_use` / `409 mcp_duplicate`, `422 mcp_unreachable`,
`502 mcp_upstream`.

| Method | Route |
|---|---|
| GET | `/api/mcp/catalog?q=` |
| POST | `/api/mcp/catalog/import` |
| DELETE | `/api/mcp/catalog/{id}` |
| GET/POST | `/api/mcp/servers` |
| GET/PATCH/DELETE | `/api/mcp/servers/{id}` |
| POST | `/api/mcp/servers/{id}/start\|stop\|restart\|test` |
| GET | `/api/mcp/servers/{id}/logs` |
| GET | `/api/mcp/servers/{id}/tools` |
| PUT | `/api/mcp/servers/{id}/env` |
| DELETE | `/api/mcp/servers/{id}/env/{key}` |
| GET | `/api/mcp/servers/{id}/incidents` |
| POST | `/api/mcp-incidents/{id}/close` |
| GET/PUT | `/api/instances/{id}/mcp-servers` |
| POST/DELETE | `/api/instances/{id}/mcp-servers/{server}` |
