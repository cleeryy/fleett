# Providers — reusable API credentials

A **provider** is a named API credential that several agents can share. Before
this feature every instance owned its token: cloning an agent demanded a new
token (`400 clone_token_required`) and rotating one meant patching each agent.
Now one `PATCH` rotates the credential for every attached instance.

Instances created before the feature keep working unchanged: an instance with
no `provider` still stores and injects its own `gate_token`/`api_key_env`, and
nothing is migrated automatically.

## Model

`providers` (SQLite, schema v8):

| Column | Meaning |
|---|---|
| `id` | UUID, used by the API and by `instances.provider` |
| `name` | unique slug (`[a-z0-9-]{2,63}`) |
| `kind` | `openai-compatible`, `anthropic` or `google` — metadata for the console; every kind is probed through `/models` |
| `base_url` | endpoint, e.g. `https://opencode.ai/inference/go/openai/v1` |
| `api_key_env` | container variable the secret is injected under, e.g. `HERMES_GATE_PROFILE_TOKEN` |
| `nonce`, `ciphertext` | AES-256-GCM blob of the secret (same key as the instance environment) |
| `created_at`, `updated_at` | RFC 3339 UTC |

An instance references a provider with `provider: "<id>"`. `model` stays
per instance. `base_url` and `api_key_env` default to the provider's values
when the instance does not set them; an explicit value in the same request
wins, so one instance can point at another endpoint while sharing the key.

The container contract does not change: the compose file always declares
`${API_KEY_ENV}` and the compose driver injects the decrypted value into the
`docker compose` child environment (the provider secret overrides any
per-instance value of that key). Nothing secret is ever written to disk.

## API

All mutations require the `admin` role; `viewer` may `GET`. The secret is
returned by **no** endpoint.

| Method | Route | Notes |
|---|---|---|
| `GET` | `/api/providers` | `[{id,name,kind,base_url,api_key_env,instances_count,created_at,updated_at}]` |
| `POST` | `/api/providers` | `{name,kind,base_url,api_key_env,secret}` → `201` summary |
| `GET` | `/api/providers/{id}` | summary + `instances:[{id,name}]` |
| `PATCH` | `/api/providers/{id}` | `{name?,kind?,base_url?,api_key_env?,secret?}` → `{provider, restarted:[{id,status,error?}]}` |
| `DELETE` | `/api/providers/{id}` | `204`, or `409 provider_in_use` with `details.instances` |
| `POST` | `/api/providers/{id}/test` | probes `${base_url}/models`, 5 s timeout → `{ok,status,latency_ms,error?}` |

`instances_count` counts the instances whose `provider` is this id.
Instance summaries and details expose `provider` (`null`/absent for a legacy
instance) and keep `base_url`/`api_key_env` as the effective values.

## Rotation

`PATCH /api/providers/{id}` with a `secret` re-encrypts it with a fresh nonce,
then restarts every attached instance **sequentially**:

1. `docker compose up -d` with the new secret in the child environment;
2. the rolling-upgrade health wait (`fleett upgrade` logic) polls
   `docker compose ps` until the container is `healthy` (120 s per instance);
3. the outcome is recorded per instance: `healthy`, or `failed` with the
   reason (status becomes `error` on the instance).

A failing instance never blocks the others: the loop continues, and the
response is a report (`restarted`) the console can display. The old secret is
not kept, so a failed instance stays on the new credential and must be fixed
(the error is in its `last_error` and in the report); there is no automatic
rollback of the secret itself.

`kind` may also be patched, though it is metadata only.

## Test endpoint

`POST /api/providers/{id}/test` decrypts the secret and performs one
`GET ${base_url}/models` with:

* `Authorization: Bearer <secret>`
* `x-api-key: <secret>` and `anthropic-version: 2023-06-01` (harmless for
  OpenAI-style endpoints, required by Anthropic)

The response reports `ok` (2xx), `status`, `latency_ms` and an `error` string
on failure or non-2xx. It never contains the key, the request URL is the only
other detail.

## Migrating existing per-instance tokens

No forced migration: old instances keep their own tokens.

To move one instance onto a shared credential:

1. `POST /api/providers` with the token you already use (the secret is
   encrypted at rest);
2. `PATCH /api/instances/{id}` with `{"provider":"<id>"}` — the provider's
   `base_url`/`api_key_env` are applied unless the same request overrides
   them, the compose file is rewritten and `up -d` runs;
3. remove the old per-instance secret: `DELETE /api/instances/{id}/env/{KEY}`
   (or `PUT /api/instances/{id}/env` without it). Until you do, the provider
   value wins at injection time, so the stale row is inert.

`POST /api/instances` accepts `provider` directly, which is the shortest path
for new agents. `clone` inherits the provider of its source (a reference, not
a secret), so cloning no longer requires a gate token when the source uses a
provider. Archives (`GET .../export`) carry the provider **id** only: an
archive restored on a host without that provider drops the reference with a
warning and falls back to the instance's own token.

## Security

* The secret is stored as one AES-256-GCM blob (`providers.nonce`,
  `providers.ciphertext`), encrypted with the fleet key
  (`/etc/fleett/secrets.key` or `FLEETT_SECRETS_KEY`). Losing the key makes
  the secret unreadable; it never appears in a backup of `FLEETT_ROOT`.
* Every API response, the audit trail and the logs carry metadata only. The
  test report carries status/latency/error, never the key.
* `provider.create|update|delete|rotate|test` are appended to the audit log
  with the actor, role and IP; details hold the name, kind, endpoint and
  variable name, never the secret.
* Deleting a provider still attached to an instance is refused (`409`), so an
  instance can never be left pointing at a missing credential.
* Writing a provider requires a configured secrets key
  (`409 secrets_key_missing` otherwise), exactly like instance secrets.
