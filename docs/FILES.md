# Instance files — browsing and editing the agent home

An instance keeps its whole Hermes home in
`FLEETT_ROOT/instances/<id>/data`, bind-mounted as `/opt/data` in the agent
container. Before this feature the only way to edit `SOUL.md`, a profile, a
skill note or a provider list was an SSH session on the host plus a manual
`chown 10000`. The console now reads and writes those files over the API.

| Method | Route | Role | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/files?path=/` | viewer | one directory level |
| GET | `/api/instances/{id}/files/content?path=/SOUL.md` | viewer | text, size, mtime, etag |
| PUT | `/api/instances/{id}/files/content` | admin | compare-and-swap write, `.bak`, optional restart |
| DELETE | `/api/instances/{id}/files/content?path=/notes/old.md` | admin | delete a file or an empty directory |
| POST | `/api/instances/{id}/files/mkdir` | admin | create one directory |

The web console uses exactly this order: tree, editor, save, then an explicit
"Save and restart" choice. The API never restarts on its own when the flag is
absent.

## Path model

Every path is **virtual** and always starts with `/`, which is the *data
directory of the instance*, never the host root:

```
/                       -> FLEETT_ROOT/instances/<id>/data
/SOUL.md                -> FLEETT_ROOT/instances/<id>/data/SOUL.md
/profiles/work/config   -> FLEETT_ROOT/instances/<id>/data/profiles/work/config
```

`..` segments, NUL bytes and relative paths are refused with
`400 invalid_path` before any filesystem call. A host absolute path such as
`/etc/passwd` is just `/etc/passwd` inside the instance data directory: it
either does not exist (`404`) or is an ordinary data file. There is no syntax
that names a file outside the data directory.

`GET /files` without `?path` lists the root. The response is a JSON array,
directories first, then names compared case-insensitively:

```json
[{"name":"SOUL.md","path":"/SOUL.md","type":"file","size":439,
  "mtime":"2026-11-09T00:17:54Z","editable":true,"rendered":false},
 {"name":"profiles","path":"/profiles","type":"dir","size":4096,
  "mtime":"2026-11-09T00:17:54Z","editable":false,"rendered":false}]
```

`mtime` is RFC 3339 (second precision, UTC). `type` is `file` or `dir`; a
socket, FIFO or device is never listed, read, deleted or written.

## Confinement and symlinks

The parent chain of every operation is canonicalized (`canonicalize`) and must
stay under the canonical data directory; a symlinked directory that points
outside is refused with `400 path_escape` before anything is read or written.
This applies to `list`, `read`, `write`, `delete` and `mkdir`.

A final symlink is handled per operation:

* **read** follows it, but only when its resolved target is inside the data
  directory (an escaping or dangling link is `400 path_escape` / `404`);
* **write** follows it the same way, then re-checks the opened inode
  (`dev` + `ino`) against the file it hashed, so a symlink or hard-link swap
  between the etag check and the write is a `409`;
* **delete** removes the link itself, after checking its target; the target file
  is never deleted through the link;
* **list** skips entries that escape, so the tree never advertises a path the
  API would refuse to open.

Creating a file uses `O_CREAT|O_EXCL`, which never follows a symlink planted
between the check and the open.

## Limits

| Limit | Value | Refusal |
|---|---|---|
| Read/write size | 512 KiB (`MAX_CONTENT_BYTES`) | `413 file_too_large` |
| Text only | valid UTF-8, no NUL byte | `415 binary_file` |
| Editable extensions | `.md .yaml .yml .json .txt .toml` | `409 file_not_editable` |
| One directory level | no recursive listing | — |

A file above the cap is still listed (with `editable:false`) but cannot be read
or written; delete it and recreate a smaller one. Reading is UTF-8 strict: a
Latin-1 or NUL-bearing file is `415` rather than mojibake.

## Rendered files

fleett owns a few files in the data directory and rewrites them on every
PATCH, upgrade or rollback:

| File | Rendered by |
|---|---|
| `data/config.yaml` | the template + the instance spec (see `render_config`) |
| `data/healthcheck.py` | fixed probe script (`render::HEALTHCHECK_SCRIPT`) |

`compose.yaml` is outside `data` and therefore not reachable at all. The two
rendered files are listed with `editable:false` / `rendered:true`; reading them
returns a `note`, and writing or deleting one returns
`409 file_regenerated` with `error.details {file, rendered: true, hint}`. The
check uses the resolved target, so a symlink or a symlinked directory pointing
at a rendered file is refused the same way. To change them, PATCH the instance
(model, channel, shares, env): the next render wins, by design.

## Concurrency: etag

`GET /files/content` returns the SHA-256 of the bytes on disk as `etag`
(lowercase hex, 64 characters). `PUT` must send the version the editor loaded,
either in the body or in the standard `If-Match` header (the header wins):

```http
PUT /api/instances/alice/files/content
If-Match: "9f0c…"

