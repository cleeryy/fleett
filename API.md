# fleett — API v1

Base: `http://127.0.0.1:8090`. Every `/api/*` route requires a bearer token
(`Authorization: Bearer <FLEETT_TOKEN>`) or, when `FLEETT_AUTH` includes
`oidc`, a `fleett_session` cookie, except `GET /api/health` and
`GET /api/auth/config`. See `docs/OIDC.md` for the SSO setup.

Errors: `{"error":{"code":"...","message":"..."}}` with the matching HTTP status
(`400` invalid input, `401` missing/wrong credential, `403` role or CSRF
refusal, `404` unknown, `409` conflict, `422` invalid YAML/compose,
`429` rate limit or authentication lockout, `502` Docker or provider failure).
A `429` always carries `Retry-After` (whole seconds) and
`details.retry_after_s`. Unauthenticated API calls never redirect: `401` JSON,
always.

Credentials are resolved in this order: session cookie, then bearer token
(`FLEETT_TOKEN` = admin, then named tokens). `viewer` tokens and sessions may
only `GET`/`HEAD`; a mutation returns `403 forbidden`. A cookie-authenticated
mutation must also echo the `fleett_csrf` cookie in `X-Fleett-CSRF`, otherwise
`403 csrf_required`; bearer clients are exempt.

## Health and auth

| Method | Route | Response |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.3.0","driver":"mock\|compose","docker":{"ok":true},"uptime_s":42,"secrets":{"key":true,"key_error":null,"encrypted":2,"plaintext":0},"egress":{"mode":"restrict\|off","marker":true}}` |
| GET | `/api/auth/config` | `{"mode":"bearer\|oidc\|both","oidc":{"enabled":true,"login_url":"/auth/login"},"bearer":{"enabled":true}}` |
| GET | `/api/auth/check` | `204` when the credential is valid |
| GET | `/api/auth/me` | `{"authenticated":true,"actor":"…","role":"admin\|viewer","method":"session\|bearer","user":{"sub":"…","email":"…","name":"…"}}` (`user` only for sessions) |
| GET | `/auth/login` | `303` to the IdP; `404` when OIDC is disabled |
| GET | `/auth/callback` | `303` to `/` and sets `fleett_session` + `fleett_csrf`; failures carry `details.reason` (`cookie_missing`, `state_mismatch`, `expired`). Browsers (`Accept: text/html`) get an HTML page with a retry link, and a missing state cookie redirects to `/auth/login`; API clients keep JSON `400`/`401`/`403`. State lifetime: `FLEETT_OIDC_STATE_TTL_SECS` (default 600) |
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

## Security

