# Audit trail — retention, signed export and consultation

Every security-relevant action is appended to `audit_log` in `fleet.db`. The
trail answers three questions: **who** acted, **what** they did, and **whether
it succeeded**. It is append-only from the API: a mutation that happened is
recorded even when the request ends in an error.

## Fields

| Column | Meaning |
|---|---|
| `id` | Row id, monotonically increasing; the list is ordered by it (newest first) |
| `at` | RFC 3339 UTC instant (`2026-11-09T10:42:00Z`) |
| `actor` | Email (or OIDC `sub`) for sessions, token name for bearers, `bootstrap`, `anonymous` or `system` |
| `role` | `admin` or `viewer`; `none` for a failed unauthenticated login, `system` for housekeeping |
| `method` | Credential used: `session`, `bearer` or `system` |
| `action` | Dot-separated verb: `instance.create`, `provider.rotate`, `auth.login`, `audit.purge`… |
| `target` | Object of the action: instance id, share name, token id, or the request path for a refused mutation |
| `ip` | First `X-Forwarded-For` hop, else `X-Real-IP`, else the peer address |
| `user_agent` | Client `User-Agent`, truncated to 512 characters |
| `result` | `ok` (default), `failed` or `refused` |
| `request_id` | Optional correlation id (reserved; currently `null` unless a caller sets it) |
| `details` | JSON object with the action-specific context; never a secret value |

Schema v11 adds `user_agent`, `result` and `request_id`, plus indexes on
`actor` and `action`. Existing rows keep `NULL` for the first and last and `ok`
for `result`, so an upgrade never rewrites history.

### What is recorded

- Every mutation of an instance, share, token, provider, file, git tree, bus
  message, memory entry and upgrade.
- `auth.login` on a successful OIDC callback (`actor` = email, `method` =
  `session`, `details.provider` = issuer) and on every rejected callback
  (`actor` = `anonymous`, `result` = `failed`, `details.reason` = the stable
  reason `cookie_missing`, `state_mismatch`, `expired`, `oidc_denied`…).
- `auth.logout`, with the identity of the session being cleared when the cookie
  is still valid.
- `auth.mutation_refused` when a viewer attempts a mutation or a cookie
  mutation arrives without the CSRF header. `result` = `refused` and
  `details.reason` = `viewer_read_only` or `csrf_required`.
- `audit.purge` after a retention run (see below).

Reads are not recorded. Secrets never are: the audit log stores identifiers and
metadata, the same rule as every API response.

## Consultation — `GET /api/audit`

Admin only. Returns a JSON array, newest first.

| Parameter | Default | Meaning |
|---|---|---|
| `limit` | `200` | Page size, `1..=1000` |
| `offset` | `0` | Rows to skip (pagination) |
| `actor` | — | Exact actor match |
| `action` | — | Exact action match (`instance.create`) |
| `target` | — | Exact target match |
| `from` | — | Inclusive lower bound |
| `to` | — | Inclusive upper bound |
| `result` | — | `ok`, `failed` or `refused` |

`from`/`to` accept an RFC 3339 instant (`2026-11-01T00:00:00Z`,
`2026-11-01T00:00:00+01:00`) or a bare `YYYY-MM-DD`. A date-only `from`
starts at midnight UTC; a date-only `to` covers the whole day
(`23:59:59Z`). Offsets are converted to UTC, the format every row uses. An
unknown `result` or a reversed range is a `400`.

```bash
curl -s -H "Authorization: Bearer $FLEETT_TOKEN" \
  'https://fleett.example/api/audit?actor=cleeryy@example.test&action=instance.create&from=2026-11-01&to=2026-11-30&limit=50'
```

The legacy `?limit=` call keeps its exact behaviour: newest first, 200 by
default, 1000 maximum.

## Export — `GET /api/audit/export`

Admin only. `from` and `to` are **required**: an export is evidence for a
stated period, not a dump. The response is:

```http
HTTP/1.1 200 OK
Content-Type: application/x-ndjson
Content-Disposition: attachment; filename="fleett-audit-20261109T104200Z.jsonl"
X-Fleett-Signature: sha256=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
Cache-Control: no-store
```

