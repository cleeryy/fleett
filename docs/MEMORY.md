# Shared fleet memory

Every instance learns something the others do not know. The fleet memory (issue #29) is a
small knowledge base in the controller database: key/value entries, full text search,
tags, a TTL and a complete history. It answers "what does the fleet already know about X?"
without mounting another share or asking a human.

The memory is **plaintext by design** (it must be searchable) and **must never hold
credentials**. Secrets belong to `instance_env` and `providers`, where they are encrypted.

## Namespaces

| Namespace | Written by | Use |
|---|---|---|
| `global` | admins / promoted entries | fleet-wide lessons, contracts, runbooks |
| `shared` | any agent | the default read/write space of the fleet |
| `instance:<id>` | one agent | private notes of an instance |
| `project:<name>` | project agents | knowledge attached to a project |

Namespaces are labels, not security boundaries yet: a token that can write can write to
`shared` and to any `instance:` namespace. `global` writes are admin-only. Per-token write
scoping follows the RBAC issue (#37).

## API

```bash
# write
curl -sS -X POST "$FLEETT_URL/api/memory" -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"namespace":"instance:probe","key":"runbook",
       "value":"restart the gateway after an upgrade","tags":["ops"]}'

# search from anywhere in the fleet
curl -sS "$FLEETT_URL/api/memory/search?q=gateway%20upgrade" \
  -H "Authorization: Bearer $FLEETT_TOKEN"

# history and rollback
curl -sS "$FLEETT_URL/api/memory/12/history" -H "Authorization: Bearer $FLEETT_TOKEN"
curl -sS -X POST "$FLEETT_URL/api/memory/12/rollback" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"version":1}'

# promote an instance lesson to global
curl -sS -X POST "$FLEETT_URL/api/memory/12/promote" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"namespace":"global"}'
```

`GET /api/memory?namespace=…` browses one namespace; `GET /api/memory/by-key?namespace=…&key=…`
reads one entry.

## CLI

```bash
fleett memory set instance:probe runbook "restart the gateway" --tag ops
fleett memory search "runbook"
fleett memory list --namespace shared
fleett memory history 12
fleett memory restore 12 1
fleett memory promote 12
```

Set `FLEETT_URL` and `FLEETT_TOKEN`, or use `~/.config/fleett/config.toml`.

## Semantics

- **Upsert keyed by `(namespace, key)`.** A second `POST` with the same pair updates the
  entry and increments `version`.
- **History is append-only.** Every create, update, delete and rollback appends a
  `memory_versions` row. A rollback restores an old value as a *new* version, so nothing is
  ever rewritten.
- **Delete is soft.** The entry disappears from reads and search, its history stays, and a
  rollback (or a new `POST`) brings it back.
- **TTL.** `ttl_s` sets `expires_at`; expired entries are excluded from every read. A
  housekeeping pass (`DELETE /api/memory/expired`) removes them from SQLite and FTS.
- **Search.** FTS5 over namespace, key, value and tags; results are ranked with `bm25` and
  every user word is quoted, so query operators are inert.
- **Promotion copies**, it does not move: the source entry stays readable in its namespace.

## Limits

- Key ≤ 128 chars, value ≤ 64 KiB, ≤ 16 tags of ≤ 32 chars.
- TTL 60 s … 1 year.
- 500 results per page.
- No binary values and no attachments: use a share for files.

## Backup

The memory lives in `fleet.db`, which `fleett backup` already archives. Restoring a fleet
backup also restores the memory; no separate file to copy.
