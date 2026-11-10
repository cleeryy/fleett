# fleett — API v1

Base: `http://127.0.0.1:8090`. Every `/api/*` route requires a bearer token
(`Authorization: Bearer <FLEETT_TOKEN>`) or, when `FLEETT_AUTH` includes
`oidc`, a `fleett_session` cookie, except `GET /api/health`,
`GET /api/auth/config` and `GET /api/openapi.json`. See `docs/OIDC.md` for the
SSO setup.

Errors: `{"error":{"code":"...","message":"..."}}` with the matching HTTP status
(`400` invalid input, `401` missing/wrong credential, `403` role or CSRF
refusal, `404` unknown, `409` conflict, `422` invalid YAML/compose,
`429` rate limit or authentication lockout, `502` Docker or provider failure).
A `429` always carries `Retry-After` (whole seconds) and
`details.retry_after_s`. Unauthenticated API calls never redirect: `401` JSON,
always.

## OpenAPI contract

`GET /api/openapi.json` is the authoritative contract of the running binary. It
is public, and its `info.version` is the binary version (`fleett --version`,
the same `CARGO_PKG_VERSION` as the `fleett_build_info` metric): the handler
parses the embedded document once and overwrites the stored version, so a stale
literal can never be served. A unit test compares the document with
`api_routes` in both directions — `cargo test` fails when a router route is not
documented, and when the document claims an operation the router does not
serve. CI validates the file with `openapi-spec-validator`. The TypeScript SDK
in `sdk/typescript` is a hand-written typed client of this contract; it is
typechecked in CI.

Credentials are resolved in this order: session cookie, then bearer token
(`FLEETT_TOKEN` = admin, then named tokens). Authorization is granted per
instance: `owner`, `operator` and `viewer` levels come from an `instance_acl`
row, from a scoped token's `instances` list, or from the global `admin` role.
See `docs/RBAC.md`. A `viewer` **session** only sees the instances an ACL grants
it; a bearer token created before scopes existed keeps its global viewer read.
A write outside an instance route, or a write an instance ACL does not allow,
returns `403 forbidden`. A cookie-authenticated mutation must also echo the
`fleett_csrf` cookie in `X-Fleett-CSRF`, otherwise `403 csrf_required`; bearer
clients are exempt.

## Health and auth

| Method | Route | Response |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.3.0","driver":"mock\|compose","docker":{"ok":true},"uptime_s":42,"secrets":{"key":true,"key_error":null,"encrypted":2,"plaintext":0},"egress":{"mode":"restrict\|off","marker":true}}` |
| GET | `/api/auth/config` | `{"mode":"bearer\|oidc\|both","oidc":{"enabled":true,"login_url":"/auth/login"},"bearer":{"enabled":true}}` |
| GET | `/api/auth/check` | `204` when the credential is valid |
| GET | `/api/auth/me` | `{"authenticated":true,"actor":"…","subject":"…","role":"admin\|viewer","method":"session\|bearer","user":{"sub":"…","email":"…","name":"…"}}` (`user` only for sessions; `subject` is the key matched against `instance_acl.subject`) |
| GET | `/auth/login` | `303` to the IdP; `404` when OIDC is disabled |
| GET | `/auth/callback` | `303` to `/` and sets `fleett_session` + `fleett_csrf`; failures carry `details.reason` (`cookie_missing`, `state_mismatch`, `expired`). Browsers (`Accept: text/html`) get an HTML page with a retry link, and a missing state cookie redirects to `/auth/login`; API clients keep JSON `400`/`401`/`403`. State lifetime: `FLEETT_OIDC_STATE_TTL_SECS` (default 600) |
| POST | `/auth/logout` | `204`, clears the fleett cookies |

## Doctor and onboarding

Read-only diagnostics and first-run state, available to every authenticated role.

| Method | Route | Response |
|---|---|---|
| GET | `/api/doctor` | `{ok, checks:[{id,label,status:"ok\|warn\|error",detail,fix?}]}` — api, docker, image, secrets, egress, egress-policy, providers, instances, permissions, disk, dns |
| GET | `/api/onboarding` | `{complete, steps:[{id,title,description,done,action?}]}` — docker, image, provider, instance; `complete` when at least one instance exists |

`fix` is either a copyable command or a UI link; a failing check is reported inline and never
turns the response into a 500. See [docs/ONBOARDING.md](docs/ONBOARDING.md).

## Assistant

A contextual drafting copilot available on every page. It borrows the secret and
base URL of the provider named by `FLEETT_ASSISTANT_PROVIDER` and never stores a
credential of its own. The context is redacted server-side before it leaves the
process: secret environment values, the provider key and known token shapes
(`sk-…`, `ghp_…`, `xoxb-…`, `Bearer …`, PEM blocks) are withheld or replaced.
Proposals are applied by the console through the regular file/env routes, so
they keep their ETag, `.bak`, git commit and audit row (`via: "assistant"`).
`FLEETT_ASSISTANT=off` removes both routes (generic `404`) and hides the panel.
Full semantics: [docs/ASSISTANT.md](docs/ASSISTANT.md).

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/assistant/config?session_id=` | — | `{enabled,provider,model,budget_tokens,used_tokens}`; `provider` is `null` when unset |
| POST | `/api/assistant/chat` | `{session_id,context:{page,instance?,path?,selection?,file_content?,schema?},messages:[{role,content}],task?}` | `{message,proposal?,usage,redaction}`; `409 assistant_not_configured`, `422 invalid_messages`, `429 assistant_budget_exceeded` |

`task` is `freeform` (default), `skill`, `soul`, `env`, `pipeline`, `fleet_yaml`,
`explain` or `config`. A proposal is `{kind:"file"|"yaml",path,content}` or
`{kind:"env",env:[{key,value?,secret}]}`; a secret env entry never carries a
value. `redaction` lists what was sent and what was withheld, so the operator can
audit the exact prompt context.

## Audit and tokens

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/audit` | — | `[{"id":1,"at":"…","actor":"…","role":"admin","method":"session","action":"instance.create","target":"alice","ip":"…","user_agent":"…","result":"ok","request_id":null,"details":{…}}]`, newest first, admin only |
| GET | `/api/audit/export?from=&to=` | — | `200 application/x-ndjson`, one JSON object per line, `Content-Disposition: attachment; filename="fleett-audit-<ts>.jsonl"`, `X-Fleett-Signature: sha256=<hex HMAC-SHA256 of the exact body>`; `409 audit_key_missing` without `FLEETT_AUDIT_HMAC_KEY`, `400 missing_range` without both bounds, admin only |
| DELETE | `/api/audit/expired` | — | `{"removed":12,"cutoff":"…","retention":"90d"}` deletes rows older than `FLEETT_AUDIT_RETENTION` and appends an `audit.purge` row, admin only |
`GET /api/audit` filters: `limit` (default 200, max 1000), `offset`, `actor`,
`action`, `target`, `from`, `to`, `result` (`ok|failed|refused`). `actor`,
`action`, `target` and `result` are exact matches; `from`/`to` accept RFC 3339
or `YYYY-MM-DD` (a date-only `to` covers the whole day). The legacy `?limit=`
behaviour is unchanged.

| GET | `/api/tokens` | — | `[{"id":"…","name":"ci","role":"viewer","created_at":"…","revoked_at":null,"scope":{"instances":["probe"],"actions":["read"]}}]`, never the secret, admin only |
| POST | `/api/tokens` | `{"name":"ci","role":"viewer\|admin","scope"?}` | `201` with the one-time `token` (`flt_<32 hex>`), admin only |
| DELETE | `/api/tokens/{id}` | — | `204` (sets `revoked_at`), `404` unknown, admin only |

`GET /api/audit` filters: `limit` (default 200, max 1000), `offset`, `actor`,
`action`, `target`, `from`, `to`, `result` (`ok|failed|refused`). `actor`,
`action`, `target` and `result` are exact matches; `from`/`to` accept RFC 3339
or `YYYY-MM-DD` (a date-only `to` covers the whole day). The legacy `?limit=`
behaviour is unchanged.

`role` defaults to `viewer`. `scope` is optional: `{"instances":["probe"],
"actions":["read","write","exec"]}` restricts the token to those instance ids
and actions. Every instance must exist, `actions` is a non-empty subset of
`read`/`write`/`exec`; violations answer `400 invalid_scope`. An omitted scope
keeps the legacy global role, and `scope` is absent from `GET /api/tokens` for
such rows. A scope always intersects the ACL, even for an `admin` token. See
`docs/RBAC.md`. Revoking is idempotent. Tokens are stored as a SHA-256 digest
only. Full audit documentation: [docs/AUDIT.md](docs/AUDIT.md).

## Instance ACL

