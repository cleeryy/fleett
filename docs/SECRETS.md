# Secrets at rest — encrypted provider and bot tokens

Instance secrets are provider API keys and messaging bot tokens:

| Key | Written by |
|---|---|
| `HERMES_GATE_PROFILE_TOKEN` | `gate_token` on create / patch |
| `<api_key_env>` when it differs from the gate token | `api_key_env` / `gate_token` |
| `TELEGRAM_BOT_TOKEN`, `DISCORD_BOT_TOKEN` | `channel.token` |
| `env` entries with `secret: true` | the API caller (`POST`, `PATCH`, `PUT .../env`) |

Environment variables with `secret: false` are stored in clear and returned by
the API; see [ENV.md](ENV.md).

They used to be written to `instances/<id>/secrets.env`, mode `0600` but plain
text: a `tar czf backup.tgz $FLEETT_ROOT` carried every token, and a leaked
backup exposed them all. They are now stored **encrypted** in the index
database, and the compose file only references their names.

## How it works

- `fleet.db` (inside `FLEETT_ROOT`) holds one row per variable in
  `instance_env`: `instance_id`, `key`, an explicit `secret` flag, and either
  the plaintext `value` (non-secret) or a random 96-bit `nonce` plus the
  AES-256-GCM `ciphertext` of the value (secret), with `updated_at`. See
  [ENV.md](ENV.md) for the editable environment and its API.
- Releases up to v0.4.0 stored one encrypted JSON map per instance in
  `instance_secrets`. It is imported into `instance_env` at startup (and the
  row dropped); the table is kept for one release so a downgrade still works.
- The key is a 32-byte random value, base64-encoded, kept **outside**
  `FLEETT_ROOT`:
  - `FLEETT_SECRETS_KEY` (inline, base64) if set, otherwise
  - `FLEETT_SECRETS_KEY_FILE` (default `/etc/fleett/secrets.key`, mode `0640`).

  Keep `FLEETT_SECRETS_KEY` in the systemd unit (or its environment), **not** in
  `$FLEETT_ROOT/fleett.env`: that file is inside the root and would end up in
  the backup next to the ciphertext.
- The compose file declares each secret as a Compose interpolation:

  ```yaml
  environment:
    HERMES_UID: '10000'
    HERMES_GID: '10000'
    TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}
  ```

  There is no `env_file:` entry any more.
- For every `docker compose` call (`up`, `stop`, `start`, `restart`, `down`,
  `ps`, `logs`) the driver decrypts the instance and injects the values into
  the **child process environment**. Compose resolves `${TELEGRAM_BOT_TOKEN}`
  there; nothing is written to disk.
- A new nonce is generated for every encryption, including rotations, so the
  same token never produces the same ciphertext twice.

A `tar czf - $FLEETT_ROOT` now contains no usable token: only ciphertext, and
the key is not under the root unless you move it there (which
`fleett secrets init` refuses to do).

## First setup

```bash
sudo fleett secrets init
# secrets key: created /etc/fleett/secrets.key
sudo chown root:root /etc/fleett/secrets.key   # already the case when run as root
```

`init`:

- generates 32 random bytes with the OS CSPRNG and writes the base64 form;
- creates the file `0640`, and **refuses to overwrite an existing file**;
- refuses a path inside `FLEETT_ROOT` (a backup of the root must not contain
  the key);
- needs write access to `/etc/fleett` (run it as root).

Then restart the service so the driver picks the key up:

```bash
sudo systemctl restart fleett
```

Check the state:

```bash
fleett secrets status
# key: FLEETT_SECRETS_KEY (environment) | /etc/fleett/secrets.key | none
# key file: /etc/fleett/secrets.key
#   mode 0640
# encrypted instances: 3
# legacy secrets.env files: 0
```

## Migrating an existing host

Instances created before this feature still have a plaintext `secrets.env`,
and their compose file still uses `env_file: secrets.env`. They keep working
in **legacy mode** (the driver reads the file at runtime) and the API returns a
warning for them (`secrets are stored in plain text in secrets.env`), but the
tar problem is only fixed once they are migrated.

```bash
fleett secrets migrate             # dry run: lists files, changes nothing
fleett secrets migrate --apply     # encrypt, rewrite compose, delete files
```

For each instance with a legacy file, `--apply`:

1. reads `instances/<id>/secrets.env`;
2. encrypts every value into `instance_env` (`secret = 1`, fresh nonce each);
3. rewrites `instances/<id>/compose.yaml`: removes `env_file: secrets.env`,
   keeps `HERMES_UID` / `HERMES_GID` as literals, adds `${KEY}` for every key;
4. deletes the plaintext file — only after 1-3 succeeded.

It needs a configured key (`fleett secrets init` first). The file is left in
place if any step fails, so a failed migration never loses a token.

