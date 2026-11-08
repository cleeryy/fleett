# Backups and archives

`instances/` and `shares/` are the source of truth, but nothing protected them: `restart:
unless-stopped` survives a crash, not a lost disk. `fleett` ships two archives, both plain
gzipped tars you can inspect with `tar -tzf`:

| Command | Scope | Layout | Restored with |
|---|---|---|---|
| `fleett backup` | the whole fleet (index + instances + shares) | `manifest.json`, `fleet.db`, `instances/`, `shares/` | `fleett backups restore` |
| `GET /api/instances/{id}/export` | one instance | `manifest.json`, `compose.yaml`, `data/` | `POST /api/instances/import` |

A backup restores a **host**; an instance archive moves **one agent** to another host. They
are deliberately different formats: `fleett backups restore` never feeds
`POST /api/instances/import`, and an instance archive is not a host backup.

## Rotating backups

```bash
fleett backup                          # ~/.fleett/backups, keeps 7 archives
fleett backup --dest /srv/backups --keep 14
fleett backups list
fleett backups list --dest /srv/backups
fleett backups restore /srv/backups/fleett-backup-20261108-020000.tgz
fleett backups restore <file> --root /srv/fleett-restored --force
```

No bearer token is needed: the commands read `FLEETT_ROOT` (or `--root`) and the same
`fleett.env` as the service. `fleett backup` can run while the control plane is up.

The archive is named `fleett-backup-YYYYMMDD-HHMMSS.tgz` (UTC, `_2`, `_3`, … when two runs
land in the same second). Rotation deletes the oldest archives beyond `--keep` and never the
one just written; `--keep` is clamped to at least 1.

### What is inside

```
manifest.json   format/version, fleett version, created_at, SQLite schema version,
                instance list (id, name, template, model, status), image tags, shares
fleet.db        consistent copy of the index (SQLite `VACUUM INTO`, taken on a live database)
instances/      compose.yaml + the full Hermes home of every agent
shares/         share contents
```

`fleet.db` is a real snapshot: `VACUUM INTO` reads a transaction-consistent view, so a
backup taken during a write never captures a torn database. Restoring replaces `fleet.db`,
`fleet.db-wal` and `fleet.db-shm` together, so a stale WAL cannot corrupt the restored index.

### What is excluded

- `secrets.env` (legacy plaintext token file) and every `*.key` file, at any depth;
- sockets, FIFOs and device nodes (`data/gateway.sock`, `*.sock`): runtime artifacts, not data;
- symlinks pointing outside the archived tree (absolute targets, `..`).

**The secrets key is out of scope.** `FLEETT_SECRETS_KEY_FILE` defaults to
`/etc/fleett/secrets.key`, outside the data root precisely so a backup never contains it.
Consequences:

- an archive never leaks a token: the encrypted blobs in `fleet.db` are useless without the
  key, and `fleett backup` never stores the key;
- restoring on another host requires the key file (or `FLEETT_SECRETS_KEY`) **and** an intact
  `instance_secrets` table, otherwise the agents restart without their tokens — re-PATCH
  them;
- put `/etc/fleett/secrets.key` in a different backup job (or a secret manager). Losing it
  does not lose the agents, only the stored tokens.

## Restore drill

The control plane holds `fleet.db` open; stop it before restoring. Nothing is started by the
restore command.

```bash
# 1. stop the service and keep a copy of the current root
systemctl stop fleett
cp -a ~/.fleett ~/.fleett.before-restore

# 2. restore into a fresh directory first (recommended)
fleett backups restore /srv/backups/fleett-backup-20261108-020000.tgz \
  --root /srv/fleett-restored

# 3. inspect: the index must list the agents, the homes must be there
sqlite3 /srv/fleett-restored/fleet.db 'select id, name, status from instances;'
ls /srv/fleett-restored/instances/*/data/config.yaml

# 4. swap it in (same filesystem, so a rename is atomic), then restart
systemctl stop fleett
mv ~/.fleett ~/.fleett.old && mv /srv/fleett-restored ~/.fleett
systemctl start fleett
```