Every `/api/*` request is rate-limited per `(client IP, credential, route class)`
and every failed authentication feeds a progressive lockout; see
[HARDENING.md](docs/HARDENING.md#api-and-control-plane-hardening) for the
implementation and the environment variables. The limits apply before the route
handler, so an invalid credential is always a constant `401`, never a `404` that
would reveal whether an instance exists.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/security/status` | — | `{"rate_limit":{"enabled":true,"limits":{"per_min":120,"auth_per_min":10,"heavy_per_min":30}},"lockout":{"failures":10,"window_s":600,"lockout_s":900,"locked_ips":0},"headers":{"csp":"…","x_frame_options":"DENY","referrer_policy":"no-referrer","permissions_policy":"camera=(), microphone=(), geolocation=()","x_content_type_options":"nosniff","hsts":true},"tokens":{"expired_or_revoked":0,"active":2},"auth_mode":"bearer"}`, admin only |

`429` bodies name the bucket that refused the request:

```json
{"error":{"code":"rate_limited","message":"too many requests: retry in 42 second(s)",
  "details":{"retry_after_s":42,"class":"api"}}}
```

`details.class` is `api`, `auth` or `heavy` for a rate limit;
`details.reason = "lockout"` identifies a progressive authentication lockout
(the lock is audited as `auth.lockout`). A `0` limit disables the bucket;
setting every limit to `0` disables rate limiting.

`/api/health` and `/api/auth/config` are exempt. `/auth/login` and
`/auth/callback` use the `auth` bucket; routes containing an `exec`, `logs`,
`files` or `export` segment use the `heavy` bucket. Every response — console,
API, errors — also carries the security headers (`Content-Security-Policy`,
`X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Permissions-Policy`,
`X-Content-Type-Options: nosniff`, and `Strict-Transport-Security` only when
`FLEETT_PUBLIC_URL` is https).

## Templates

`GET /api/templates`
```json
[{"id":"guest","name":"Guest","description":"Web + memory, no infra access",
  "default_model":"deepseek-v4.1-flash","channel":"telegram",
  "env":[{"key":"TELEGRAM_ALLOWED_USERS","description":"Comma-separated Telegram user ids"}]}]
```

## Providers

Reusable API credentials shared by several agents; see `docs/PROVIDERS.md`.
The secret is never returned by any endpoint.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/providers` | — | `[{"id":"…","name":"shared","kind":"openai-compatible","base_url":"…","api_key_env":"…","instances_count":2,"created_at":"…","updated_at":"…"}]` |
| POST | `/api/providers` | `{"name":"shared","kind":"openai-compatible","base_url":"…","api_key_env":"…","secret":"…"}` | `201` summary, admin only |
| GET | `/api/providers/{id}` | — | summary + `"instances":[{"id":"alice","name":"Alice"}]` |
| PATCH | `/api/providers/{id}` | `{"name"?,"kind"?,"base_url"?,"api_key_env"?,"secret"?}` | `{"provider":{…},"restarted":[{"id":"alice","status":"healthy\|failed","error"?}]}`, admin only |
| DELETE | `/api/providers/{id}` | — | `204`, or `409 provider_in_use` with `details.instances`, admin only |
| POST | `/api/providers/{id}/test` | — | `{"ok":true,"status":200,"latency_ms":42}`, admin only |

An instance attaches a provider with `"provider":"<id>"`. `base_url` and
`api_key_env` default to the provider's values; an explicit value in the same
request wins. `secret` on `PATCH` rotates the credential and restarts every
attached instance sequentially (one `restarted` entry each). Instances without
a provider keep their own `gate_token`/`api_key_env`.

## Instances

`GET /api/instances` → `InstanceSummary[]`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "image":"hermes-agent:0.22.2-local","channel":"telegram","status":"running",
 "health":"healthy","cpu_limit":1.0,"allow_lan":false,"provider":"9e2c…",
 "shares":["findings"],"last_error":null,"created_at":"2026-11-08T01:00:00Z"}
```

`POST /api/instances` → `201 InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest",
 "model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN",
 "provider":"9e2c…",
 "channel":{"type":"telegram","token":"123:ABC"},
 "shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,"notes":"",
 "env":[{"key":"DISCORD_ALLOWED_USERS","value":"123","secret":false}]}
```
`id`: `[a-z0-9-]{2,32}`, unique. `channel.type`: `telegram`, `discord` or `none`.
The token is **never** returned by the API.

`GET /api/instances/{id}` → `InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "image":"hermes-agent:0.22.2-local",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN","channel":"telegram",
 "provider":"9e2c…",
 "status":"running","shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,
 "warnings":[],"notes":"","created_at":"...","updated_at":"...",
 "last_error":null,"data_dir":"/opt/fleett/instances/alice/data",
 "env":[{"key":"HERMES_GATE_PROFILE_TOKEN","secret":true,"updated_at":"..."},
        {"key":"DISCORD_ALLOWED_USERS","secret":false,"value":"123","updated_at":"..."}],
 "container":{"id":"3f2a…","image":"hermes-agent:0.22.2-local","uptime_s":3600},
 "compose_yaml":"name: fleett-alice\n…"}