{"path":"/SOUL.md","content":"…","etag":"9f0c…"}
```

* the value must equal the current `etag`; otherwise `409 etag_mismatch` with
  `error.details.current` (the version now on disk), and **nothing is written**;
* `If-Match: *` means "the file must exist" (any version);
* an existing file without an etag is a conflict, never an unconditional
  overwrite; a missing file with no etag (or an empty one) is a creation.

Two browser tabs editing the same file therefore cannot lose an update: the
second `PUT` is refused and the console reloads.

## Backups

Before overwriting an existing file, the current bytes are copied next to it:

```
SOUL.md.bak-20261109-183512
```

`YYYYMMDD-HHMMSS` is UTC; a second write in the same second gets a `-1`, `-2`
suffix. The backup path is returned in the `PUT` response (`backup`, virtual
path) and appears in the next listing. Creating a new file writes no backup,
and creating/overwriting never touches a `fleet.db`: a `../../fleet.db` attempt
is `400 invalid_path` and writes nothing.

Backups are ordinary data files: they are browsable, they are not editable
(the extension is not in the allowlist), and they are removed with the instance
directory when the instance is deleted with `?purge=true`.

## Restart semantics

A file write changes bytes on disk only. Hermes reads `config.yaml` and
`SOUL.md` at gateway startup, so the change applies on the next start:

* `PUT …/files/content` (no flag): write only, `restarted:false`, no `status`;
* `PUT …/files/content?restart=true`: write, then `docker compose restart`.
  The response carries `restarted:true` and `status` (the observed status after
  the restart, e.g. `running`).

The write is audited *before* the restart, so a driver failure (502
`driver_error`, instance marked `error`) never hides a mutation that already
happened. Without `?restart=true` the console must show the "pending restart"
banner itself; nothing restarts implicitly.

## Audit

Every mutation appends one row to `GET /api/audit`:

| Action | `target` | `details` |
|---|---|---|
| `instance.file.write` | instance id | `{path, size, backup, restart}` |
| `instance.file.delete` | instance id | `{path}` |
| `instance.file.mkdir` | instance id | `{path}` |

The actor is the SSO email (or the token name), the role and the credential
type as for every other mutation. Reads are not audited.

## Errors

| Status | Code | Cause |
|---|---|---|
| 400 | `invalid_path` | `..`, NUL, relative path |
| 400 | `path_escape` | symlink or parent chain outside the data directory |
| 400 | `not_a_file` | content endpoint on a directory (or on the root) |
| 400 | `not_a_directory` | listing a file |
| 400 | `unsupported_file_type` | socket, FIFO, device |
| 403 | `forbidden` | viewer mutation, CSRF missing on a cookie session |
| 404 | `not_found` | unknown instance, path or dangling link |
| 409 | `etag_mismatch` | stale or missing etag (`details.current`) |
| 409 | `file_regenerated` | rendered `config.yaml` / `healthcheck.py` |
| 409 | `file_not_editable` | extension outside the allowlist |
| 409 | `already_exists` | `mkdir` on an existing path |
| 409 | `directory_not_empty` | `DELETE` on a non-empty directory |
| 413 | `file_too_large` | above 512 KiB (`details {size, limit}`) |
| 415 | `binary_file` | not UTF-8, or contains NUL |
| 500 | `internal` | filesystem failure |
| 502 | `driver_error` | the optional restart failed (the write stands) |

## Ownership

The control plane runs as root (or as the `fleett` service user) and the agent
container runs as UID/GID `10000`. A successful write and `mkdir` hand the
result back to `10000:10000` (best effort, like the rest of `store`), and an
overwrite preserves the mode of the file. `docker exec` is no longer needed to
keep the agent's own home writable.

## Not in scope

No recursive tree, no rename, no upload/binary transfer, no file content in the
audit log, and no editing outside the data directory (compose file, secrets,
`fleet.db`). Templates are edited through the template directory on the host,
not through this API.