Rehearse the drill on a copy, not on the live root. A restore refuses to write into a
non-empty directory unless `--force` is given, and `--force` merges: existing unrelated
files are kept, archived files overwrite their namesakes. Ownership is repaired best effort
(container UID/GID 10000 on `instances/*/data` and `shares/*`).

## Scheduling

systemd timer, daily at 02:30, keeping 14 archives:

```ini
# /etc/systemd/system/fleett-backup.service
[Unit]
Description=fleett backup
After=network-online.target

[Service]
Type=oneshot
User=fleett
Environment=FLEETT_ROOT=/opt/fleett
ExecStart=/usr/local/bin/fleett backup --dest /srv/backups/fleett --keep 14
ProtectSystem=strict
ReadWritePaths=/srv/backups /opt/fleett
PrivateTmp=true
```

```ini
# /etc/systemd/system/fleett-backup.timer
[Unit]
Description=Daily fleett backup

[Timer]
OnCalendar=*-*-* 02:30:00
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
systemctl daemon-reload
systemctl enable --now fleett-backup.timer
systemctl list-timers fleett-backup.timer
journalctl -u fleett-backup.service -n 20
```

`/srv/backups` must be owned by the service user (`fleett`) and needs room for `--keep`
archives. Do not nest it inside `FLEETT_ROOT`: it would double every backup and slow the
walk (it still works, `instances/` and `shares/` are the only trees copied).

### vzdump / PBS

`fleett` files are plain directories and a SQLite file, so a host-level job is enough:

- include `/opt/fleett` (or `~/.fleett`) and `/etc/fleett`;
- exclude nothing from the root except a local backup directory if you configured one inside
  it — `fleett backup` already skips secrets, but a vzdump copy of the whole root does not;
- the Docker volumes of the agents are not created by fleett: the compose files bind-mount
  `instances/<id>/data` and `shares/<name>`, both inside the root, so they are covered;
- `/etc/fleett/secrets.key` must be in the same job (or in a separate secret backup):
  without it the encrypted rows restored from `fleet.db` cannot be decrypted.

Restoring a PBS snapshot of the whole CT is equivalent to restoring the root; keep a
`fleett backup` archive as well, it survives a broken Docker/Compose installation and can be
inspected without restoring a machine.

## Instance archive (export / import)

```bash
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  http://127.0.0.1:8090/api/instances/alice/export -o alice.tgz

curl -fsS -X POST -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H 'Content-Type: application/gzip' \
  --data-binary @alice.tgz \
  'http://127.0.0.1:8090/api/instances/import?id=alice-copy&start=false'
```

- `manifest.json` carries format/version, `created_at` and the instance metadata (id, name,
  template, model, base_url, api_key_env, channel, mem_limit_mb, cpu_limit, allow_lan,
  shares, notes);
- `compose.yaml` and `data/` (the full Hermes home) are included; share contents are not;
- the import rewrites the Compose project and container to `fleett-<new_id>`, drops every
  `env_file` and re-renders the compose file with this build, keeping the `${KEY}` token
  names;
- it validates the id (free, else `409 instance_exists`), the manifest (else `400
  invalid_manifest` / `invalid_archive`), every entry path (absolute or `..` → `400
  invalid_archive`, nothing written) and the compressed size (`413 archive_too_large`,
  2 GiB);
- the imported instance has **no secret**: the response `warnings` list the variables to
  PATCH (`HERMES_GATE_PROFILE_TOKEN`, `TELEGRAM_BOT_TOKEN`, …). Nothing is started unless
  `start=true`;
- shares named by the manifest are attached only when they already exist on the target host,
  otherwise the warning says which ones to create.

See `API.md` for the full HTTP contract.
