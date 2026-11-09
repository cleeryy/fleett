# Instance environment — editable after creation

Before v0.5.0 an instance received its whole environment at creation, and every
value went into the encrypted secret store. An allowlist such as
`DISCORD_ALLOWED_USERS` could not be added to a running agent without
recreating it.

Every variable is now an editable, per-key row with an explicit **secret flag**:

| | `secret: false` | `secret: true` |
|---|---|---|
| Database | plaintext `value` | AES-256-GCM `nonce` + `ciphertext`, no `value` |
| `GET /api/instances/{id}` | `value` returned | `value` never returned |
| Compose file | `${KEY}` | `${KEY}` |
| `docker compose` child env | the plaintext value | the decrypted value |

The AES key is the one managed by `fleett secrets init` (see
[SECRETS.md](SECRETS.md); `FLEETT_SECRETS_KEY` or `FLEETT_SECRETS_KEY_FILE`).
A secret value is encrypted
together with its variable name, so a ciphertext moved to another row fails to
decrypt instead of silently changing another variable. A fresh nonce is used
for every write.

The compose file never contains a value: only
`KEY: ${KEY}` declarations. The compose driver injects both kinds of values
into the `docker compose` child process, so `docker inspect <container>` shows
the environment of the running container.

## Storage

Schema v7 adds:

```sql
CREATE TABLE instance_env (
    instance_id  TEXT NOT NULL,
    key          TEXT NOT NULL,
    secret       INTEGER NOT NULL DEFAULT 0,
    value        TEXT,               -- secret = 0
    nonce        BLOB,               -- secret = 1
    ciphertext   BLOB,               -- secret = 1
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (instance_id, key)
);
```

The table is part of `fleet.db`, which lives inside `FLEETT_ROOT`. Keep the
secrets key outside the root: a backup of the root then contains ciphertext
only.

### Migrating the old stores

Two stores are read as a fallback and superseded by the first API write:

- `instance_secrets` (one encrypted JSON map per instance, releases up to
  v0.4.0). At startup every map is decrypted and split into `instance_env` rows
  with `secret = 1`, and the superseded row is deleted. The table itself is kept
  for one release so a downgrade can still read it; it is no longer written by
  this build. Without a configured key the maps stay in place (readable through
  the fallback) and a warning is logged.
- `instances/<id>/secrets.env` (plaintext, pre-encryption releases). The driver
  still injects its values, `GET` lists the keys as secrets, and
  `fleett secrets migrate --apply` imports them. See [SECRETS.md](SECRETS.md).

The first API write (`POST`, `PATCH`, `PUT .../env`, `DELETE .../env/{key}`)
writes the complete set into `instance_env`, deletes the `instance_secrets`
row and removes the plaintext `secrets.env`. A stale token can then never
reappear.

**Behaviour change:** the API no longer writes plaintext token files. Rotating a
token on a host that still uses a `secrets.env` therefore requires the secrets
key (`sudo fleett secrets init`, then retry). Before v0.5.0 the API rewrote the
plaintext file.

A legacy key that does not match the new grammar (a lowercase name accepted by
older releases, for example) is imported as it is. The first write that would
rewrite it is refused with `400 invalid_env` naming the key; delete it once with
`DELETE /api/instances/{id}/env/{key}`.

## API

### Detail

```json
{
  "id": "tchouniz",
  "env": [
    {"key": "DISCORD_ALLOWED_USERS", "secret": false, "updated_at": "2026-11-09T10:00:00Z", "value": "123456789"},
    {"key": "DISCORD_BOT_TOKEN", "secret": true, "updated_at": "2026-11-09T10:00:00Z"}
  ]
}
```

`env` replaces the `env_keys` array of older releases (**breaking**, v0.5.0).
A secret entry never carries `value`; the key names are returned so a console
can show the masked list and round-trip it. Entries are sorted by key.

### Full replace

```bash
curl -X PUT https://fleett.example/api/instances/tchouniz/env \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env": [
        {"key": "DISCORD_ALLOWED_USERS", "value": "123456789,987654321", "secret": false},
        {"key": "DISCORD_ALLOWED_ROLES", "value": "424242", "secret": false},
        {"key": "DISCORD_BOT_TOKEN", "secret": true}
      ]}'
```

- Every key the request leaves out is **deleted**.
- `secret` is required on every entry.
- A secret entry without a value keeps the stored value. This is what makes a
  round-trip possible: the API never returned that value, so a console can
  send the key back as `{"key": "...", "secret": true}`.
- A new secret, or a non-secret entry, without a value is a `400 invalid_env`.