Per-instance grants; admin or an `owner` of the instance only. See
`docs/RBAC.md` for the role matrix and enforcement points.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/instances/{id}/acl` | — | `{"instance":"probe","acl":[{"subject":"cle@example.test","role":"owner","created_at":"…"}]}`, `404` unknown instance |
| PUT | `/api/instances/{id}/acl` | `{"acl":[{"subject":"…","role":"owner\|operator\|viewer"}]}` | `200` with the new list; replaces every row; `400 invalid_acl` on a bad role/subject/duplicate |
| DELETE | `/api/instances/{id}/acl/{subject}` | — | `204`, `404` when the subject has no row |

The level is read from SQLite on every request: a grant, a role change or a
removal applies to the next call. Changes are audited as
`instance.acl.update` / `instance.acl.delete` with the affected `subject` in
`details`.

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
`/auth/callback` use the `auth` bucket; routes containing an `exec`, `files` or
`export` segment — and the legacy per-instance `/api/instances/{id}/logs` —
use the `heavy` bucket. The log-center read routes under `/api/logs` use the
ordinary `api` bucket, so the console's one-second polling fallback fits the
default limit. Every response — console,
API, errors — also carries the security headers (`Content-Security-Policy`,
`X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Permissions-Policy`,
`X-Content-Type-Options: nosniff`, and `Strict-Transport-Security` only when
`FLEETT_PUBLIC_URL` is https).

## Templates

Built-in templates (`guest`, `guest-discord`, `solo`) are compiled constants and
read-only. Custom templates live under `$FLEETT_ROOT/templates/<id>/` and keep a
versioned history; see `docs/TEMPLATES.md`.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/templates` | — | `TemplateInfo[]` (built-ins + custom, env schema and defaults resolved through the parent chain) |
| POST | `/api/templates` | `{"id","name","description"?,"parent"?,"default_model"?,"channel"?,"env":[…],"config_yaml"?,"soul_md"?,"defaults"?}` | `201 TemplateDetail`, admin only |
| GET | `/api/templates/{id}` | — | `TemplateDetail` (`config_yaml`, `soul_md`, `versions_count`) |
| PUT | `/api/templates/{id}` | same fields, all optional; `null` clears | `200 TemplateDetail`, admin only |
| DELETE | `/api/templates/{id}` | — | `204`; `409 read_only` for a built-in; `409 template_in_use` with `details.instances` / `details.templates` (child templates), admin only |
| POST | `/api/templates/from-instance` | `{"instance_id","id","name"?}` | `201 TemplateDetail`, admin only |
| GET | `/api/templates/{id}/versions` | — | `[{"version":1,"action":"create\|update\|delete","actor":"…","at":"…","sha256":{"config.yaml":"…","soul.md":"…"}}]` |
| POST | `/api/templates/{id}/restore` | `{"version":1}` | `200 TemplateDetail`; the restored files are written and a new version is appended, admin only |

`TemplateInfo`:
```json
{"id":"house","name":"House","description":"…","default_model":"deepseek-v4.1-flash",
 "channel":"telegram","parent":"guest","read_only":false,
 "env":[{"key":"HOUSE_FLAG","description":"…","example":"1","required":true,
         "format":"boolean","secret":false}],
 "defaults":{"mem_limit_mb":2048,"cpu_limit":2.0,"allow_lan":false},
 "source_instance":null,"persona":"house","instances_count":2,
 "created_at":"2026-11-09T01:00:00Z","updated_at":"2026-11-09T02:00:00Z"}
```

`env` entries extend the pre-0.6 shape: `example`, `required` and `format` are
optional (`required` defaults to `false`). Inherited entries are merged by key,
the child winning the description/flags. `defaults` drive
`POST /api/instances` when the request leaves `model`, `base_url`,
`mem_limit_mb`, `cpu_limit`, `allow_lan` or `notes` out.

A custom template may set `parent`; resolution merges the parent then the child
per field. A cycle is `400 template_cycle`. Built-ins can be parents but never
modified (`409 read_only` on PUT/DELETE/restore).

`POST /api/instances` refuses an unknown template (`400 unknown_template`), and
a template with a `required` env entry the request does not supply returns
`400 missing_required_env`:
```json
{"error":{"code":"missing_required_env","message":"template \"house\" requires environment variable(s): HOUSE_FLAG",
 "details":[{"key":"HOUSE_FLAG","field":"env"}]}}
```
When an env entry omits `secret`, the template schema decides; an explicit
`secret` in the request wins.

A built-in template looks like:

```json
[{"id":"guest","name":"Guest","description":"Web + memory, no infra access",
  "default_model":"deepseek-v4.1-flash","channel":"telegram",
  "env":[{"key":"TELEGRAM_ALLOWED_USERS","description":"Comma-separated Telegram user ids",
          "secret":false,"example":"123456789","required":true,"format":"csv-of-ids"},
         {"key":"GATEWAY_ALLOW_ALL_USERS","description":"Accept every user…",
          "secret":false,"example":"false","required":false,"format":"bool"}]}]
```

`example` is the value the console should pre-fill, `required` marks the
allowlists that make the gateway refuse every message when unset, and `format`
is enforced by the write endpoints (`csv-of-ids`, `bool`, `url`, `text`). The
three fields are optional on input: a `templates/<id>/template.yaml` written for
an older release still parses.

### Instance check

`GET /api/instances/{id}/check` compares the template `required` suggestions
against the stored environment and the live container health:
```json
{"ok":false,
 "warnings":[{"code":"allowlist_missing","key":"DISCORD_ALLOWED_USERS",
              "message":"DISCORD_ALLOWED_USERS is required by the guest-discord template but is not set: …",
              "field":"env.DISCORD_ALLOWED_USERS"}],
 "template":"guest-discord"}
```

- `allowlist_missing`: a required key is absent or empty (warning for every
  channel, stopped or running);
- `gateway_may_refuse`: the container is `healthy` while a required key is
  missing;
- `ok` is `true` iff `warnings` is empty;
- `field` is the console form field to focus (`env.<KEY>`);
- `template` is the template id (`InstanceDetail.template`).

The same messages are appended to `InstanceDetail.warnings` (after the
egress/LAN and plaintext-secrets warnings), so the console does not need to call
`/check` to display them. Read-only: no health/last_error write, no compose
call.

## Personas

A persona is a reusable `SOUL.md` template plus the variables it is rendered
with. It lives under `$FLEETT_ROOT/personas/<id>/` (`persona.yaml` + `SOUL.md`),
is versioned in one git repository at the root of that directory, and can be
assigned to any instance. See [docs/PERSONAS.md](docs/PERSONAS.md).

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/personas` | — | `PersonaSummary[]` (`instances_count`, `excerpt`, `updated_at`) |
| POST | `/api/personas` | `{"id","name","description"?,"tags":[…],"variables":[{key,description,default}],"soul_md"}` | `201 PersonaDetail`, admin only |
| GET | `/api/personas/{id}` | — | `PersonaDetail` (`soul_md`, `variables`, `instances`) |
| PATCH | `/api/personas/{id}` | same fields, all optional; `description:""` clears | `200 PersonaDetail`, admin only |
| DELETE | `/api/personas/{id}` | — | `204`; `409 persona_in_use` with `details.instances`, admin only |
| POST | `/api/personas/{id}/preview` | `{"variables":{…}?,"instance":"alice"?}` | `{"rendered","missing":[…]}` |
| POST | `/api/personas/{id}/apply` | `{"instances":["alice","bob"],"variables":{…}?}` | `{"results":[{"id","status":"applied\|failed","error"?}]}`, admin only |
| GET | `/api/personas/{id}/git/log?limit=50` | — | `{"commits":[GitCommit]}` touching this persona |
| GET | `/api/personas/{id}/git/diff?from=&to=` | — | `{"from","to","diff"}` (unified patch of the persona files) |

`PersonaDetail`:
```json
{"id":"house","name":"House","description":"The shared voice of the fleet",
 "tags":["ops","french"],
 "variables":[{"key":"tone","description":"How direct the agent is","default":"direct"},
              {"key":"language","description":"Answer language","default":"français"}],
 "soul_md":"# Identité\n\nTu es {{agent_name}}.\nTon ton est {{tone}}.\n",
 "instances":["alice","bob"],"instances_count":2,
 "created_at":"2026-11-09T10:00:00Z","updated_at":"2026-11-09T11:00:00Z"}