```

`image` is the value of `services.agent.image` in the instance compose file (the image the
next `up -d` uses); `container.image` is what Docker reports for the running container, so the
two differ while an upgrade is in flight or after a failed rollback.

`last_error` carries the last known failure: the health-probe message while the container stays
unhealthy, or the `docker inspect` cause (`State.Error`, `OOMKilled`, non-zero exit code) when
the container is not healthy. It is `null` for a healthy container and for a clean stop.

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Editable fields: `name`, `model`,
`base_url`, `api_key_env`, `provider` (`null` detaches), `channel` (`token` optional =
rotation), `shares`, `mem_limit_mb`, `cpu_limit`, `allow_lan`, `notes`, `env`. Rewrites the
compose file and runs `up -d`.

`cpu_limit` is a float in `0.1..=8.0` (default `1.0`), rendered as `cpus` plus
`memswap_limit == mem_limit`. Optional fleet caps (`FLEETT_MAX_TOTAL_CPU`,
`FLEETT_MAX_TOTAL_MEM_MB`) return `409` with `error.details {used, requested, cap}`.

### Environment variables

Every env entry is rendered as `${KEY}` in the compose file and injected by the driver, so no
value is written to disk. `secret=true` values are encrypted at rest (AES-GCM, same key as the
instance secrets) and are never returned; `secret=false` values are readable and are returned
as `value`.

| Method | Route | Body | Effect |
|---|---|---|---|
| PUT | `/api/instances/{id}/env` | `{"env":[{"key","value","secret"}]}` | full replace and apply |
| DELETE | `/api/instances/{id}/env/{key}` | — | remove one and apply |
| PATCH | `/api/instances/{id}` | `"env":[…]` | merge: `value:null` deletes, `""` keeps an empty value, missing `secret` keeps the flag |

To keep a stored secret in a full replace, resend `{"key":"TOKEN","secret":true}` without a
`value` (or with `null`). Keys match `^[A-Z_][A-Z0-9_]{0,63}$`; reserved names (`HERMES_UID`,
`HERMES_GID`, `PATH`, `HOME`, `FLEETT_*`) are refused. `409 secrets_key_missing` means the host
has no secrets key yet (`sudo fleett secrets init`). Templates expose suggested variables in
their `env` field (Telegram/Discord allowlists).

### Instance files

Browse and edit the files inside `FLEETT_ROOT/instances/<id>/data` (the Hermes
home mounted at `/opt/data`). Paths are virtual: `/` is the data directory.
`..`, NUL and relative paths are `400 invalid_path`; a symlink escaping the
directory is `400 path_escape`. Reads need any role, mutations need `admin`.
See `docs/FILES.md` for the full security model.

| Method | Route | Body | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/files?path=/` | — | `[{name,path,type:"file\|dir",size,mtime,editable,rendered}]`, one level |
| GET | `/api/instances/{id}/files/content?path=/SOUL.md` | — | `{path,content,etag,size,mtime,editable,rendered,note?}`; `413` above 512 KiB, `415` binary |
| PUT | `/api/instances/{id}/files/content[?restart=true]` | `{"path","content","etag"}` | compare-and-swap write; `409 etag_mismatch` on a stale/missing etag; timestamped `.bak` before overwrite; `restart=true` restarts and returns `{restarted,status}` |
| DELETE | `/api/instances/{id}/files/content?path=/notes/old.md` | — | `204`; files and empty directories only (`409 directory_not_empty`), never a rendered file |
| POST | `/api/instances/{id}/files/mkdir` | `{"path":"/profiles"}` | `201 {path,type:"dir"}`, parent must exist |

`etag` is the lowercase SHA-256 of the bytes on disk. The `If-Match` header is
accepted instead of the body field (`*` = the file must exist). Editing is
limited to `.md .yaml .yml .json .txt .toml`; other text files are readable
with `editable:false` (`409 file_not_editable` on write). `data/config.yaml`
and `data/healthcheck.py` are rendered by fleett on every PATCH: they are
readable with `editable:false`/`rendered:true`, and any write returns
`409 file_regenerated`.

### Instance history