The response is the updated instance detail.

### Delete one key

```bash
curl -X DELETE https://fleett.example/api/instances/tchouniz/env/DISCORD_ALLOWED_ROLES \
  -H "Authorization: Bearer $FLEETT_TOKEN"
```

`204 No Content`, or `404 not_found` when the key is not set.

### Merge on PATCH

```bash
curl -X PATCH https://fleett.example/api/instances/tchouniz \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env": [
        {"key": "DISCORD_ALLOWED_USERS", "value": "123456789"},
        {"key": "DISCORD_ALLOWED_CHANNELS", "value": "555"},
        {"key": "OBSOLETE", "value": null}
      ]}'
```

- merge by key: only the listed keys move;
- `"value": null` deletes the key;
- `"value": ""` keeps an empty value;
- a missing `secret` keeps the flag of an existing key (`false` for a new one).

### Create

`POST /api/instances` accepts the same list:

```json
{
  "id": "helper",
  "template": "guest-discord",
  "model": "deepseek-v4.1-flash",
  "base_url": "https://opencode.ai/inference/go/openai/v1",
  "channel": {"type": "discord", "token": "..."},
  "env": [
    {"key": "DISCORD_ALLOWED_USERS", "value": "123456789", "secret": false}
  ]
}
```

The gate token (`gate_token`) and the platform token (`channel.token`) are
always secret, whether or not they appear in `env`, and win over an entry with
the same key.

### Deprecated v0.4.0 object form

`env` used to be an object of strings on create (`"env": {"KEY": "value"}`).
That form is **deprecated but still accepted** on `POST /api/instances`,
`PATCH /api/instances/{id}` and `PUT /api/instances/{id}/env`; it logs
`deprecated=env_map` at `warn` level. Every entry of it is stored with
`secret = 1`, exactly as v0.4.0 did, so the values are encrypted and never
returned by `GET /api/instances/{id}`. The response `env` is always the list
form.

Migrating is a one-time request:

```bash
# 1. Make the allowlists explicitly non-secret (they were secret in v0.4.0).
curl -X PATCH https://fleett.example/api/instances/tchouniz \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env":[{"key":"DISCORD_ALLOWED_USERS","value":"123456789","secret":false}]}'

# 2. A PUT replaces the whole set; resend each secret without its value to keep it.
curl -X PUT https://fleett.example/api/instances/tchouniz/env \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"env":[
        {"key":"DISCORD_ALLOWED_USERS","value":"123456789","secret":false},
        {"key":"DISCORD_BOT_TOKEN","secret":true}
      ]}'
```

A malformed `env` returns `400 invalid_env` with the expected schema in
`error.details` (`expected`, `received`, `example`), both on the canonical list
and on the object form.

## Validation

| Rule | Error code |
|---|---|
| key matches `^[A-Z_][A-Z0-9_]{0,63}$` | `invalid_env` |
| key is not `HERMES_UID`, `HERMES_GID`, `PATH`, `HOME` or `FLEETT_*` | `reserved_env` |
| value is at most 8 KiB | `invalid_env` |
| value contains no newline, carriage return or NUL | `invalid_env` |
| no duplicate key in one request | `duplicate_env` |
| secret flag present where required | `invalid_env` |
| value matches the `format` of the template suggestion | `invalid_env_format` |

The format of a suggested variable is enforced on `POST /api/instances`, on
`POST .../clone` for the values the clone inherits, on the `env` of a `PATCH`
and on `PUT .../env`. Unknown keys are never rejected; a suggestion without a
format (or with a format this release does not know) is not checked.

| `format` | Accepted values |
|---|---|
| `csv-of-ids` | comma-separated ASCII digits, spaces around entries allowed (`123, 456`) |
| `bool` | `true`, `false`, `1`, `0`, `yes`, `no` (case-insensitive) |
| `url` | absolute `http://` or `https://` URL with a host |
| `text` | any non-empty value |

A refused value answers `400` with the key, the expected format and the
received value:

```json
{"error":{"code":"invalid_env_format",
          "message":"environment variable \"DISCORD_ALLOWED_USERS\" must match format \"csv-of-ids\": \"alice\" is not a numeric id: use comma-separated ASCII digits",
          "details":{"key":"DISCORD_ALLOWED_USERS","expected":"csv-of-ids","received":"alice"}}}
```

The reserved names belong to the container contract: `HERMES_UID` and
`HERMES_GID` are literals in the compose file, and the driver environment would
otherwise shadow `PATH`/`HOME` of the `docker compose` process.

## Apply and audit