Once an instance has `instance_env` rows, those rows win: the driver reads them
at runtime, and a stale `secrets.env` never reverts a token rotated through the
API (the startup import only rewrites the compose file, and `--apply` deletes
the file). Prefer rotating through the API over editing `secrets.env` by hand.

Running instances keep the environment they started with until they are
recreated. After migrating, apply the new compose file with:

```bash
curl -X POST https://fleett.example/api/instances/alice/restart \
  -H "Authorization: Bearer $FLEETT_TOKEN"
# or, on the host:
docker compose -f $FLEETT_ROOT/instances/alice/compose.yaml up -d
```

### Automatic import at startup

On every start, when a key is configured and a legacy file exists, the control
plane encrypts it into the database automatically when the instance has no
stored environment yet (existing rows win, so a rotation is never reverted); the
plaintext file is **not** deleted, so no data is lost if the import or the
compose rewrite fails. The plaintext file is deleted only when you opt in:

```bash
FLEETT_SECRETS_AUTOIMPORT=1
```

Even then, the file is deleted only after the import and the compose rewrite
succeeded. Leaving the default (`0`) is safe: `fleett secrets migrate --apply`
remains the explicit, audited path, and the startup warning keeps listing the
files.

## Rotating a token

Rotation works exactly as before, and does not need a compose change: the key
name is stable (`TELEGRAM_BOT_TOKEN`, ...), only the value changes.

```bash
curl -X PATCH https://fleett.example/api/instances/alice \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"channel": {"type": "telegram", "token": "123456:NEW-TOKEN"}}'
```

- encrypted instance: the row is replaced with a new nonce and ciphertext;
- legacy plaintext instance: the rotation needs the secrets key (`fleett
  secrets init` then migrate); the API never writes a plaintext token again;
- the compose file is unchanged (same `${KEY}`), so `docker compose up -d`
  starts the container with the new value.

`PATCH` also accepts `gate_token` to rotate the provider gate token.

## What happens without a key

| Situation | Result |
|---|---|
| New instance with tokens, no key, no legacy file | `409 secrets_key_missing` — run `sudo fleett secrets init` |
| New instance with no token at all | created |
| Instance with a legacy `secrets.env`, no key | keeps running (the driver reads the file) and a warning is reported, but API env writes need the key |
| Instance with encrypted values, key missing | every compose call fails with `docker compose … failed: secrets … fleett secrets init`; the API returns `409 secrets_key_missing` on patch |

The API never returns a secret value: `GET /api/instances/{id}` lists the names
and masks secret values (see [ENV.md](ENV.md)).

## Backup implications

- **Back up the key separately** from `FLEETT_ROOT`: `/etc/fleett/secrets.key`
  (or your `FLEETT_SECRETS_KEY` value). Without it, the ciphertext in
  `fleet.db` is unreadable and every agent must be given new tokens.
- **Do not** copy the key into the root, a Docker volume, or the same archive
  as the root. `fleett secrets status` warns if the key path is inside the
  root.
- `fleet.db` now holds the only copy of the encrypted secrets. It used to be a
  rebuildable index; a backup of the root that excludes `fleet.db`, or a
  deleted `fleet.db`, loses the tokens (the agents keep running with the
  environment they already have, but the next `up -d` starts them without
  tokens until you rotate).
- An unencrypted backup is now safe for the root, **unless** it also contains
  the key file. Encrypt the key backup (or store it in a password manager).

## Caveats

- **`docker inspect` sees the values.** Environment variables are part of the
  container configuration: `docker inspect <container>` (and
  `/proc/<pid>/environ` inside the container) show the token in plain text.
  This is inherent to passing secrets as environment variables; it is not a
  disk exposure, but anyone with Docker access on the host can read them. Use
  `docker inspect --format '{{json .Config.Env}}'` yourself if you need to
  audit a container.
- The `docker compose` child process holds the values in its environment for
  the duration of the call; they are not logged (only compose stderr is kept,
  truncated, in API errors).
- The key file is protected by UNIX permissions (`0640`), not by a passphrase.
  Root on the host can read it; that is the trust boundary.
- Rotating the key itself is not supported: decrypting with a new key would
  require re-encrypting every row. Delete `instance_secrets` content and
  re-create tokens (or re-import legacy files) after swapping the key.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `409 secrets_key_missing` on create/patch | no key configured; `sudo fleett secrets init` |
| `cannot decrypt the secrets: authentication failed` | wrong key file, or a tampered/corrupted `instance_secrets` row |
| `docker compose … failed: … ${TELEGRAM_BOT_TOKEN}` | the driver could not read the secrets: check the key and the row |
| container starts without tokens | compose file not rewritten (legacy `env_file`) or started before the rotation: `POST /api/instances/<id>/restart`, or `docker compose up -d` on the host |
| `fleett secrets status` says "the key file is inside FLEETT_ROOT" | move the key out of the root; `fleett secrets init` refuses that path |
