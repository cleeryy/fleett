# fleett — API v1

Base: `http://127.0.0.1:8090`. Every `/api/*` route requires
`Authorization: Bearer <FLEETT_TOKEN>`, except `GET /api/health`.

Errors: `{"error":{"code":"...","message":"..."}}` with the matching HTTP status
(`400` invalid input, `401` missing/wrong token, `404` unknown, `409` conflict,
`422` invalid YAML/compose, `502` Docker failure).

## Health and auth

| Method | Route | Response |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.2.0","driver":"mock\|compose","docker":{"ok":true}}` |
| GET | `/api/auth/check` | `204` when the token is valid |

## Templates

`GET /api/templates`
```json
[{"id":"guest","name":"Guest","description":"Web + memory, no infra access",
  "default_model":"deepseek-v4.1-flash","channel":"telegram"}]
```

## Instances

`GET /api/instances` → `InstanceSummary[]`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "channel":"telegram","status":"running","health":"healthy","cpu_limit":1.0,
 "shares":["findings"],"last_error":null,
 "created_at":"2026-11-08T01:00:00Z"}
```

`POST /api/instances` → `201 InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest",
 "model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN",
 "channel":{"type":"telegram","token":"123:ABC"},
 "shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"notes":""}
```
`id`: `[a-z0-9-]{2,32}`, unique. `channel.type`: `telegram`, `discord` or `none`.
The token is **never** returned by the API.

`GET /api/instances/{id}` → `InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN","channel":"telegram",
 "status":"running","shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"notes":"","created_at":"...","updated_at":"...",
 "last_error":null,"data_dir":"/opt/fleett/instances/alice/data",
 "env_keys":["HERMES_GATE_PROFILE_TOKEN","TELEGRAM_BOT_TOKEN"],
 "container":{"id":"3f2a…","image":"hermes-agent:0.22.2-local","uptime_s":3600},
 "compose_yaml":"name: fleett-alice\n…"}
```

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Editable fields: `name`, `model`,
`base_url`, `api_key_env`, `channel` (`token` optional = rotation), `shares`,
`mem_limit_mb`, `cpu_limit`, `notes`. Rewrites the compose file and runs `up -d`.

`cpu_limit` is a float in `0.1..=8.0` (default `1.0`), rendered as `cpus` plus
`memswap_limit == mem_limit`. Optional fleet caps (`FLEETT_MAX_TOTAL_CPU`,
`FLEETT_MAX_TOTAL_MEM_MB`) return `409` with `error.details {used, requested, cap}`.

`POST /api/instances/{id}/start|stop|restart` → `200 {"status":"running|stopped"}`
`DELETE /api/instances/{id}` → `204` (data preserved) · `?purge=true` erases `data/`.
`GET /api/instances/{id}/logs?tail=200` → `text/plain`

## Shares

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/shares` | — | `[{"name":"findings","path":"…","instances":[{"id":"alice","mode":"rw"}]}]` |
| POST | `/api/shares` | `{"name":"findings"}` | `201` |
| DELETE | `/api/shares/{name}` | — | `204` · `409` when still attached |
| POST | `/api/shares/{name}/attach` | `{"instance":"alice","mode":"rw\|ro"}` | `204` |
| POST | `/api/shares/{name}/detach` | `{"instance":"alice"}` | `204` |

## Statuses

`created` · `running` · `stopped` · `error` · `unknown`

`status` is a cache: the authoritative value is read through `docker compose ps` on every
`GET /api/instances` (driver `compose`). Under the `mock` driver the state is simulated.

`health` is read from the container healthcheck (`healthy`, `unhealthy`, `starting`, `none`).
The generated compose probes `/opt/data/gateway.sock` with the gateway's `identify` verb;
`last_error` is set while the probe is unhealthy and cleared when it recovers.