Every write rewrites `instances/<id>/compose.yaml` and runs `docker compose up
-d`, so **the container is recreated** (brief downtime). `docker compose ps` is
then read back:

- success: the status and health of the detail reflect the new container;
- failure: `last_error` carries the compose stderr and the status is `error`,
  while the stored environment keeps the new values (retry the apply with
  another write, or `POST .../restart`).

Audit entries:

| Action | When |
|---|---|
| `instance.env.update` | `PUT .../env` (and `PATCH .../env` goes through `instance.patch`) |
| `instance.env.delete` | `DELETE .../env/{key}` |

`instance.env.update` details carry the key list and the number of secrets,
never a value.

## Templates

`GET /api/templates` returns the variables a template expects, with an
`example`, a `required` flag and the `format` the API enforces:

| Template | Suggestions |
|---|---|
| `guest` (Telegram) | `TELEGRAM_ALLOWED_USERS` (required, `csv-of-ids`), `GATEWAY_ALLOW_ALL_USERS` (`bool`) |
| `guest-discord` | `DISCORD_ALLOWED_USERS` (required, `csv-of-ids`), `DISCORD_ALLOWED_ROLES` (`csv-of-ids`), `DISCORD_ALLOWED_CHANNELS` (`csv-of-ids`), `DISCORD_ALLOW_ALL_USERS` (`bool`) |
| `solo` | none |

```json
{
  "id": "guest-discord",
  "env": [
    {
      "key": "DISCORD_ALLOWED_USERS",
      "description": "Comma-separated Discord user ids allowed to talk to the agent…",
      "secret": false,
      "example": "274575374111866880",
      "required": true,
      "format": "csv-of-ids"
    }
  ]
}
```

A custom template declares its own list in `templates/<id>/template.yaml`.
The three new fields are optional, so a `template.yaml` written for an older
release still parses (`required` defaults to `false`, `format` and `example` to
`null`):

```yaml
env:
  - key: DISCORD_ALLOWED_USERS
    description: Comma-separated Discord user ids allowed to talk to the agent.
    example: "123456789,987654321"
    required: true
    format: csv-of-ids
    secret: false
```

`required: true` makes `POST /api/instances` fail with
`400 missing_required_env` (`details: [{"key":"…","field":"env"}]`) until the
variable is supplied; `secret` defaults to `false` and a request that omits the
flag follows the schema. The parent template's entries are merged by key. See
[TEMPLATES.md](TEMPLATES.md) for the full schema and inheritance rules.

All built-in suggestions are non-secret: they are allowlists, not credentials.

## Check endpoint and warnings

`GET /api/instances/{id}/check` reports whether the template allowlists of an
instance are satisfied (the container is queried for its health, not started):

```json
{
  "ok": false,
  "warnings": [
    {
      "code": "allowlist_missing",
      "key": "DISCORD_ALLOWED_USERS",
      "message": "DISCORD_ALLOWED_USERS is required by the guest-discord template but is not set: the gateway will refuse every message until it is",
      "field": "env.DISCORD_ALLOWED_USERS"
    },
    {
      "code": "gateway_may_refuse",
      "key": "DISCORD_ALLOWED_USERS",
      "message": "the container is healthy but DISCORD_ALLOWED_USERS is empty: the gateway is probably refusing every message",
      "field": "env.DISCORD_ALLOWED_USERS"
    }
  ],
  "template": "guest-discord"
}
```

- `allowlist_missing`: a `required` key of the template is absent or empty;
- `gateway_may_refuse`: the container is `healthy` while such a key is missing,
  i.e. the gateway is up and rejecting every message (the exact failure from
  issue #27);
- `ok` is `true` only when there is no warning;
- `field` is the console form field to focus (`env.<KEY>`);
- `template` is the id of the template that was checked (the same value as
  `InstanceDetail.template`; an adopted `custom` instance has no suggestion).

The same messages are appended to `warnings` in `InstanceDetail`, after the
egress/LAN and plaintext-secrets warnings, so the console can display them
without calling `/check`. A stopped instance only gets `allowlist_missing`:
`gateway_may_refuse` requires a healthy container.

Adding the missing key through `PATCH`/`PUT .../env` rewrites the compose file
and runs `docker compose up -d`, so the agent is unblocked after the write
without a manual restart.

## Clone and export

- `POST /api/instances/{id}/clone` copies the **non-secret** variables of the
  source and never a secret one; the gate and platform tokens must still be
  supplied by the request, as before.
- `GET /api/instances/{id}/export` excludes every environment value, secret or
  not. An import therefore starts with an empty environment and the usual
  warning telling the operator which tokens to set.