Every instance keeps a git repository at `instances/<id>/.git` (libgit2, no
host `git` needed) versioning a whitelist: `data/SOUL.md`, `data/config.yaml`,
`data/skills/**` and the other editor files under `data/` — never a database, a
`.bak`, `healthcheck.py` or `secrets.env`. Reads need any role, the revert needs
`admin`. `docs/GIT.md` documents the layout, authors and limits.

| Method | Route | Body | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/git/log?path=/SOUL.md&limit=50` | — | `{commits:[{id,short,author,email,at,message,paths}]}`, newest first |
| GET | `/api/instances/{id}/git/diff?from=&to=` | — | `{from,to,diff}`; `to` defaults to `HEAD`, `from` to its first parent |
| GET | `/api/instances/{id}/git/status` | — | `{modified:[…],untracked:[…]}`, whitelist only |
| POST | `/api/instances/{id}/git/revert` | `{"commit","paths?","restart?"}` | restores the files of one commit (`.bak` first), commits the restore, audits `instance.git.revert`, optional restart; `404` for an unknown commit, `400 untracked_path` outside the whitelist |

A save through the file API is a `ui: data/SOUL.md` commit attributed to the
signed-in identity (`name <email>` for SSO, `<actor> <actor@fleett.local>` for a
bearer token). A change made outside the API is recorded as `sync: …` by
`fleett <fleett@localhost>` at startup and after every compose apply. Nothing
here talks to Docker: a stopped instance still serves its log, diff and revert.

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

`POST /api/instances/{id}/clone` → `201 InstanceDetail`. Body:
```json
{"new_id":"bob","name":"Bob","model":"…",
 "gate_token":"…","channel_token":"…",
 "shares_inherit":true,"copy_data":true}
```
The clone gets its own Compose project (`fleett-<new_id>`), container, data directory and
database row. Secrets are **never** copied: every token the source holds must be replaced
(`gate_token` for `HERMES_GATE_PROFILE_TOKEN`/`api_key_env`, `channel_token` for the platform
bot token). A missing replacement fails with `400 clone_token_required`
(`error.details {source, missing}`) before anything is written. `copy_data` (default `true`)
copies `data/` recursively; the generated `config.yaml`, `SOUL.md` and health probe always
come from the clone's own rendering. `shares_inherit` (default `true`) copies only the shares
that still exist.

`GET /api/instances/{id}/logs` → `text/plain` (default) or `application/json`.

Query: `tail` (`1..=2000`, default `200`), `since` (Docker duration `30s|10m|2h|1h30m` or an
RFC 3339 timestamp), `grep` (regex), `level=error|warn` (`warn` keeps warnings and above),
`format=text|json`. The response is capped at 1 MiB; `grep`/`level` filtering runs in the app.
`format=json` returns `[{"time":"2026-11-08T01:02:03Z","message":"…"}]` for lines that start
with a timestamp, and `[{"line":"…"}]` otherwise. Invalid values answer `400` with
`invalid_tail`, `invalid_since`, `invalid_grep`, `invalid_level` or `invalid_format`.

`GET /api/instances/{id}/errors?since=24h` → `{"count":2,"last":"…","last_at":"…"}`: counts
the error/warn lines of a bounded window (at most 2000 lines) without returning the log.
`last_at` is the parsed timestamp of the last matching line (`null` when it has none).

`GET /api/instances/{id}/export` → `application/gzip`, `Content-Disposition: attachment;
filename="<id>.tgz"`. A gzipped tar with `manifest.json` (format/version, `created_at` and the
instance metadata: id, name, template, model, base_url, api_key_env, channel, mem_limit_mb,
cpu_limit, allow_lan, shares, notes), `compose.yaml` and `data/` (the full Hermes home).
Share contents and secrets are never included: no `secrets.env`, no `*.key`, no token value.

`POST /api/instances/import?id=<new_id>&start=false` → `201 InstanceDetail`. The raw gzip
archive is the request body (`Content-Type: application/gzip`); the id is required and must be
free (`409 instance_exists`, nothing overwritten). Compression size cap: 2 GiB (`413
archive_too_large`). Every archive entry is checked first: an absolute path or a `..` component
answers `400 invalid_archive` before anything is written; an absent or incompatible manifest
answers `400 invalid_archive` / `400 unsupported_archive` / `400 invalid_manifest`. The import
rewrites the Compose project and container to `fleett-<new_id>`, strips `env_file` and
re-renders the compose file with this build while keeping the `${KEY}` interpolation names.
Secrets are not restored: `warnings` lists the variables to PATCH
(`HERMES_GATE_PROFILE_TOKEN`, the platform bot token), and shares named by the manifest are
attached only when they already exist on this host. Nothing is started unless `start=true`.
See `docs/BACKUP.md`.

## Shared memory

Key/value knowledge base with FTS5 search, TTL and history. See
[docs/MEMORY.md](docs/MEMORY.md). Values are knowledge, never secrets.

`POST /api/memory` → `201` (create) or `200` (update) `{"entry": MemoryEntry, "created": bool}`

```json
{"namespace":"instance:probe","key":"runbook","value":"restart the gateway after an upgrade",
 "tags":["ops"],"ttl_s":86400}
