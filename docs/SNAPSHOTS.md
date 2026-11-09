# Restore points (snapshots)

`fleett upgrade` can roll back an *image*. It never protected the *data* of an
agent: a broken `SOUL.md`, a badly applied environment or a failed migration had
no safety net, and `fleett backup` archives the whole fleet — too heavy for one
risky change.

A snapshot is a per-instance, restorable archive of the Hermes home plus the
configuration around it:

```bash
fleett snapshots alice                     # list the restore points
fleett snapshot create alice --label "before model change"
fleett snapshot restore <snapshot-id>      # asks before touching anything
fleett snapshot diff <a> <b>               # file-level differences
fleett snapshot rm <snapshot-id>
```

Creating, restoring and deleting need `admin`; listing and comparing need any
authenticated role. The API contract is in [API.md](../API.md), the client
commands in [CLI.md](CLI.md).

## What is inside

Files live in `FLEETT_ROOT/snapshots/<snapshot-id>/<timestamp>.tgz`, so a
snapshot is a plain gzipped tar an operator can inspect with `tar -tzf`:

```
snapshot.json   format/version, snapshot and instance ids, image, label,
                created_at, expires_at, fleett version
compose.yaml    the compose file as it was
config.yaml     the rendered Hermes config (also present under data/)
soul.md         the agent persona, when it had one (also under data/)
env.json        the instance_env rows, secrets still encrypted
data/           the Hermes home
```

`data/` uses the same walker as `fleett backup`: `secrets.env` and `*.key` never
leave the host, sockets, FIFOs and device nodes are skipped, and symlinks whose
target escapes the tree are omitted. Snapshots additionally skip runtime caches
(`__pycache__`, `.cache`, `cache`, `node_modules`) because they are regenerated
on the next start. The whole archive is capped by the shared decompression
guard (8 GiB uncompressed).

## Secrets never leave in clear

`env.json` is *not* a decrypted dump. A secret row carries the AES-256-GCM
nonce and ciphertext exactly as they sit in `instance_env`, base64-encoded:

```json
{
  "provider": null,
  "api_key_env": "HERMES_GATE_PROFILE_TOKEN",
  "rows": [
    {"key": "DISCORD_ALLOWED_USERS", "secret": false, "value": "123",
     "updated_at": "2026-11-09T10:00:00Z"},
    {"key": "TELEGRAM_BOT_TOKEN", "secret": true,
     "nonce": "q83v…", "ciphertext": "9Zc…",
     "updated_at": "2026-11-09T10:00:00Z"}
  ]
}
```

Consequences:

- a snapshot never contains a token: without `/etc/fleett/secrets.key` (or
  `FLEETT_SECRETS_KEY`) the encrypted rows are opaque;
- restoring on the same host keeps the tokens (the key is unchanged);
- non-secret variables stay readable, exactly like the database — they are
  configuration, not credentials;
- putting the secrets key in the same backup job as the snapshots is still a
  separate decision: the key files themselves are never archived.

## Restore semantics

`POST /api/snapshots/{id}/restore` runs synchronously and holds the instance
lock for the whole operation:

1. the archive manifest is read and checked against the snapshot row and the
   instance (a snapshot always belongs to exactly one instance);
2. the instance is stopped;
3. the current `data/` is renamed to
   `instances/<id>/data.pre-restore-<YYYYMMDD-HHMMSS>` — **it is never
   deleted**;
4. `data/` is extracted from the archive, then `config.yaml` and `SOUL.md` are
   written from their root copies (authoritative), and ownership is handed back
   to UID/GID 10000;
5. the compose file is restored and the image is re-read from it;
6. the `instance_env` rows are replaced by the encrypted rows of `env.json`;
7. `docker compose up -d` runs and the container must become `healthy` within
   180 seconds.

Any failure in steps 4–7 triggers a rollback: the extracted directory is
removed, the `data.pre-restore-*` directory is renamed back, the previous
compose file and env rows are restored, the instance is started again and its
status becomes `error`. The response is `409 restore_failed` with the reason
(`error.details.rolled_back = true`); the snapshot row records `failed` and its
error text. A successful restore leaves the snapshot row `ok` and returns the
number of files and the observed status.