One JSON object per line, each with every field of the list endpoint (including
`result`), newest first, and a trailing newline. The signature is the
hex HMAC-SHA256 of the **exact response body** (the bytes as sent, including
the final newline), keyed by `FLEETT_AUDIT_HMAC_KEY`.

### Key

```dotenv
# 32 random bytes, hex-encoded (recommended: openssl rand -hex 32)
FLEETT_AUDIT_HMAC_KEY=<64 hex characters>
```

The value is accepted as hexadecimal or raw text:

- `hex:<digits>` decodes the digits; an invalid value refuses to start;
- `raw:<text>` uses the UTF-8 bytes as-is;
- without a prefix, an **even-length** string made only of hex digits is
  decoded (the `openssl rand -hex 32` case), anything else is used raw.

Because a raw key that happens to look like hex is decoded, prefix an
ambiguous raw key with `raw:`. Keep the key outside `FLEETT_ROOT` (for
example in `/etc/fleett.env`, mode 0600): anyone holding it can forge an
export.

Without a key the endpoint answers `409 audit_key_missing` and names
`FLEETT_AUDIT_HMAC_KEY`. It never exports unverifiable bytes silently.

### Verifying

`fleett audit export` verifies the download when `--key` is given and always
saves the signature next to the file:

```bash
fleett audit export --from 2026-11-01 --to 2026-11-30 -f audit.jsonl --key "$FLEETT_AUDIT_HMAC_KEY"
# signature verified with the supplied key
# audit.jsonl.sig contains "sha256=<hex>"
```

Without `--key`, the export is kept with its `.sig` file and the tool says so;
re-run with the key to verify later. A mismatch exits `1` and writes nothing.

Shell (raw key; for a hex key decode it first, for example with `xxd -r -p`):

```bash
curl -sS -D headers.txt -o audit.jsonl \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  'https://fleett.example/api/audit/export?from=2026-11-01&to=2026-11-30'
grep -i '^x-fleett-signature:' headers.txt
openssl dgst -sha256 -hmac "$FLEETT_AUDIT_HMAC_KEY" audit.jsonl
```

Python (works for hex and raw keys):

```python
import hashlib, hmac

key_text = "…"                        # FLEETT_AUDIT_HMAC_KEY
key = bytes.fromhex(key_text) if len(key_text) % 2 == 0 and all(
    c in "0123456789abcdefABCDEF" for c in key_text
) else key_text.encode()
body = open("audit.jsonl", "rb").read()
print("sha256=" + hmac.new(key, body, hashlib.sha256).hexdigest())
```

Compare with the `X-Fleett-Signature` header (or `audit.jsonl.sig`).

## Retention

```dotenv
FLEETT_AUDIT_RETENTION=90d     # default
# 30d                          days
# 24h                          hours
# 30                           bare integer = days
```

Zero or a negative value refuses to start. The control plane applies the policy
once a day in the background (first run one day after start, so a restart does
not look like housekeeping) and on demand:

```bash
curl -X DELETE -H "Authorization: Bearer $FLEETT_TOKEN" \
  https://fleett.example/api/audit/expired
# {"removed":12,"cutoff":"2026-08-11T10:42:00Z","retention":"90d"}
```

Every row strictly older than `now - retention` is deleted and one
`audit.purge` entry is appended with `details.removed`, `details.cutoff`,
`details.retention` and `details.trigger` (`api` or `schedule`). The purge row
is newer than the cutoff, so a second run never deletes it; the scheduled run
only journals when it removed something, the endpoint journals every explicit
call. `fleett audit purge` is the CLI equivalent.

## CLI

```bash
fleett audit --actor cleeryy@example.test --from 2026-11-01 --limit 50
fleett audit --result refused --json
fleett audit export --from 2026-11-01 --to 2026-11-30 -f audit.jsonl --key "$FLEETT_AUDIT_HMAC_KEY"
fleett audit purge
```

See [CLI.md](CLI.md) and the route table in [API.md](../API.md).