```

Placeholders are `{{key}}` (inner spaces allowed) where `key` matches
`^[a-z][a-z0-9_]{0,31}$`; `{{MODEL}}` and any other non-matching text are left
verbatim so the instance placeholders keep working. `agent_name` is filled with
the instance name when one is known. A key with no value (no default, no
override) is reported in `missing` by the preview and refuses an `apply`
(`400 missing_persona_variables`, `details.missing`).

On an instance, `PATCH` accepts `persona` (`null` detaches **without** touching
`SOUL.md`) and `persona_vars` (merged; `null` deletes one key). Assigning or
changing a value renders `data/SOUL.md`, backs the previous file up
(`SOUL.md.bak-YYYYMMDD-HHMMSS`), commits it to the instance history, stores the
binding and restarts the instance. `persona_state` is `applied` (the file is a
fresh render of the persona with the current values), `diverged` (hand edit,
persona change or missing file) or `none`.

## Skills

A versioned, read-only skill registry shared by several instances. Each version
lives under `$FLEETT_ROOT/skills/<name>/<version>/` (`SKILL.md` required, plus
any text files, `skill.yaml` metadata); one git repository covers the tree and
a `latest` pointer drives the instances that do not pin a version. See
[docs/SKILLS.md](docs/SKILLS.md).

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/skills` | — | `[{name,description,latest,versions,instances_count,source?,ref?,commit?}]` |
| GET | `/api/skills/{name}` | — | metadata, versions and files (each version may carry `source?`,`ref?`,`commit?`,`upstream_version?`,`author?`,`tags?`,`installed_at?`) |
| POST | `/api/skills` | `{"name","version","description"?,"files":[{path,content}]}` | `201`; `409 skill_version_exists`, admin only |
| POST | `/api/skills/import` | multipart zip (`skill.yaml` + files) | `201` with the next free version, admin only |
| POST | `/api/skills/{name}/promote` | `{"version":"v2"}` | `{"latest","results":[{id,status:"restarted\|failed",error?}]}`; restarts followers only, admin only |
| DELETE | `/api/skills/{name}` | — | `204`; `409 skill_in_use` with `details.instances`, admin only |
| GET | `/api/skills/{name}/git/log?limit=50` | — | commits touching the skill |
| GET | `/api/skills/{name}/git/diff?from=&to=` | — | unified patch |

On an instance, `skills: [{name, version?}]` mounts each skill read-only at
`/opt/data/skills/<name>`. Omitting `version` follows `latest`; pinning freezes
the mount. The instance detail returns `{name, version, effective_version}`.
Changing the list rewrites the compose file and runs `up -d`.

## Marketplace

A catalogue of upstream skills with preview, install, update and promote; see
[docs/MARKETPLACE.md](docs/MARKETPLACE.md). Providers (`local`, `git`,
`github`, `skills.sh`) are independent: one failing provider reports a status
and never fails the search. Installed versions carry their provenance in
`skill.yaml` (`source`, `ref`, `commit`, `upstream_version`, `author`, `tags`,
`installed_at`), exposed additively by the skill endpoints.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/marketplace/skills?q=&source=&limit=` | — | `{"entries":[{name,description,provider,source?,ref?,url?,author?,tags?,version?,commit?}],"sources":[{source,status:"ok\|disabled\|error",count,message?}]}` |
| POST | `/api/marketplace/skills/preview` | `{"source","ref"?}` | `{name,description,version?,author?,files:[{path,size,script}],total_bytes,skill_md,scripts,commit?,source?,requires_confirmation}`; any role, rate-limited |
| POST | `/api/marketplace/skills/install` | `{"source","ref"?,"name"?,"confirm_scripts"?}` | `201` `SkillDetail`; `409 scripts_require_confirmation` with `details.scripts`, `409 skill_version_exists`, admin only |
| POST | `/api/marketplace/skills/{name}/update` | `{"promote"?,"confirm_scripts"?}` | `{name,from,to,commit?,files:[{path,status}],promoted?:{latest,results}}`; `409 no_upstream`, `409 upstream_unchanged`, admin only |
| GET | `/api/marketplace/updates` | — | `{"updates":[{name,installed_version,upstream_version?,commit?,source,ref?}],"warnings":[…]}`, cached 60 s; an outage yields an empty list plus warnings, never `500` |
| POST | `/api/marketplace/sync` | — | `{"skills":N,"source":"owner/repo","commit"?}`; `409 no_index_configured`, admin only |

Uninstall stays `DELETE /api/skills/{name}` (`409 skill_in_use` while an
instance mounts the skill). Imported content is never executed, symlinks are
refused, downloads are capped and HTTPS-only (loopback excepted).

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

## Declarative state (GitOps)

One document describes the whole fleet; see `docs/GITOPS.md`. The document is
the raw `fleet.yaml` text (or a JSON object) and never contains a secret value,
only `secret_ref` references resolved by the control plane at apply time.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/desired-state` | — | `{"applied_at":"…","source":"cli","document_text":"…","document":{…},"in_sync":false,"drift":{…}}`; `document`/`applied_at` are absent before the first apply |
| GET | `/api/desired-state/drift` | — | `{"found":true,"applied_at":"…","source":"api","in_sync":false,"drift":{…}}` |
| POST | `/api/desired-state/plan` | `{"document":"<yaml>"}` or the raw document | `Plan`, no write |
| POST | `/api/desired-state/apply[?allow_delete=true]` | `{"document":"<yaml>","allow_delete":false,"source":"api"}` | `{"plan":{…},"applied":[…],"applied_at":"…","source":"api"}`, admin only |

`Plan` has four arrays of `{"action":"create|update|delete","resource":
"provider|instance","id":"alice","name":"Alice","fields":[{"field":"model",
"from":"…","to":"…"}]}`: `create`, `update`, `delete` (executed) and `refused`
(deletions blocked because `allow_delete` was not set). Applying an equivalent
document returns an empty plan. Every apply is audited as `gitops.apply`;
deletions require `allow_delete` (query or body). `409 gitops_conflict` is
returned when a provider deletion would orphan an instance.

## Docker hosts

