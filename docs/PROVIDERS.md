# Providers — reusable API credentials

A **provider** is a named API credential that several agents can share. Before
this feature every instance owned its token: cloning an agent demanded a new
token (`400 clone_token_required`) and rotating one meant patching each agent.
Now one `PATCH` rotates the credential for every attached instance.

Instances created before the feature keep working unchanged: an instance with
no `provider` still stores and injects its own `gate_token`/`api_key_env`, and
nothing is migrated automatically.

## Model

`providers` (SQLite, schema v32):

| Column | Meaning |
|---|---|
| `id` | UUID, used by the API and by `instances.provider` |
| `name` | unique slug (`[a-z0-9-]{2,63}`) |
| `kind` | `openai-compatible`, `anthropic` or `google` — metadata for the console; every kind is probed through `/models` and `/chat/completions` |
| `base_url` | endpoint, e.g. `https://opencode.ai/inference/go/openai/v1` |
| `api_key_env` | container variable the secret is injected under, e.g. `HERMES_GATE_PROFILE_TOKEN` |
| `model` | optional default model of the credential, used by the assistant and the test |
| `header_names` | names of the configured upstream headers (JSON array; metadata only) |
| `nonce`, `ciphertext` | AES-256-GCM blob of the secret (same key as the instance environment) |
| `headers_nonce`, `headers_ciphertext` | AES-256-GCM blob of the upstream header map (issue #64) |
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
| `GET` | `/api/providers` | `[{id,name,kind,base_url,api_key_env,model,extra_headers,instances_count,created_at,updated_at}]` |
| `POST` | `/api/providers` | `{name,kind,base_url,api_key_env,secret,model?,extra_headers?}` → `201` summary |
| `GET` | `/api/providers/{id}` | summary + `instances:[{id,name}]` |
| `PATCH` | `/api/providers/{id}` | `{name?,kind?,base_url?,api_key_env?,secret?,model?,extra_headers?}` → `{provider, restarted:[{id,status,error?}]}` |
| `DELETE` | `/api/providers/{id}` | `204`, or `409 provider_in_use` with `details.instances` |
| `POST` | `/api/providers/{id}/test?model=` | `GET /models` **and** a 1-token `POST /chat/completions` → `{ok,models_ok,completion_ok,latency_ms,models_status?,completion_status?,error?}` |

`extra_headers` is written as an object and returned as an **array of names**:
the values are encrypted and never leave the process. `model` is the
credential's default model (`null` when unset).

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

## Upstream headers

Some gateways need more than a credential: `opencode.ai` routes a completion by
`x-opencode-session`, Azure wants `api-version`, Anthropic `anthropic-version`,
a corporate proxy an `X-Api-Version`. A provider carries those headers:

```json
{
  "name": "opencode-go",
  "kind": "openai-compatible",
  "base_url": "https://opencode.ai/inference/go/openai/v1",
  "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
  "secret": "…",
  "model": "deepseek-v4.1-flash",
  "extra_headers": {"x-opencode-session": "3a11b7b3-24a1-4a09-b5a6-fa27cf93c9aa"}
}
```

Rules:

* names are RFC 7230 tokens, unique case-insensitively, at most **32** headers,
  **8 KiB** per value, no control character (a value can never inject a second
  header);
* values are encrypted with the fleet key, exactly like the secret, and bound to
  the `header:` field: a blob swapped with the secret fails to decrypt;
* **no** endpoint returns a value — summaries and details expose
  `extra_headers: ["x-opencode-session"]`, the names only;
* `PATCH {"extra_headers": {…}}` replaces the whole map, `{"extra_headers": {}}`
  removes every header, and omitting the field keeps the stored map;
* a header may override `authorization` or `content-type` (the map is applied
  after the defaults), which is how a proxy with its own auth scheme is wired;
* instances are **not** restarted: headers only affect the assistant and the
  connectivity test, never the container contract.

### Recipe — opencode.ai

The gateway rejects a completion without a routing header and answers `400
MissingSessionID`, while the same key is valid on `/models` (which is why the
test button used to be green while every real call failed). Two ways to fix it:

1. **let fleett do it** — nothing to configure. When the `base_url` host
   contains `opencode.ai`, every assistant call adds `x-opencode-session`
   derived from the assistant `session_id` (stable for the whole conversation,
   sanitised to `[A-Za-z0-9._:-]`, at most 128 characters) and the provider test
   sends a fresh UUID. An explicit `x-opencode-session` in `extra_headers`
   always wins and is never duplicated.
2. **pin it yourself** — set the header explicitly when the gateway expects a
   specific value (a shared session, a routing key issued by the proxy):

   ```
   PATCH /api/providers/<id>
   {"extra_headers": {"x-opencode-session": "3a11b7b3-24a1-4a09-b5a6-fa27cf93c9aa"}}
   ```

   The value is encrypted at rest and never returned; only
   `extra_headers: ["x-opencode-session"]` shows in `GET /api/providers`.

`model` is also worth setting: the assistant uses it before
`FLEETT_ASSISTANT_MODEL`, so one console can drive an opencode provider with
`deepseek-v4.1-flash` while the environment default stays untouched.

## Test endpoint

`POST /api/providers/{id}/test` makes **two** calls, because a provider can pass
the first and fail the second:

1. `GET ${base_url}/models` with `Authorization: Bearer <secret>`,
   `x-api-key: <secret>` and `anthropic-version: 2023-06-01` (harmless for
   OpenAI-style endpoints, required by Anthropic);
2. `POST ${base_url}/chat/completions` with the same headers, the configured
   `extra_headers`, the session header when the host is `opencode.ai`, and a
   real 1-token completion: `{"model": …, "messages":[{"role":"user",
   "content":"ping"}], "max_tokens": 1, "temperature": 0}`.

Each call has a 5 s timeout. The answer reports them separately:

```json
{
  "ok": false,
  "models_ok": true,
  "completion_ok": false,
  "latency_ms": 193,
  "models_status": 200,
  "completion_status": 400,
  "error": "POST /chat/completions failed: HTTP 400: {\"error\":{\"type\":\"MissingSessionID\"}}"
}
```

`ok` is `models_ok && completion_ok`, so the console can turn red on the second
line alone. `error` is the first failure, truncated (512 bytes) and redacted
(the secret, every header value, `sk-…`, `flt_…` and `Bearer …`). A provider
without `extra_headers` behaves exactly as before, with an extra completion
check. `?model=` overrides the provider default for this test only (a `null`
provider model falls back to `FLEETT_ASSISTANT_MODEL`).

The secret and the header values never appear in the answer; the request path is
the only other detail.

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
* The upstream header map is a second blob (`providers.headers_nonce`,
  `providers.headers_ciphertext`) encrypted with the same key and bound to the
  `header:` field prefix.
* Every API response, the audit trail and the logs carry metadata only (header
  **names**, never a value). The test report carries statuses, latency and a
  redacted error, never the key.
* `provider.create|update|delete|rotate|test` are appended to the audit log
  with the actor, role and IP; details hold the name, kind, endpoint and
  variable name, never the secret.
* Deleting a provider still attached to an instance is refused (`409`), so an
  instance can never be left pointing at a missing credential.
* Writing a provider requires a configured secrets key
  (`409 secrets_key_missing` otherwise), exactly like instance secrets.
