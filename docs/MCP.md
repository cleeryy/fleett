# fleett as an MCP server

`fleett mcp` exposes the fleet to an orchestrating agent (Hermes, Claude, a "fleet
captain" agent) through the [Model Context Protocol](https://modelcontextprotocol.io):
one tool per operation, no shell, no free-form command.

The MCP server is a **client of the fleett HTTP API**. Every tool maps to one API call made
with the token of `~/.config/fleett/config.toml` (or `FLEETT_URL` / `FLEETT_TOKEN`). It has no
database access, no Docker access and no privilege of its own:

- the API enforces the token role — a `viewer` token gets `403 forbidden` as soon as a tool
  maps to a mutation, and the MCP server returns that error with the API message;
- the API appends every successful **mutation** to `/api/audit` with the actor of the token
  (`bootstrap` for `FLEETT_TOKEN`, the token name for a named token), so an agent's actions
  stay attributable (the API does not audit reads);
- the MCP server never reads a secret value: tool results only carry what the API returns, and
  no API response contains a token or a secret.

Use a named token (`POST /api/tokens`) per orchestrator so the audit log names it. Give it
`viewer` for a read-only copilot and `admin` only to an orchestrator that must act.

## Configuration

The server uses the same configuration as every other client command:

```toml
# ~/.config/fleett/config.toml
[server]
url = "http://127.0.0.1:8090"
token = "flt_0123456789abcdef0123456789abcdef"
```

Environment variables win, field by field:

| Variable | Overrides |
|---|---|
| `FLEETT_URL` | `[server] url` |
| `FLEETT_TOKEN` | `[server] token` |

`HOME` locates the configuration file (`$HOME/.config/fleett/config.toml`); when both
environment variables are set the file may be absent. The token is optional only against a
server started with `FLEETT_ALLOW_NO_AUTH=1`.

`fleett mcp` ignores the global `--json` flag: MCP results have their own shape (see
[Results](#results)).

## Running it

```bash
# stdio, the usual MCP transport (one JSON-RPC message per line).
fleett mcp

# Streamable HTTP: POST /mcp on this address, one JSON response per request.
fleett mcp --http 127.0.0.1:8091
```

Diagnostics go to **stderr**; stdout only carries JSON-RPC responses, so a client can own the
pipe. The server exits when stdin reaches EOF (stdio mode).

### Hermes

Declare the server once in the Hermes configuration:

```yaml
mcp_servers:
  fleett:
    command: fleett
    args: ["mcp"]
    env:
      FLEETT_URL: http://127.0.0.1:8090
      FLEETT_TOKEN: flt_0123456789abcdef0123456789abcdef
```

A client that speaks Streamable HTTP URLs can use the HTTP mode instead:

```yaml
mcp_servers:
  fleett:
    url: http://127.0.0.1:8091/mcp
```

The client prefixes the tool names with the server name (`fleett_list_instances`,
`fleett_start_instance`, …); the exact separator is the client's choice.

The token can also be a **named token** restricted to `viewer`: the agent can then list,
inspect, read logs/files and follow git history, but every mutation returns an explicit
`forbidden` error instead of doing anything.

## Tools

Nineteen tools, each one strict JSON Schema (`additionalProperties: false`, no `cmd`/`command`
field). Required arguments are marked `*`.

| Tool | Arguments | API call | Role |
|---|---|---|---|
| `list_instances` | — | `GET /api/instances` | any |
| `get_instance` | `id`* | `GET /api/instances/{id}` | any |
| `create_instance` | `id`*, `name`, `template`, `model`, `base_url`, `api_key_env`, `provider`, `gate_token`, `channel`, `channel_token`, `allow_lan`, `cpu_limit`, `mem_limit_mb`, `notes` | `POST /api/instances` | admin |
| `start_instance` | `id`* | `POST /api/instances/{id}/start` | admin |
| `stop_instance` | `id`* | `POST /api/instances/{id}/stop` | admin |
| `restart_instance` | `id`* | `POST /api/instances/{id}/restart` | admin |
| `delete_instance` | `id`*, `purge` | `DELETE /api/instances/{id}[?purge=true]` | admin |
| `logs` | `id`*, `tail`, `since`, `grep`, `level` | `GET /api/instances/{id}/logs?format=json` | any |
| `env_set` | `id`*, `key`*, `value`*, `secret` | `PATCH /api/instances/{id}` (merge) | admin |
| `env_unset` | `id`*, `key`* | `PATCH /api/instances/{id}` (merge, `value: null`) | admin |
| `read_file` | `id`*, `path`* | `GET /api/instances/{id}/files/content` | any |
| `write_file` | `id`*, `path`*, `content`*, `create_parents` | `GET` then `PUT /api/instances/{id}/files/content` (`POST …/files/mkdir` when `create_parents`) | admin |
| `git_log` | `id`*, `path`, `limit` | `GET /api/instances/{id}/git/log` | any |
| `git_status` | `id`* | `GET /api/instances/{id}/git/status` | any |
| `git_revert` | `id`*, `commit`*, `paths`, `restart` | `POST /api/instances/{id}/git/revert` | admin |
| `list_providers` | — | `GET /api/providers` | any |
| `test_provider` | `id`* | `POST /api/providers/{id}/test` | admin |
| `usage` | — | `GET /api/usage` | any |
| `run_pipeline` | `pipeline_id`, `pipeline`, `wait` | — (not available yet) | — |

Constraints worth knowing:

- `id` matches `^[a-z0-9-]{2,32}$`; `path` is a virtual absolute path inside the instance
  data directory (`/SOUL.md`, `/notes/todo.md`).
- `channel` is `telegram`, `discord` or `none`; `channel_token` requires `channel` and is
  meaningless with `none`.
- `cpu_limit` is `0.1..=8.0`, `mem_limit_mb` is `256..=8192`, `tail` is `1..=2000`,
  `limit` (git log) is `1..=500`.
- `env_set` stores the value; a `secret: true` value is encrypted at rest and never returned
  by any tool. `env_unset` removes the variable and re-applies the instance.
- `write_file` is compare-and-swap: it reads the current `etag` first and sends it with the
  write. A concurrent edit fails with `etag_mismatch` (nothing overwritten). `create_parents`
  creates the missing directory chain one level at a time; only `.md .yaml .yml .json .txt
  .toml` files are editable, and generated files (`data/config.yaml`, `data/healthcheck.py`)
  are refused by the API.
- `run_pipeline` answers `not_available`: this build has no pipeline API (issue #31 is not
  merged). It never calls a made-up endpoint. Use the individual instance tools meanwhile.

The authoritative catalog is `tools/list`; the schemas are compiled into the binary. Example
of one schema:

```json
{
  "name": "create_instance",
  "description": "Create and start an instance. Secrets (gate_token, channel_token) are stored encrypted and never returned.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "id": { "type": "string", "pattern": "^[a-z0-9-]{2,32}$", "description": "Instance id." },
      "channel": { "type": "string", "enum": ["telegram", "discord", "none"] },
      "cpu_limit": { "type": "number", "minimum": 0.1, "maximum": 8.0 },
      "mem_limit_mb": { "type": "integer", "minimum": 256, "maximum": 8192 }
    },
    "required": ["id"],
    "additionalProperties": false
  }
}
```

## Results

A successful `tools/call` returns the MCP standard shape:

```json
{
  "content": [{ "type": "text", "text": "alice  status=running health=healthy ..." }],
  "structuredContent": [{ "id": "alice", "status": "running", "...": "..." }]
}
```

- `content[0].text` is a compact human/agent-readable rendering (no ANSI, no table borders).
- `structuredContent` is the API JSON when the API returned JSON; for tools that read a raw
  body (logs, file contents) it carries the metadata and, when small enough, the body.
- Huge textual bodies (logs, file contents) are cut at **64 KiB** on a UTF-8 boundary with an
  explicit `… [truncated: N of M bytes shown]` marker; `structuredContent.truncated` is then
  `true` and the raw body is omitted. The full file stays available through
  `GET /api/instances/{id}/files/content`; logs can be narrowed with `tail`, `since` and
  `grep`.

Execution failures are **tool errors**, not protocol errors: the result has `isError: true`
and a `structuredContent.error` object carrying the API code:

```json
{
  "content": [{ "type": "text", "text": "fleett: the viewer role is read-only (code: forbidden)" }],
  "structuredContent": {
    "ok": false,
    "error": { "code": "forbidden", "status": 403, "message": "the viewer role is read-only" }
  },
  "isError": true
}
```

That is how a `viewer` token calling `start_instance` is refused: the `403` is the API's,
the message is the API's, and nothing was started. `not_available` (pipeline) and
`invalid_arguments` (a missing/unknown tool argument) use the same shape.

Protocol-level errors are JSON-RPC error objects: `-32700` parse error, `-32600` invalid
request, `-32601` unknown method, `-32602` unknown tool or malformed `tools/call`. The server
implements `initialize`, `ping`, `tools/list`, `tools/call` and accepts
`notifications/initialized`.

## Streamable HTTP mode

```bash
fleett mcp --http 127.0.0.1:8091
curl -sS http://127.0.0.1:8091/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

- `POST /mcp` carries **one** JSON-RPC message and returns **one** JSON response.
- A notification (`notifications/initialized`, no `id`) answers `202 Accepted` with an empty
  body.
- The endpoint is stateless: no `Mcp-Session-Id`, no per-session state, no server-initiated
  messages.
- **SSE streaming is not implemented.** `GET /mcp` and `DELETE /mcp` answer `405` with
  `{"error":{"code":"sse_not_implemented"}}`.
- JSON-RPC batches are rejected (`-32600`): send one message per request.
- The HTTP endpoint has **no authentication of its own**: whoever reaches the port acts with
  the token of the server process. Bind `127.0.0.1` (or a private interface) and put an
  authenticating reverse proxy in front of it when remote agents need it. Prefer stdio when
  the orchestrator runs on the same host.
- TLS is not terminated here; use a reverse proxy.

## Limitations

- Tools only: no resources, prompts, sampling, logging notifications or completions.
- No SSE, no streaming, no server-initiated requests; HTTP is stateless.
- `run_pipeline` waits for the pipeline API of issue #31 (`not_available`).
- The MCP server does not cache: every call is a live API call, and it counts against the API
  like any other client. Logs above 64 KiB are truncated (see above).
- The MCP server cannot elevate a token: if the token cannot do it over `curl`, the tool
  cannot do it either.

## Verifying an install

```bash
# 1. The client configuration works:
fleett doctor

# 2. The handshake answers (two lines: request then response):
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' | fleett mcp

# 3. A viewer token is refused on a mutation:
FLEETT_TOKEN=flt_<viewer> fleett mcp   # tools/call start_instance → isError, code "forbidden"

# 4. The mutation is in the audit log (reads are not audited by the API):
curl -sS -H "Authorization: Bearer $FLEETT_TOKEN" \
  'http://127.0.0.1:8090/api/audit?limit=5' | jq '.[] | {actor, action, target}'
```