A host is a Docker endpoint. `local` always exists; `ssh` hosts are reached with
`DOCKER_HOST=ssh://…` (Docker's own SSH transport). See `docs/HOSTS.md` for the
bind-mount requirement: paths are resolved on the target, not on the controller.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/hosts` | — | `[{"id":"local","name":"local","transport":"local","address":"","status":"ok","last_check_at":"…","last_error":null,"instances":2,"capacity":{"ncpu":8,"mem_total_bytes":17179869184,"docker_version":"27.3.1"},"free_mem_bytes":16106127360,"free_cpu":6.0}]` |
| POST | `/api/hosts` | `{"id"?,"name":"pve3","transport":"local\|ssh","address"?,"docker_context"?}` | `201 HostSummary`, admin only; the connection is tested immediately and `status`/`last_error` are recorded |
| GET | `/api/hosts/{id}` | — | `200 HostSummary`, probed now |
| PATCH | `/api/hosts/{id}` | `{"name"?,"transport"?,"address"?,"docker_context"?}` | `200 HostSummary`, re-tested; the local host cannot be renamed or moved, admin only |
| DELETE | `/api/hosts/{id}` | — | `204` · `409 host_protected` (local) · `409 host_in_use` with `details.instances`, admin only |
| POST | `/api/hosts/{id}/test` | — | `{"ok":true,"status":"ok","checked_at":"…","latency_ms":42,"capacity":{…}}`, admin only |

`status`: `unknown`, `ok` or `unreachable`. `capacity` is best-effort (`docker info`
`NCPU`/`MemTotal`/`ServerVersion`) and `free_mem_bytes`/`free_cpu` subtract the declared
limits of the instances already placed on the host. A failed probe never fails the list:
the host is reported `unreachable` with `last_error`.

Instances carry a `host` field. `POST /api/instances` accepts `"host":"<id>|auto"`
(default `local`): an explicit host must be registered (`400 unknown_host`), while `auto`
probes every host and picks the reachable one with the most free memory, then CPU, then the
fewest instances. Lifecycle, logs, exec/files (`docker cp`) and upgrades all target the
instance host.

`POST /api/instances/{id}/move` → `200 InstanceDetail`
```json
{"to":"pve3","start":true}
```
Stops the instance, relabels it, starts it on the target and rolls the label back (restarting
the source) if the start fails. **The data directory is not copied by the API**: the target
must expose the same absolute bind-mount path (shared storage), or the operator transfers it
first with `fleett instances move` (scp/ssh) and then calls `--skip-transfer`. See
`docs/HOSTS.md`.

## Instances

`GET /api/instances` → `InstanceSummary[]`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "image":"hermes-agent:0.22.2-local","channel":"telegram","status":"running",
 "health":"healthy","cpu_limit":1.0,"allow_lan":false,"host":"local","provider":"9e2c…",
 "persona":"house","persona_vars":{"tone":"direct"},"persona_state":"applied",
 "shares":["findings"],"last_error":null,"created_at":"2026-11-08T01:00:00Z",
 "role":"owner"}
```

The list is filtered server-side: a non-admin only receives the instances it
may read (ACL or token scope), and `role` is the caller's level on that row
(`owner`, `operator` or `viewer`). See `docs/RBAC.md`.

`POST /api/instances` → `201 InstanceDetail` (admin only)
```json
{"id":"alice","name":"Alice","template":"guest",
 "model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN",
 "provider":"9e2c…",
 "channel":{"type":"telegram","token":"123:ABC"},
 "shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,"host":"local",
 "disk_quota_mb":0,"disk_policy":"warn","notes":"",
 "env":[{"key":"DISCORD_ALLOWED_USERS","value":"123","secret":false}]}
```
`id`: `[a-z0-9-]{2,32}`, unique. `channel.type`: `telegram`, `discord` or `none`.
The token is **never** returned by the API. `host` defaults to `local`; `auto` applies
placement (`docs/HOSTS.md`). `env` is a list of `{key, value, secret}`; the
deprecated v0.4.0 map (`{"KEY": "value"}`) is still accepted and stores every entry as a secret
(see [Environment variables](#environment-variables)).

`GET /api/instances/{id}` → `InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "image":"hermes-agent:0.22.2-local",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN","channel":"telegram",
 "provider":"9e2c…","persona":"house","persona_vars":{"tone":"direct"},
 "persona_state":"applied",
 "status":"running","shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"cpu_limit":1.0,"allow_lan":false,"host":"local",
 "disk_quota_mb":0,"disk_policy":"warn",
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

`warnings` is a list of human-readable strings. It starts with the host egress/LAN warnings and
the plaintext-`secrets.env` warning, then carries one line per template warning (missing
`required` allowlist, gateway healthy but refusing because of it). See "Instance check" above;
the structured form (with `code`, `key` and `field`) comes from
`GET /api/instances/{id}/check`.

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Editable fields: `name`, `model`,
`base_url`, `api_key_env`, `provider` (`null` detaches), `persona` (`null` detaches without
touching `SOUL.md`), `persona_vars`, `channel` (`token` optional =
rotation), `shares`, `mem_limit_mb`, `cpu_limit`, `allow_lan`, `notes`, `env`. Rewrites the
compose file and runs `up -d`.

`cpu_limit` is a float in `0.1..=8.0` (default `1.0`), rendered as `cpus` plus
`memswap_limit == mem_limit`. Optional fleet caps (`FLEETT_MAX_TOTAL_CPU`,
`FLEETT_MAX_TOTAL_MEM_MB`) return `409` with `error.details {used, requested, cap}`.

`disk_quota_mb` (default `0` = unlimited) and `disk_policy` (`warn`, `freeze`, `stop`)
bound the data directory; a create also requires the host to keep
`max(FLEETT_MIN_FREE_DISK_MB, 2 * disk_quota_mb)` free, otherwise it answers
`409 insufficient_disk` with `details {free_mb, required_mb}`. See
[QUOTA.md](QUOTA.md).

### Environment variables

`env` is a **list of objects**:

```json
[{"key": "DISCORD_ALLOWED_USERS", "value": "123,456", "secret": false}]
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `key` | string | yes | `^[A-Z_][A-Z0-9_]{0,63}$`, not a reserved name |
| `value` | string \| null | create: yes; PATCH: no; PUT: for a new or non-secret key | `null` deletes on PATCH and keeps a stored secret on PUT |
| `secret` | boolean | create/PUT: yes; PATCH: no | encrypted at rest and never returned; a missing flag on PATCH keeps the current one (`false` for a new key) |

Every env entry is rendered as `${KEY}` in the compose file and injected by the driver, so no
value is written to disk. `secret=true` values are encrypted at rest (AES-GCM, same key as the
instance secrets) and are never returned; `secret=false` values are readable and are returned
as `value` by `GET /api/instances/{id}`.

| Method | Route | Body | Effect |
|---|---|---|---|
| PUT | `/api/instances/{id}/env` | `{"env":[{"key","value","secret"}]}` | full replace and apply |
| DELETE | `/api/instances/{id}/env/{key}` | — | remove one and apply |
| PATCH | `/api/instances/{id}` | `"env":[…]` | merge: `value:null` deletes, `""` keeps an empty value, missing `secret` keeps the flag |

Two copy-paste examples:

```bash
# PATCH: merge one key (rewrites compose and runs up -d)
curl -X PATCH https://fleett.example/api/instances/tchouniz \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env":[{"key":"DISCORD_ALLOWED_USERS","value":"123456789","secret":false}]}'
```

```bash
# PUT: replace the whole environment
curl -X PUT https://fleett.example/api/instances/tchouniz/env \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env":[
        {"key":"DISCORD_ALLOWED_USERS","value":"123456789","secret":false},
        {"key":"DISCORD_BOT_TOKEN","secret":true}
      ]}'
```

To keep a stored secret in a full replace, resend `{"key":"TOKEN","secret":true}` without a
`value` (or with `null`). Keys match `^[A-Z_][A-Z0-9_]{0,63}$`; reserved names (`HERMES_UID`,
`HERMES_GID`, `PATH`, `HOME`, `FLEETT_*`) are refused. `409 secrets_key_missing` means the host
has no secrets key yet (`sudo fleett secrets init`). Templates expose suggested variables in
their `env` field (Telegram/Discord allowlists) with `example`, `required` and `format`.

A value that does not match the `format` of its template suggestion is refused on create,
clone, `PATCH` and `PUT .../env` with `400 invalid_env_format` and
`details {key, expected, received}`. Unknown keys are never rejected. Formats:
`csv-of-ids` (comma-separated ASCII digits, spaces allowed), `bool`
(`true`/`false`/`1`/`0`/`yes`/`no`), `url` (absolute http(s) with a host), `text`
(non-empty). See `docs/ENV.md`.

#### Deprecated v0.4.0 object form

Releases up to v0.4.0 sent `"env": {"KEY": "value"}` on create. That form is **still accepted**
by `POST /api/instances`, `PATCH /api/instances/{id}` and `PUT /api/instances/{id}/env`, but it
is deprecated and logs `deprecated=env_map` at `warn` level. Every entry of it is stored with
`secret: true` (the v0.4.0 behaviour), so the values are encrypted and never returned by `GET`.
Migrate to the list form: a non-secret allowlist has to be resent explicitly with
`"secret": false` (a `PUT` replaces the whole set). The object form is never returned: the
response `env` is always a list.

A malformed `env` (a string, a number, an array entry that is not an object, an object value
that is not a string) is a `400 invalid_env` that names the expected schema and the received
JSON type, instead of axum's raw deserialization text:

```json
{"error":{"code":"invalid_env","message":"env must be a list of {key,value,secret}","details":{"expected":"sequence","received":"map","example":[{"key":"DISCORD_ALLOWED_USERS","value":"123,456","secret":false}]}}}
```

Any other deserialization failure of these bodies is a `400 invalid_body`; `error.details.path`
names the offending field when serde reports one (for example `mem_limit_mb`).

#### Precedence with the historical `secrets.env`

The per-key `instance_env` rows written by the `env` API are the only source the compose file
interpolates. The historical plaintext `instances/<id>/secrets.env` is still injected by the
driver while the instance has no row, and `GET` lists its keys as secrets; the first env write
(`POST`, `PATCH`, `PUT .../env`, `DELETE .../env/{key}`) supersedes it and removes the file. An
`env` entry therefore always wins over a key of the same name in `secrets.env`, and a value
that exists only there disappears from the compose file after that write. See `docs/ENV.md` and
`docs/SECRETS.md`.

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
| POST | `/api/instances/{id}/files/upload?path=/&overwrite=false` | multipart: `path` field + one or more `file` fields | `201 {directory,files:[{path,size,backup?}],total}`; 10 MiB per file, 32 MiB per request, executables refused |
| GET | `/api/instances/{id}/files/download?path=/capture.png` | — | exact bytes with `Content-Disposition: attachment`; binary allowed |
| GET | `/api/instances/{id}/files/archive?path=/notes` | — | `application/zip` of one directory; `409 archive_too_large` above the cap |
| GET | `/api/instances/{id}/files/search?q=&path=/&regex=false&limit=200` | — | `{results:[{path,line,preview}],truncated,scanned}`; literal by default, `regex=true` for a pattern |

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

### Snapshots (restore points)

Per-instance restore points for the data of one agent: `data/`, compose,
rendered config, `SOUL.md` and the `instance_env` rows (secrets stay
encrypted). Reads need any role, every mutation needs `admin`. The full
semantics are in [docs/SNAPSHOTS.md](docs/SNAPSHOTS.md).

| Method | Route | Body | Response |
|---|---|---|---|
| POST | `/api/instances/{id}/snapshots` | `{"label":"before upgrade","ttl_s":604800}` (both optional) | `201 Snapshot`, `400 invalid_snapshot` for a bad label/TTL |
| GET | `/api/instances/{id}/snapshots` | — | `Snapshot[]`, newest first |
| POST | `/api/instances/{id}/snapshots/compare` | `{"a":"<id>","b":"<id>"}` | `SnapshotDiff` |
| GET | `/api/snapshots/{sid}` | — | `Snapshot`, `404` unknown |
| DELETE | `/api/snapshots/{sid}` | — | `204`, files removed too |
| POST | `/api/snapshots/{sid}/restore` | — | `200 RestoreReport`, `409 restore_failed` (already rolled back) |
| DELETE | `/api/snapshots/expired` | — | `{"removed":N,"failed":M}` |

```json
{"id":"0f0e…","instance_id":"alice",
 "path":"/opt/fleett/snapshots/0f0e…/20261109-183512.tgz",
 "label":"before upgrade","created_at":"2026-11-09T18:35:12Z",
 "expires_at":"2026-11-16T18:35:12Z","size_bytes":1048576,
 "status":"ok","error":null}
```

`status`: `ok`, `restoring` (a restore is in progress) or `failed` (the last
restore was rolled back; `error` carries the reason). `ttl_s` is
`1..=7776000` (90 days), default 7 days.

`SnapshotDiff`:

```json
{"instance_id":"alice","a":"<id>","b":"<id>",
 "added":[{"path":"data/NEW.md","size":12,"sha256":"…"}],
 "removed":[{"path":"data/OLD.md","size":8,"sha256":"…"}],
 "changed":[{"path":"data/SOUL.md","size_a":210,"size_b":212,
             "sha256_a":"…","sha256_b":"…"}],
 "unchanged":42}
```

`RestoreReport`:

```json
{"snapshot":{…},"instance_id":"alice","files":37,"status":"running",
 "env_restored":true,"compose_restored":true,
 "previous_data_dir":"/opt/fleett/instances/alice/data.pre-restore-20261109-184001"}
```

A restore stops the instance, moves `data/` to a `.pre-restore-<ts>` sibling
(kept, never deleted), extracts the archive, restores the compose file and env
rows, starts the instance and waits up to 180 s for a healthy container. On any
failure it puts the previous data directory, compose file and env rows back,
restarts the instance and answers `409 restore_failed` with
`error.details {"snapshot","rolled_back":true}`. An `auto:` snapshot is taken
before upgrades, environment writes and git reverts (best effort: a failed
snapshot never blocks the operation).

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

### Egress destination policy

Per-instance allowlist/blocklist of destinations, enforced by an operator-run
Squid proxy. Full model and enforcement limits: [docs/EGRESS-POLICY.md](docs/EGRESS-POLICY.md).
An instance without a policy keeps the legacy `allow_lan` / `FLEETT_EGRESS`
behaviour; `GET …/egress` always returns that effective projection with
`source` set to `explicit`, `allow_lan`, `legacy` or `disabled`.

| Method | Route | Body | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/egress` | — | `{instance,source,mode,fqdns,cidrs,ports,updated_at?,proxy?,proxy_reachable?,proxy_error?,proxy_acl?,legacy_equivalent,warnings}` |
| PUT | `/api/instances/{id}/egress` | `{"mode":"deny\|allow","fqdns":[],"cidrs":[],"ports":[],"proxy":"http://…"}` | validate, probe the rendered proxy, store, rewrite compose, restart; `400 invalid_egress_policy`; `422 proxy_unreachable` (`details {instance,proxy,hint}`) before any write when it does not answer; audit `egress.policy.update` |
| DELETE | `/api/instances/{id}/egress` | — | drop the explicit policy (back to legacy); `404` when none; audit `egress.policy.delete` |
| POST | `/api/instances/{id}/egress/test` | `{"host":"api.github.com","port":443}` | `{allowed,reason,source,mode,enforced,enforcement?,proxy?}` |
| GET | `/api/instances/{id}/egress/blocks?limit=200` | — | `[{id,instance_id,host,port,reason?,at}]`, newest first |
| DELETE | `/api/instances/{id}/egress/blocks` | — | `{"removed":N}`; audit `egress.blocks.clear` |
| GET | `/api/egress/summary` | — | `{instances:[{instance,policy:"none\|legacy\|enforced",mode,fqdns,cidrs,ports,proxy?,proxy_reachable?,proxy_error?,source,warnings}]}`; readable instances only, one probe per distinct proxy |
| GET | `/api/egress/blocks?instance=&limit=` | — | admin view across the fleet |
| POST | `/api/egress/blocks` | `{"instance","host","port","reason?"}` | `201` refused-destination record; token required, 120/min/instance |

`mode: "deny"` is an allowlist (empty policy denies everything);
`mode: "allow"` is a blocklist (empty policy allows everything); `ports` empty
means every port. `fqdns` are lowercased; a leading `*.` matches subdomains
only. A policy adds `HTTP_PROXY=http://fleett-egress-proxy:3128`,
`HTTPS_PROXY` and `NO_PROXY` to the container; `FLEETT_EGRESS_PROXY` overrides
the address, and an empty/`off` value renders no proxy variable.

`POST …/egress/test` never pretends: `allowed` is the policy decision, while
`enforced` is true only when the proxy TCP endpoint answers (explicit policy,
instance not on `fleett-lan`) or the host firewall marker matches (legacy
`restrict`). Every other case returns `enforced:false` with the reason. The
socket probe does not verify that the proxy ACLs match the stored policy.

`POST /api/egress/blocks` is authenticated (bearer or session, so a `viewer`
token cannot POST), validates the host/port/reason, answers `404` for an unknown
instance and `429 rate_limited` beyond 120 records per instance per minute.

`GET /api/egress/summary` is the fleet-wide projection: `policy` is `enforced`
when an explicit policy is stored, `legacy` when only `allow_lan` / the host
firewall applies, `none` under `FLEETT_EGRESS=off`. It is filtered by the same
instance authorization as `GET /api/instances` (a scoped token or a viewer
session only sees its instances) and probes each distinct proxy URL at most
once. `proxy` and `proxy_reachable` are `null` when no proxy is rendered;
`proxy_error` carries the socket error when `proxy_reachable` is `false`.

`PUT …/egress` refuses a policy whose rendered proxy does not answer with
`422 proxy_unreachable` before writing the row or recreating the container;
`error.details` is `{instance,proxy,hint}` where `hint` is the deploy command
(`docker compose -f docker/fleett-proxy.example.yaml up -d`). The same guard
applies to every other recreation path (patch, env, clone, import, shares).
`GET …/egress` reports a proxy that died after the write through
`proxy_reachable:false` + `proxy_error`, without failing the read.

`POST /api/instances/{id}/start|stop|restart` → `200 {"status":"running|stopped"}`
`DELETE /api/instances/{id}` → `204` (data preserved) · `?purge=true` erases `data/`.
Requires the `owner` level on the instance (or the admin role): `operator` may write but not
destroy.

`POST /api/instances/{id}/clone` → `201 InstanceDetail` (admin only). Body:
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

## Exec

One-shot command execution inside an instance container, **admin only** (including the SSE
route, which is a `GET`). Full security model in [docs/EXEC.md](docs/EXEC.md): argv only
(no shell), allowlist of basenames, hard denylist, basename-only names, 1..=300 s timeout,
256 KiB per stream and an `instance.exec` audit row for every attempt.

| Method | Route | Body | Response |
|---|---|---|---|
| POST | `/api/instances/{id}/exec` | `{"argv":["ls","/opt/data"],"timeout_s":5}` or `{"cmd":"df -h","timeout_s":5}` | `{"exit_code":0,"stdout":"…","stderr":"","timed_out":false,"duration_ms":12,"truncated":false}`; `403 exec_refused` (details: `reason`, `argv`, `allowlist`, `denylist`, `wildcard`); `400 invalid_exec`/`invalid_timeout`; `404`; `502 driver_error` |
| GET | `/api/instances/{id}/exec/stream?cmd=ls%20%2Fopt%2Fdata&timeout_s=5` | — | `text/event-stream`: `event: stdout` / `event: stderr` with `data: {"line":"…"}`, then `event: exit` with `{"exit_code","timed_out","duration_ms","truncated"}`; a refusal is a JSON `403 exec_refused` before any event |
| GET | `/api/exec/policy` | — | `{"wildcard":false,"source":"default","allowlist":[…],"denylist":[…],"max_args":32,"max_arg_bytes":4096,"max_output_bytes":262144,"timeout_s":{"default":30,"min":1,"max":300}}`, admin only |

`cmd` is split on whitespace and never given to a shell; it refuses `; | & $ ` < > ( ) \ " '`
and newlines (`400 invalid_exec`). Send `argv` for an argument containing a space or a quote.
Default allowlist: `ls cat tail head grep env ps df du date which pwd id uname uptime free echo`
(`FLEETT_EXEC_ALLOWLIST` replaces it, `*` disables it while the denylist stays in force).
`env` accepts no argument in allowlist mode. A timeout answers `200` with
`"timed_out":true` and `"exit_code":null` (the streams are empty); it is not an HTTP error.
The SSE route is **one-shot**: the command runs to completion, then the captured output is
emitted and the stream closes — there is no stdin, PTY or session, and `EventSource` cannot
send a bearer header (use `fetch`). See `docs/EXEC.md`.

`GET /api/instances/{id}/export` → `application/gzip`, `Content-Disposition: attachment;
filename="<id>.tgz"`. A gzipped tar with `manifest.json` (format/version, `created_at` and the
instance metadata: id, name, template, model, base_url, api_key_env, channel, mem_limit_mb,
cpu_limit, allow_lan, shares, notes), `compose.yaml` and `data/` (the full Hermes home).
Share contents and secrets are never included: no `secrets.env`, no `*.key`, no token value.

`POST /api/instances/import?id=<new_id>&start=false` → `201 InstanceDetail` (admin only). The raw gzip
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

## Cron schedules

Per-agent cron schedules and their run history; see [docs/SCHEDULES.md](docs/SCHEDULES.md)
for cron semantics, overlap, catch-up and the run command contract.

| Method | Route | Body | Response |
|---|---|---|---|
| POST | `/api/instances/{id}/schedules` | `{"name","cron","prompt","enabled"?,"timezone"?,"overlap"?,"timeout"?,"catchup"?}` | `201 Schedule` · `400 invalid_cron\|invalid_timeout\|invalid_overlap\|…` · `404 unknown_instance` · `409 schedule_exists` |
| GET | `/api/instances/{id}/schedules` | — | `[Schedule]`, ordered by name |
| PATCH | `/api/schedules/{sid}` | every create field, all optional | `200 Schedule` · `404 not_found` |
| DELETE | `/api/schedules/{sid}` | — | `204` (history included) · `404` |
| POST | `/api/schedules/{sid}/run` | — | `201 ScheduleRun` (executed inline) · `409 run_in_progress` |
| GET | `/api/instances/{id}/runs?limit=50` | — | `[ScheduleRun]`, newest first, `limit` `1..=500` |
| GET | `/api/schedules` | — | `[ScheduleView]`, admin only |

`Schedule`:
```json
{"id":"2b6d…","instance_id":"alice","name":"every-five","cron":"*/5 * * * *",
 "prompt":"summarize","enabled":true,"timezone":"UTC","overlap":"skip",
 "timeout":300,"catchup":false,
 "last_run_at":"2026-11-09T10:05:00Z","next_run_at":"2026-11-09T10:10:00Z",
 "created_at":"…","updated_at":"…"}
```

`cron` is a five-field expression evaluated in UTC (`*`, `*/n`, lists, ranges,
stepped ranges; day-of-month and day-of-week are ORed when both are
restricted). `overlap` is `skip` (default; a due occurrence is recorded as
`skipped` while the previous run is still going) or `queue` (it waits for the
next tick after the run finishes). `timeout` is `1..=3600` seconds (default
300; `timeout_s` is accepted as an alias). `catchup` defaults to `false`: at
startup a schedule whose slot is long past is moved to its next future
occurrence; `true` fires exactly one catch-up run. `timezone` is stored but
evaluation is UTC for now. A disabled schedule has no `next_run_at`.

`ScheduleRun.status`: `running`, `ok`, `error`, `timeout` or `skipped`.
`exit_code` is the command exit code (`null` on timeout or Docker failure).
`output` combines stdout and stderr, truncated to 64 KiB. `duration_ms` is the
wall-clock duration of the command.

`ScheduleView` is a `Schedule` flattened with
`{"last_run": ScheduleRun|null, "runs_total": 12, "runs_failed": 2,
"failure_rate": 0.1667}`. Failures are `error` and `timeout`; `skipped` and
`running` runs are not counted in the rate. Deleting an instance removes its
schedules and run history.
## Watchdog and incidents

Self-healing is opt-in per instance; see [docs/WATCHDOG.md](docs/WATCHDOG.md).
The watchdog fills the registry from `docker ps`/`docker inspect` every 30 s
(`FLEETT_WATCHDOG_INTERVAL_S`).

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/incidents?status=open\|closed&instance=&limit=` | — | `Incident[]`, newest first (limit 1…500, default 100) |
| GET | `/api/instances/{id}/incidents?limit=` | — | `Incident[]` of one instance |
| POST | `/api/incidents/{id}/close` | — | `Incident` · `409 incident_closed` when already closed (admin) |
| PATCH | `/api/instances/{id}/heal` | `{"heal":"off\|restart\|recreate","unhealthy_after":3,"max_restarts":5,"heal_backoff_s":30}` | policy view (admin) |

```json
{
  "id": "6f1c…",
  "instance_id": "alice",
  "cause": "unhealthy",
  "status": "open",
  "opened_at": "2026-11-09T10:00:00Z",
  "detail": "container health check failed for 3 consecutive sample(s) (threshold 3)",
  "actions": [{"at": "2026-11-09T10:00:30Z", "action": "restart", "result": "ok"}],
  "restart_count": 0,
  "last_seen_at": "2026-11-09T10:00:30Z"
}
```

`cause`: `oom` · `crashloop` · `unhealthy` · `image_missing` · `heal_failed`.
`status`: `open` · `closed`; `closed_at` is present once closed. A clean stop
never opens an incident, and an incident closes by itself on the first healthy
sample. Both `PATCH …/heal` and the manual close are written to the audit log
(`instance.heal`, `incident.close`). Invalid policy values answer
`400 invalid_heal_policy`, `400 invalid_unhealthy_after`,
`400 invalid_max_restarts` or `400 invalid_heal_backoff_s`.

## Notifications

Outbound alert channels; see [docs/NOTIFICATIONS.md](docs/NOTIFICATIONS.md). Secrets are
encrypted and never returned (`has_secret` only).

| Method | Path | Body / query | Result |
|---|---|---|---|
| GET | `/api/notifications` | — | `Channel[]` |
| POST | `/api/notifications` | `{"name","kind":"webhook|discord|telegram|smtp","target","secret?","events":["instance.down","incident.*"],"enabled?":true}` | `201 Channel` |
| GET/PATCH/DELETE | `/api/notifications/{id}` | partial `Channel` | `200` / `204` |
| POST | `/api/notifications/{id}/test` | — | `202 Delivery` (synthetic `notification.test`) |
| GET | `/api/notifications/deliveries?channel=&limit=` | — | `Delivery[]` |

Events: `instance.down`, `instance.unhealthy`, `instance.up`, `upgrade.failed`,
`quota.exceeded`, `egress.drift`, `secret.expiring`, `incident.open`, `incident.close`,
`notification.test`; `*` and `incident.*` are accepted. Deliveries are coalesced per
channel/event over five minutes and retried with backoff.

## Webhooks

Signed outbound event deliveries (distinct from notifications); see
[docs/WEBHOOKS.md](docs/WEBHOOKS.md).

| Method | Path | Body / query | Result |
|---|---|---|---|
| GET | `/api/webhooks` | — | `Webhook[]` (`has_secret`, never the secret) |
| POST | `/api/webhooks` | `{"name","url","events":["instance.started"],"secret"}` | `201 Webhook` |
| GET/PATCH/DELETE | `/api/webhooks/{id}` | partial | `200` / `204` |
| POST | `/api/webhooks/{id}/test` | — | `202 Delivery` |
| GET | `/api/webhooks/{id}/deliveries` | — | `Delivery[]` |
| GET | `/api/openapi.json` | — | OpenAPI 3.0.3 document |

Each delivery carries `X-Fleett-Event`, `X-Fleett-Delivery` and
`X-Fleett-Signature: sha256=<hex HMAC-SHA256 of the exact body>`. Events:
`instance.created|started|stopped|deleted`, `env.changed`, `upgrade.*`, `incident.*`,
`secret.rotated`, `pipeline.run.*`.

## Log center

Persistent searchable logs; see [docs/LOGS.md](docs/LOGS.md).

| Method | Path | Query | Result |
|---|---|---|---|
| GET | `/api/logs` | filters below | `{"entries":[…],"count":n,"limit":n}`, newest first |
| GET | `/api/logs/instances/{id}` | filters below | same envelope, restricted to one instance |
| GET | `/api/logs/stream` | `instances=` | SSE `log` events carrying an `id:` field, 15 s heartbeat |
| GET | `/api/logs/export` | `format=json\|csv` + filters | downloadable file (`json` or `csv`) |

| Parameter | Meaning |
|---|---|
| `instances` | Comma-separated ids, at most 100; empty means every instance. |
| `q` | FTS5 query, with a substring `LIKE` fallback for partial words. |
| `level` | `info`, `warn` (warnings **and** errors) or `error`. |
| `since` / `until` | RFC 3339 timestamp or relative window (`30m`, `24h`, `7d`). `since` must be before `until`. |
| `after_id` | Only entries with a strictly greater id; the live-follow cursor. |
| `limit` | `1..=500`, default 100. |
| `format` | Export only: `json` (default) or `csv`. |

`400` error codes: `invalid_limit`, `invalid_level`, `invalid_since`,
`invalid_until`, `invalid_window`, `invalid_instance`, `too_many_instances`,
`invalid_query`, `invalid_after_id`, `invalid_format`.

Secrets are redacted at ingestion (`flt_`, `sk-`, `Bot`, `Bearer`, `token=`). Retention is
`FLEETT_LOG_RETENTION` (default `7d`) and `FLEETT_LOG_MAX_ROWS` (default 500000).

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
 "caps":{"max_cpu":8.0,"max_mem_mb":16384},
 "warnings":[]}
```

Instances are grouped by `host` and each daemon is sampled once. An unreachable host yields
an entry in `warnings` (`host "pve3" is unreachable: …`) and zeroed numbers for its instances
instead of failing the whole report; `200` is returned as long as at least one host answered.

With `FLEETT_DRIVER=compose`, `cpu_pct`/`mem_used_bytes` come from one
`docker stats --no-stream --format json` call per host (containers are matched by name),
`disk_bytes` from `du -sb` on `instances/<id>/data` (the controller-side path the target must
share), and `mem_limit_bytes` from the container limit (or the configured limit when the
container is absent). Stopped instances report zero CPU and memory.
`totals` sums every instance; `caps` echoes `FLEETT_MAX_TOTAL_CPU` and
`FLEETT_MAX_TOTAL_MEM_MB` (`0` = unlimited). An empty fleet answers zeros without touching
Docker. Under `FLEETT_DRIVER=mock` the numbers are stable simulations.

The fleet is also checked by `fleett doctor`, which reads `docker`, `secrets` and `egress`
from `/api/health` plus `GET /api/auth/me`; see `docs/CLI.md`.

## Tokens and costs

Token accounting, model prices and monthly budgets ([docs/COST.md](docs/COST.md)).

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/cost?from=&to=&group_by=instance\|model\|day` | — | `200 CostReport` · `400 invalid_range` / `invalid_group_by` |
| GET | `/api/cost/budgets` | — | `200 [BudgetStatus]` |
| POST | `/api/usage/ingest` | `{"instance":"alice","lines":["…"]}` or `{"instance":"alice","prompt_tokens":1200,"completion_tokens":300}` | `200 IngestSummary` · `404` unknown instance · `400` invalid payload |
| GET | `/api/models/pricing` | — | `200 [Pricing]` (latest version per model) |
| POST | `/api/models/pricing` | `{"model":"m","input_per_mtok":0.27,"output_per_mtok":1.1,"currency":"USD","effective_from":"2026-11-01"}` | `200 Pricing` · `400 invalid_pricing` |
| DELETE | `/api/models/pricing/{model}` | — | `204` · `404` unknown model |
| GET | `/api/instances/{id}/budget` | — | `200 BudgetView` (`configured:false` when unset) |
| PUT | `/api/instances/{id}/budget` | `{"monthly_usd":25,"enabled":true}` | `200 BudgetView` · `400 invalid_budget` |

`GET /api/cost` defaults to the current month up to now. `from` is inclusive;
a bare date for `to` covers the whole day, an RFC 3339 `to` is exclusive.
`group_by` accepts `instance` (default), `model` and `day`.

```json
{"from":"2026-11-01T00:00:00Z","to":"2026-11-09T03:00:00Z","group_by":"instance",
 "currency":"USD",
 "totals":{"prompt_tokens":1500,"completion_tokens":500,"total_tokens":2000,
           "cost_usd":0.006,"events":2},
 "groups":[{"key":"alice","prompt_tokens":1500,"completion_tokens":500,
            "total_tokens":2000,"cost_usd":0.006,"events":2}],
 "top_agents":[{"instance_id":"alice","cost_usd":0.006,"total_tokens":2000}],
 "budgets":[{"instance_id":"alice","month":"2026-11","monthly_usd":25.0,
             "enabled":true,"used_usd":0.006,"percent":0.024,"exceeded":false,
             "projected_month_usd":0.1,"updated_at":"2026-11-09T02:00:00Z"}],
 "projection":{"month":"2026-11","elapsed_fraction":0.27,
               "month_to_date_usd":0.006,"projected_usd":0.022}}
```

`IngestSummary` is `{"parsed":1,"inserted":1,"duplicates":0,"ignored":0}`: a
replayed payload parses but inserts nothing. `Pricing` is
`{"model":"…","input_per_mtok":0.27,"output_per_mtok":1.1,"currency":"USD","effective_from":"2026-11-01","updated_at":"…"}`.

Prices are **versioned by `effective_from`**: a new effective date adds a row
and the historical report is recomputed with the price effective on each event
day. `DELETE /api/models/pricing/{model}` removes every version. A budget past
its limit produces a `cost.budget` event through `cost::events::emit` (a WARN
log line until the incident pipeline is merged). Reads are open to viewers;
every write is admin-only.

## Disk quotas and cleanup

See [QUOTA.md](QUOTA.md) for the semantics (80 % warning, 100 % policy) and the
cleanup rules. Reads accept any authenticated role; mutations need `admin` and
are audited as `instance.quota.update` / `instance.quota.cleanup`.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/instances/{id}/quota` | — | `{"instance":"alice","quota_mb":2048,"policy":"freeze","used_bytes":52428800,"quota_bytes":2147483648,"percent":2.44,"state":"ok","warn_percent":80,"last_event":{…}}` |
| PUT | `/api/instances/{id}/quota` | `{"quota_mb":2048,"policy":"freeze"}` (each field optional) | the same view; `400 invalid_quota` / `400 invalid_policy`, `404` unknown instance |
| GET | `/api/instances/{id}/usage/detail` | — | `{"instance":"alice","data_dir":"…","data":{"total_bytes":…,"tmp_bytes":…,"cache_bytes":…,"logs_bytes":…,"other_bytes":…,"file_count":…},"docker_log_path":"…","docker_log_bytes":…,"image":"…","image_bytes":…,"quota":{…},"last_event":{…}}` |
| POST | `/api/instances/{id}/quota/cleanup` | `{}` | `{"instance":"alice","before_bytes":…,"after_bytes":…,"freed_bytes":…,"removed_files":3,"removed_paths":["cache","logs/agent.log.1","tmp"]}` |
| GET | `/api/disk/events?instance=&limit=50` | — | `[{"id":1,"instance":"alice","at":"…","action":"warn","before_bytes":…,"after_bytes":…,"detail":"…"}]`, newest first, `limit` 1..=500 |

`state` is `ok` (< 80 %), `warn` (>= 80 %) or `exceeded` (>= 100 %). `used_bytes`
comes from the driver sample (`du`); the detail endpoint falls back to a
filesystem walk when Docker is unreachable. `docker_log_path`/`docker_log_bytes`
are omitted when Docker does not report a json-file log. `action` is one of
`warn`, `freeze`, `stop`, `clean`.

## Images and rolling upgrades

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/images` | — | `[{"tag":"hermes-agent:0.22.2-local","id":"sha256:…","size":"1.2GB","created":"…","instances":["alice"]}]` — the union of every instance host; an unreachable host is skipped |
| POST | `/api/upgrade` | `{"image":"<tag>","parallel":1,"dry_run":false,"timeout_s":180,"canary":0,"soak_s":600,"at":"sun 04:00","window":"nightly"}` | `202 UpgradeJob` · `202 Deployment` when `at` is in the future · `400 invalid_*` · `403 maintenance_window` · `409 upgrade_in_progress` · `502 image_unavailable` (the tag is ensured on every instance host before the job starts) |
| GET | `/api/upgrade` | — | `200 UpgradeJob` (current or last) · `404` before the first job |

`UpgradeJob`:
```json
{"id":"2b6d…","image":"hermes-agent:0.23.0-local",
 "started_at":"2026-11-08T01:00:00Z","finished_at":"2026-11-08T01:04:12Z",
 "dry_run":false,"parallel":1,"timeout_s":180,"canary":1,"soak_s":600,
 "deployment_id":"2b6d…",
 "results":[{"id":"alice","from":"hermes-agent:0.22.2-local",
             "to":"hermes-agent:0.23.0-local","status":"healthy"},
            {"id":"bob","from":"hermes-agent:0.22.2-local",
             "to":"hermes-agent:0.23.0-local","status":"rolled_back",
             "error":"upgrade failed: the container reported unhealthy; rolled back to hermes-agent:0.22.2-local"}]}
```

`status` per instance: `pending`, `upgrading`, `healthy`, `rolled_back`, `failed`. `from` is
`null` only when the instance had no image recorded yet. `finished_at` is absent while the job
runs and on a dry run is equal to `started_at`. `canary` and `soak_s` are additive fields
(`0`/`0` for a plain rolling upgrade); `deployment_id` is absent on a dry run.

`POST /api/upgrade` pulls the image first (`docker image inspect`, then `docker pull`) and
answers `502 image_unavailable` with the Docker detail when it cannot; nothing is written in
that case, but the failed attempt is journaled as a `failed` deployment. Requests are
validated: `image` is required (`400 invalid_image`), `parallel` is `1..=16` (default `1`,
i.e. sequential), `timeout_s` is `1..=3600` (default `180`), `canary` is at most the fleet size
(`400 invalid_canary`), `soak_s` is `0..=3600` (default `600`, `400 invalid_soak`) and `window`
must name an enabled window (`400 invalid_window`). A second job while one runs answers
`409 upgrade_in_progress`, whatever the body. Admin only: a viewer gets `403 forbidden` like
every other mutation.

Before the pull, the maintenance gate runs: when at least one enabled window exists and none is
open, the request answers `403 maintenance_window` with `details.next_open` (see below). A past
`at` is `400 invalid_at`.

With `canary: N`, the first N instances move first and must stay healthy for `soak_s` seconds
(probed every 10 s) before the rest moves; a canary that fails — during its own upgrade or
during the soak — stops the deployment and the other instances stay untouched (`pending`, with
`error: "not attempted: …"`). A canary that degrades during the soak is rolled back to its
previous image. See `docs/DEPLOYMENTS.md`.

With a future `at` (RFC 3339 or `sun 04:00`, UTC), nothing is pulled now: the response is
`202` with the `Deployment` row (`status: "scheduled"`, `started_at` = planned time,
`result.request` = the deferred body). The server fires it within 30 s of the due time; the
window is re-evaluated then (a closed window `cancelled` the row).

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
`docs/UPGRADE.md` for the operator procedure and `docs/DEPLOYMENTS.md` for the windows, canary
and journal details.

## Maintenance windows

Recurring UTC day/time ranges that gate destructive operations (`POST /api/upgrade` and
`DELETE /api/instances/{id}`). Enforcement is inert while no enabled window exists;
`FLEETT_MAINTENANCE_ENFORCE=0` disables it entirely. Admin only for mutations.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/maintenance-windows` | — | `[{"id":"…","name":"nightly","days":["mon","tue"],"start_minute":120,"end_minute":300,"enabled":true,"created_at":"…","updated_at":"…"}]` |
| POST | `/api/maintenance-windows` | `{"name":"nightly","days":["mon","tue"],"start_minute":120,"end_minute":300,"enabled":true}` | `201` window · `400 invalid_window` |
| PATCH | `/api/maintenance-windows/{id}` | any subset of the fields | `200` window · `400 invalid_window` · `404` |
| DELETE | `/api/maintenance-windows/{id}` | — | `204` · `404` |
| GET | `/api/maintenance-windows/status` | — | `{"open":false,"next_open":"2026-11-10T02:00:00Z","enforce":true}` |

`days` accepts a JSON array or a `"mon,tue"` string. Minutes are UTC (`0..=1439`), `start` is
inclusive and `end` exclusive; `end < start` is an overnight window owned by its start day and
`start == end` covers the whole day. The status `open` field is `true` when a destructive
operation would be allowed right now (`next_open` is then `null`). A refused operation is
`403 maintenance_window` with `details.next_open`:

```json
{"error":{"code":"maintenance_window",
          "message":"no maintenance window is open; the next opening is 2026-11-10T02:00:00Z",
          "details":{"next_open":"2026-11-10T02:00:00Z"}}}
```

## Deployments

Durable journal of every upgrade (immediate, canary or scheduled), newest first.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/deployments?limit=50&status=ok` | — | `[Deployment]` · `400 invalid_limit` · `400 invalid_status` |
| GET | `/api/deployments/{id}` | — | `200 Deployment` · `404` |

`Deployment`:
```json
{"id":"0b7e…","image":"hermes-agent:0.23.0-local","status":"ok",
 "instances":["alice","bob"],"canary":1,"soak_s":600,
 "started_at":"2026-11-09T03:00:00Z","finished_at":"2026-11-09T03:12:40Z",
 "result":[{"id":"alice","from":"hermes-agent:0.22.2-local",
            "to":"hermes-agent:0.23.0-local","status":"healthy"}],
 "error":null,"rollback":false,"created_by":"cleeryy"}
```

`status`: `pending`, `canary`, `running`, `scheduled`, `ok`, `failed`, `rolled_back`,
`cancelled`. `limit` is `1..=500` (default `50`). While a row is `scheduled`, `result` holds
`{"request":{…}}` (the deferred `POST /api/upgrade` body) and `started_at` is the planned
time; after firing it holds the per-instance results and `started_at` is the real start.
`rollback` is `true` when at least one instance was restored to its previous image.

`docs/UPGRADE.md` for the operator procedure.

## MCP server (client-side)

This document describes the control plane routes; `fleett mcp` adds **no API route**. It is a
client of the routes above: every MCP tool is one authenticated API call made with the token
of `~/.config/fleett/config.toml` (`FLEETT_URL` / `FLEETT_TOKEN`), so the role check, the audit
entry of a mutation and the error envelope are exactly the ones documented here. A
`403 forbidden` from the API is returned to the MCP client as a tool error carrying the API
message.

`fleett mcp --http <addr>` is a separate, optional listener that serves JSON-RPC on
`POST /mcp`; it is not part of the control plane router and is not proxied by `/api`. It has
no authentication of its own: it acts with the token of its own process, so bind it to
loopback (or front it with an authenticating proxy). SSE streaming is not implemented.

See [docs/MCP.md](docs/MCP.md) for the tool catalog, the Hermes configuration and the
limitations.

## MCP marketplace

MCP servers fleett deploys, discovers and attaches to agents (issue #62). See
[docs/MCP-SERVERS.md](docs/MCP-SERVERS.md). Reads accept any authenticated role;
every mutation requires `admin`. No endpoint ever returns an environment value,
a header value, an image registry credential or a compose environment value.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/mcp/catalog?q=` | — | `[{id,name,description,kind,image,command,port,path,env:[{key,secret,required}],tags}]` merged from the built-ins and `$FLEETT_ROOT/marketplace/mcp/index.json` |
| POST | `/api/mcp/catalog/import` | one catalogue entry | `201` entry; `409 mcp_duplicate`, `400 invalid_mcp_server`, admin only (`POST /api/mcp/catalog` is an alias) |
| DELETE | `/api/mcp/catalog/{id}` | — | `204` for an imported entry; a built-in answers `400 invalid_mcp_server`, admin only |
| GET | `/api/mcp/servers` | — | `[{id,name,description,kind,image,command,port,path,persist,cpu_limit,mem_limit_mb,host,status,health,last_error,last_probe_at,endpoint,tools,tools_count,tools_hash,env:[{key,secret,set}],headers:[name],instances,created_at,updated_at}]` |
| POST | `/api/mcp/servers` | `{"catalog":"fetch"}` or a complete spec (`id`,`name`,`kind`,`image`,`command`,`port`,`path`,`persist`,`cpu_limit`,`mem_limit_mb`,`host`,`env`,`headers`) | `201` server view; deploy + discovery, `409 mcp_duplicate`, `502 mcp_upstream`, admin only |
| GET | `/api/mcp/servers/{id}` | — | server view |
| PATCH | `/api/mcp/servers/{id}` | `{name?,description?,image?,command?,port?,path?,persist?,cpu_limit?,mem_limit_mb?,host?,headers?:{…}\|null}` | server view; a spec change does not redeploy automatically, admin only |
| DELETE | `/api/mcp/servers/{id}?purge=` | — | `204`; `409 mcp_server_in_use` with `details.instances`, admin only |
| POST | `/api/mcp/servers/{id}/start\|stop\|restart` | — | server view after refresh (and discovery for start/restart), admin only |
| POST | `/api/mcp/servers/{id}/test` | — | `{server,reachable,latency_ms,tools,tools_count,tools_hash,protocol_version,server_info}`; `422 mcp_unreachable` with the upstream detail, admin only |
| GET | `/api/mcp/servers/{id}/logs?tail=&since=` | — | plain-text log tail |
| GET | `/api/mcp/servers/{id}/tools` | — | `{server,tools,count,hash,probed_at}` |
| PUT | `/api/mcp/servers/{id}/env` | `{"env":[{key,value,secret}]}` | server view; redeploys a running container to apply the values, admin only |
| DELETE | `/api/mcp/servers/{id}/env/{key}` | — | server view; `404` when the variable is absent, admin only |
| GET | `/api/mcp/servers/{id}/incidents?status=&limit=` | — | `[{id,server_id,cause,status,opened_at,closed_at,detail,actions,restart_count,last_seen_at}]` |
| POST | `/api/mcp-incidents/{id}/close` | — | closed incident; `404` when unknown or already closed, admin only |
| GET | `/api/instances/{id}/mcp-servers` | — | `{id,status,servers:[{server,mode,kind,status,health,tools_count,endpoint}]}` |
| PUT | `/api/instances/{id}/mcp-servers` | `{"servers":[{server,mode?}]}` | replaces the list, rewrites `config.yaml` and recreates the agent, admin only |
| POST | `/api/instances/{id}/mcp-servers/{server}` | `{"mode"?: "readonly\|rw"}` | attach one server, recreate the agent, admin only |
| DELETE | `/api/instances/{id}/mcp-servers/{server}` | — | detach one server, recreate the agent, admin only |

`mode` is stored in `instance_mcp_servers` and reported by the API. The managed
`mcp_servers` block of `config.yaml` uses `fleett-<server>` keys: a
`streamable-http` entry is `{url,timeout,connect_timeout}`, a `stdio` entry is
`{command,args,env}` with `${KEY}` references. Attaching or detaching an agent
never deploys a second container: the server is shared by Docker DNS on the
`fleett` network.
