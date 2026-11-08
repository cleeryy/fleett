# fleett — API v1

Base: `http://127.0.0.1:8090`. Every `/api/*` route requires a bearer token
(`Authorization: Bearer <FLEETT_TOKEN>`) or, when `FLEETT_AUTH` includes
`oidc`, a `fleett_session` cookie, except `GET /api/health` and
`GET /api/auth/config`. See `docs/OIDC.md` for the SSO setup.

Errors: `{"error":{"code":"...","message":"..."}}` with the matching HTTP status
(`400` invalid input, `401` missing/wrong credential, `403` role or CSRF
refusal, `404` unknown, `409` conflict, `422` invalid YAML/compose,
`502` Docker or provider failure). Unauthenticated API calls never redirect:
`401` JSON, always.

Credentials are resolved in this order: session cookie, then bearer token
(`FLEETT_TOKEN` = admin, then named tokens). `viewer` tokens and sessions may
only `GET`/`HEAD`; a mutation returns `403 forbidden`. A cookie-authenticated
mutation must also echo the `fleett_csrf` cookie in `X-Fleett-CSRF`, otherwise
`403 csrf_required`; bearer clients are exempt.

## Health and auth

| Method | Route | Response |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.2.0","driver":"mock\|compose","docker":{"ok":true}}` |
| GET | `/api/auth/config` | `{"mode":"bearer\|oidc\|both","oidc":{"enabled":true,"login_url":"/auth/login"},"bearer":{"enabled":true}}` |
| GET | `/api/auth/check` | `204` when the credential is valid |
| GET | `/api/auth/me` | `{"authenticated":true,"actor":"…","role":"admin\|viewer","method":"session\|bearer","user":{"sub":"…","email":"…","name":"…"}}` (`user` only for sessions) |
| GET | `/auth/login` | `303` to the IdP; `404` when OIDC is disabled |
| GET | `/auth/callback` | `303` to `/` and sets `fleett_session` + `fleett_csrf`; `400 invalid_state`, `401 oidc_failed`, `403 forbidden` |
| POST | `/auth/logout` | `204`, clears the fleett cookies |

## Audit and tokens

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/audit?limit=200` | — | `[{"id":1,"at":"…","actor":"…","role":"admin","method":"session","action":"instance.create","target":"alice","ip":"…","details":{…}}]`, newest first, admin only |
| GET | `/api/tokens` | — | `[{"id":"…","name":"ci","role":"viewer","created_at":"…","revoked_at":null}]`, never the secret, admin only |
| POST | `/api/tokens` | `{"name":"ci","role":"viewer\|admin"}` | `201` with the one-time `token` (`flt_<32 hex>`), admin only |
| DELETE | `/api/tokens/{id}` | — | `204` (sets `revoked_at`), `404` unknown, admin only |

`role` defaults to `viewer`. Revoking is idempotent. Tokens are stored as a
SHA-256 digest only.

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
 "allow_lan":false,"shares":["findings"],"last_error":null,
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
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,"notes":""}
```
`id`: `[a-z0-9-]{2,32}`, unique. `channel.type`: `telegram`, `discord` or `none`.
The token is **never** returned by the API.

`GET /api/instances/{id}` → `InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN","channel":"telegram",
 "status":"running","shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,
 "warnings":[],"notes":"","created_at":"...","updated_at":"...",
 "last_error":null,"data_dir":"/opt/fleett/instances/alice/data",
 "env_keys":["HERMES_GATE_PROFILE_TOKEN","TELEGRAM_BOT_TOKEN"],
 "container":{"id":"3f2a…","image":"hermes-agent:0.22.2-local","uptime_s":3600},
 "compose_yaml":"name: fleett-alice\n…"}
```

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Editable fields: `name`, `model`,
`base_url`, `api_key_env`, `channel` (`token` optional = rotation), `shares`,
`mem_limit_mb`, `cpu_limit`, `allow_lan`, `notes`. Rewrites the compose file and runs `up -d`.

`cpu_limit` is a float in `0.1..=8.0` (default `1.0`), rendered as `cpus` plus
`memswap_limit == mem_limit`. Optional fleet caps (`FLEETT_MAX_TOTAL_CPU`,
`FLEETT_MAX_TOTAL_MEM_MB`) return `409` with `error.details {used, requested, cap}`.

### LAN access and egress enforcement

`allow_lan` is `false` by default: the instance only joins the restricted `fleett` network.
Setting it to `true` also attaches `fleett-lan` (created on demand), which bypasses the host
egress firewall and gives the agent LAN access; the detail then carries a warning in
`warnings`. `docs/EGRESS.md` documents the firewall itself.

With `FLEETT_EGRESS=restrict` (the default), creating or patching an instance that stays on
the restricted network requires the marker written by `sudo fleett egress install`, whose
subnet must match the current `fleett` network. Otherwise the request fails with
`409 egress_not_enforced` (`error.details {egress, marker}`; the message points at the
command to run). `FLEETT_EGRESS=off` disables the check and adds a warning to `warnings`.
Start/stop/restart/delete are never blocked by this policy.

`warnings` is a list of operator-facing strings (empty when there is nothing to report).

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