```

`namespace`: `global`, `shared`, `instance:<id>` or `project:<name>`. `ttl_s` is optional
(60 … 31536000). `PUT /api/memory/{id}` updates by id.

`GET /api/memory?namespace=&tag=&limit=50` → `MemoryEntry[]` (newest first, expired and
deleted entries excluded).

`GET /api/memory/search?q=&namespace=&tags=a,b&limit=50` → `MemoryEntry[]`. Every search
term is quoted before it reaches FTS5, so `OR`, `NEAR` and column filters are inert. Tags are
ANDed.

`GET /api/memory/by-key?namespace=&key=` → `MemoryEntry`, `404` when absent.

`GET /api/memory/{id}/history` → `MemoryVersion[]` (newest first). Every write appends a
version; `DELETE` is a soft delete and stays restorable.

`POST /api/memory/{id}/rollback` `{"version":1}` → `MemoryEntry` (a new version).

`POST /api/memory/{id}/promote` `{"namespace":"global"}` → `MemoryEntry` (copy).

`DELETE /api/memory/{id}` → `204`. `DELETE /api/memory/expired` → `{"removed": N}`.

## Agent-to-agent messages

The bus is persistent (SQLite) and documented in [docs/BUS.md](docs/BUS.md).

`POST /api/instances/{id}/messages` → `201 Message`. The path id is the sender.

```json
{"to":"tchouniz","kind":"ask","body":"what changed?","correlation_id":"deploy-42","ttl_s":3600}
```

`kind`: `ask`, `tell` (default) or `result`. `from` is optional and admin-only (defaults to
the path id). 16 KiB max, TTL 30 s … 30 days. A conversation that reaches 8 hops answers
`409 message_loop`.

`GET /api/instances/{id}/messages?view=inbox|outbox|all&status=queued|delivered|read&limit=100`
→ `Message[]`, newest first, expired rows excluded. Reading the inbox stamps `delivered_at`.

`POST /api/instances/{id}/messages/{message_id}/ack` → `{"message": Message}`; stamps
`read_at`. Only the recipient can ack.

`GET /api/messages?instance=&from=&to=&kind=&limit=` → `Message[]` (admin only).

`DELETE /api/messages/expired` → `{"removed": N}` (admin only).

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

## Usage

`GET /api/usage` → `200`, any authenticated role (read-only):
```json
{"instances":[{"id":"alice","status":"running","cpu_pct":1.5,
               "mem_used_bytes":134742016,"mem_limit_bytes":1610612736,
               "disk_bytes":12582912}],
 "totals":{"cpu_pct":1.5,"mem_used_bytes":134742016,
           "mem_limit_bytes":1610612736,"disk_bytes":12582912},
 "caps":{"max_cpu":8.0,"max_mem_mb":16384}}