The `data.pre-restore-*` directory of a successful restore is kept on purpose:
inspect it, then delete it when the restore is confirmed. It doubles the disk
usage of the instance until then, and the next `fleett backup` archives it as
part of `instances/` — delete it before a backup if the duplication matters.
`DELETE /api/instances/{id}?purge=true` erases those siblings with the data
directory.

## Automatic snapshots

A snapshot is taken automatically before the risky operations that exist in
this build, best effort: a snapshot that cannot be written logs a warning and
never blocks the operation (a full disk must not stop an upgrade).

| Operation | Label | Where |
|---|---|---|
| Rolling image upgrade (`POST /api/upgrade`) | `auto: upgrade` | before `services.agent.image` moves |
| `PUT /api/instances/{id}/env` | `auto: env` | before the rows are replaced |
| `PATCH /api/instances/{id}` with `env`/tokens/channel | `auto: patch` | before the env rewrite |
| `DELETE /api/instances/{id}/env/{key}` | `auto: env-delete` | before the row is removed |
| `POST /api/instances/{id}/git/revert` | `auto: git-revert` | before files are restored |

The issue also mentioned applying a `fleet.yaml`. This build has no such
operation (the fleet is created through the per-instance API), so nothing is
wired there; if a declarative apply endpoint is added later it should call
`snapshots::auto_before(state, id, "fleet-apply")` before its first write.

Automatic snapshots use the default 7-day retention. A failed upgrade keeps its
`auto: upgrade` snapshot, which is the intended restore point for the data; the
image itself is rolled back by the upgrade job. The data is **not** restored
automatically on a failed upgrade, on purpose:

- memory and sessions written between the snapshot and the failure are usually
  worth keeping;
- the upgrade already restores the previous image, so the agent comes back on
  the image it ran before;
- an automatic data restore would overwrite the `last_error` of the upgrade,
  which is the operator's explanation of what happened.

To return an instance to the exact pre-upgrade data, restore the
`auto: upgrade` snapshot explicitly (`fleett snapshots <id>`, then
`fleett snapshot restore <snapshot-id>`).

Deleting an instance does not delete its snapshots: the restore point may be
exactly what is needed after a mistaken delete, and the TTL eventually removes
it. Once the instance is gone, `GET /api/snapshots/{sid}` still returns the
row, while `POST /api/snapshots/{sid}/restore` answers `404` (no instance to
restore into); `DELETE /api/snapshots/{sid}` removes it.

## Retention and purge

`ttl_s` defaults to 7 days and is capped at 90 days (`400 invalid_snapshot`
outside `1..=90 days`). Labels are optional, trimmed, at most 128 characters and
free of control characters.

Expired snapshots are deleted (files and row) by:

- a housekeeping call at every control-plane start;
- `DELETE /api/snapshots/expired` (admin), which answers
  `{"removed": N, "failed": M}`. A row whose file cannot be deleted is kept and
  counted in `failed` instead of disappearing from the index.

`DELETE /api/snapshots/{id}` removes one snapshot and its archive. Restoring
does not delete the snapshot: it can be restored again.

## Comparison

`POST /api/instances/{id}/snapshots/compare` takes two snapshot ids of the same
instance and reads both tars directly — nothing is extracted to disk. It
returns the archive paths of:

- `added`: present in `b`, absent from `a`;
- `removed`: present in `a`, absent from `b`;
- `changed`: present in both with a different SHA-256 (sizes included);
- `unchanged`: number of identical files.

`snapshot.json` is skipped; symlinks are compared through a hash of their target
with size 0. Paths keep their archive names (`data/SOUL.md`, `compose.yaml`,
`env.json`, …).

## Relationship to the other archives

| Command | Scope | Restores |
|---|---|---|
| `fleett backup` | whole host (index, instances, shares) | `fleett backups restore` |
| `fleett export` / `import` | one instance, portable | `POST /api/instances/import` |
| snapshot | one instance, in place | `POST /api/snapshots/{id}/restore` |

A snapshot is not a backup: it lives under the same data root (a lost disk loses
both) and only covers one instance. Keep `fleett backup` (and the host backup)
for disaster recovery, and snapshots for the minute before a risky change.
