# hermes-fleet — API v1

Base: `http://127.0.0.1:8090`. Every `/api/*` route requires
`Authorization: Bearer <FLEET_TOKEN>`, except `GET /api/health`.

Errors: `{"error":{"code":"...","message":"..."}}` with the matching HTTP status
(`400` invalid input, `401` missing/wrong token, `404` unknown, `409` conflict,
`422` invalid YAML/compose, `502` Docker failure).

## Health and auth

| Method | Route | Response |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.1.0","driver":"mock\|compose","docker":{"ok":true}}` |
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
 "channel":"telegram","status":"running","shares":["findings"],
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
 "mem_limit_mb":1536,"notes":""}
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
 "last_error":null,"data_dir":"/opt/hermes-fleet/instances/alice/data",
 "env_keys":["HERMES_GATE_PROFILE_TOKEN","TELEGRAM_BOT_TOKEN"],
 "container":{"id":"3f2a…","image":"hermes-agent:0.22.2-local","uptime_s":3600},
 "compose_yaml":"name: hermes-alice\n…"}
```

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Editable fields: `name`, `model`,
`base_url`, `api_key_env`, `channel` (`token` optional = rotation), `shares`,
`mem_limit_mb`, `notes`. Rewrites the compose file and runs `up -d`.

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