```

With `FLEETT_DRIVER=compose`, `cpu_pct`/`mem_used_bytes` come from one
`docker stats --no-stream --format json` call (containers are matched by name), `disk_bytes`
from `du -sb` on `instances/<id>/data`, and `mem_limit_bytes` from the container limit (or the
configured limit when the container is absent). Stopped instances report zero CPU and memory.
`totals` sums every instance; `caps` echoes `FLEETT_MAX_TOTAL_CPU` and
`FLEETT_MAX_TOTAL_MEM_MB` (`0` = unlimited). The endpoint answers `502 usage_unavailable`
when Docker fails, and an empty fleet answers zeros without touching Docker. Under
`FLEETT_DRIVER=mock` the numbers are stable simulations.

The fleet is also checked by `fleett doctor`, which reads `docker`, `secrets` and `egress`
from `/api/health` plus `GET /api/auth/me`; see `docs/CLI.md`.

## Images and rolling upgrades

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/images` | — | `[{"tag":"hermes-agent:0.22.2-local","id":"sha256:…","size":"1.2GB","created":"…","instances":["alice"]}]` |
| POST | `/api/upgrade` | `{"image":"<tag>","parallel":1,"dry_run":false,"timeout_s":180}` | `202 UpgradeJob` · `409 upgrade_in_progress` · `502 image_unavailable` |
| GET | `/api/upgrade` | — | `200 UpgradeJob` (current or last) · `404` before the first job |

`UpgradeJob`:
```json
{"id":"2b6d…","image":"hermes-agent:0.23.0-local",
 "started_at":"2026-11-08T01:00:00Z","finished_at":"2026-11-08T01:04:12Z",
 "dry_run":false,"parallel":1,"timeout_s":180,
 "results":[{"id":"alice","from":"hermes-agent:0.22.2-local",
             "to":"hermes-agent:0.23.0-local","status":"healthy"},
            {"id":"bob","from":"hermes-agent:0.22.2-local",
             "to":"hermes-agent:0.23.0-local","status":"rolled_back",
             "error":"upgrade failed: the container reported unhealthy; rolled back to hermes-agent:0.22.2-local"}]}
```

`status` per instance: `pending`, `upgrading`, `healthy`, `rolled_back`, `failed`. `from` is
`null` only when the instance had no image recorded yet. `finished_at` is absent while the job
runs and on a dry run is equal to `started_at`.

`POST /api/upgrade` pulls the image first (`docker image inspect`, then `docker pull`) and
answers `502 image_unavailable` with the Docker detail when it cannot; nothing is written in
that case. Requests are validated: `image` is required (`400 invalid_image`), `parallel` is
`1..=16` (default `1`, i.e. sequential), `timeout_s` is `1..=3600` (default `180`). A second
job while one runs answers `409 upgrade_in_progress`, whatever the body. Admin only: a viewer
gets `403 forbidden` like every other mutation.

Per instance, the job:

1. makes sure the image is local (skipped in a dry run);
2. records `from` and moves `services.agent.image` to the new tag, keeping the old one as the
   `fleett.previous_image` label of the compose file;
3. runs `docker compose up -d` and polls `ps` until the container reports `healthy`;
4. on `unhealthy` or after `timeout_s`, rewrites the previous tag, runs `up -d` again and sets
   `last_error` to `upgrade failed: <reason>; rolled back to <old>` with status `rolled_back`;
   a failed rollback leaves the instance `error` and the result `failed`.

A pull failure for one instance (after the initial check) marks that instance `failed` without
touching it. Every result is audited: `instance.upgrade` on success, `instance.upgrade.rollback`
on a restore. A dry run lists the planned changes (`results[*].status = "pending"`), writes no
file, pulls nothing and audits nothing.

The compose file is the source of truth: the database `image` column is resynchronised from
`services.agent.image` at startup, so a hand edit or a restore is picked up. See
`docs/UPGRADE.md` for the operator procedure.
