# Instance history — git for the agent home

Editing `SOUL.md` in the console used to be a one-way door: the timestamped
`.bak` copy kept a single generation and told nothing about *why* a line had
changed. Every instance now owns a git repository that records the files the
console can edit, and the API exposes the log, the diffs, the drift and a
revert.

| Method | Route | Role | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/git/log?path=&limit=50` | viewer | commits, newest first |
| GET | `/api/instances/{id}/git/diff?from=&to=` | viewer | unified patch text |
| GET | `/api/instances/{id}/git/status` | viewer | `modified` / `untracked`, whitelist only |
| POST | `/api/instances/{id}/git/revert` | admin | restore the files of one commit |

`git2` (libgit2) is linked into the binary: the `.deb`, the container image and
an offline host need no `git` executable. Nothing here talks to Docker, so the
history of a stopped instance is fully readable and revertable.

## Layout

The repository lives at `FLEETT_ROOT/instances/<id>/.git`; its work tree is the
instance directory itself. Stored paths are therefore the real ones
(`data/SOUL.md`, `data/config.yaml`, ...), not paths relative to the data
directory:

```
FLEETT_ROOT/instances/alice/
├── .git/                 repository (history of the whitelist)
├── compose.yaml          not versioned
├── secrets.env           not versioned (legacy plaintext, migrated away)
└── data/
    ├── SOUL.md           versioned
    ├── config.yaml       versioned (${KEY} placeholders only)
    ├── healthcheck.py    not versioned (rendered probe)
    ├── skills/*.md       versioned
    ├── sessions/         not versioned (runtime state)
    └── state.db          not versioned
```

Exporting an instance walks `data/` and `compose.yaml` only, so a `.tgz` never
carries `.git`; importing one starts a fresh history on the target host. A
purge delete (`DELETE /api/instances/{id}?purge=true`) removes the repository
with the rest of the instance directory.

## Whitelist

`history::is_tracked` is the single source of truth. A `.gitignore` would not be
enough: a path outside the whitelist must never reach the index, so every commit
rebuilds the index from this list and adds explicit paths only.

Versioned:

* `data/SOUL.md` and `data/config.yaml` (even though `config.yaml` is rendered);
* `data/skills/**` when the entry is a regular UTF-8 text file;
* every other file under `data/` whose extension the editor accepts:
  `.md .yaml .yml .json .txt .toml`;
* files are capped at 512 KiB (`files::MAX_CONTENT_BYTES`); larger, binary and
  symlinked entries are ignored.

Never versioned:

* `data/healthcheck.py`, `data/secrets.env`, `*.db`, `*.db-wal`, `*.db-shm`,
  `*.sock`, `*.key`;
* the timestamped `.bak-YYYYMMDD-HHMMSS` copies the editor writes;
* the runtime directories `logs/`, `sessions/`, `node_modules/`,
  `__pycache__/`;
* anything outside `data/` — in particular `compose.yaml`, `secrets.env`.

A deletion performed through the API (or on disk) is recorded: the next commit
simply stops including the path, which git stores as a deletion.

Secrets never enter a file to begin with: they are encrypted in `fleet.db`, and
the rendered `config.yaml` carries `${KEY}` placeholders. A test walks *every*
object of the repository (reachable or not) and asserts the known channel token
is absent, and that `.bak-*`/`secrets.env` never appear in a commit.

## Who wrote what

| Trigger | Author | Message |
|---|---|---|
| Save/delete in the file browser | the authenticated actor | `ui: data/SOUL.md` |
| Revert through the API | the admin who reverted | `ui: data/SOUL.md` |
| Change detected outside the API | `fleett <fleett@localhost>` | `sync: data/SOUL.md` |
| Several paths in one commit | same | `sync: data/SOUL.md (+2 more)` |

For an OIDC session the author is `name <email>` from the ID token (falling back
to the email, then to `sub`). A bearer token — the bootstrap `FLEETT_TOKEN` or
a named API token — has no email, so the author is
`<actor> <actor@fleett.local>` (`bootstrap <bootstrap@fleett.local>`). Names and
emails are sanitised to what `git` accepts (`<`, `>` and control characters
replaced, non-email characters in the email replaced by `-`).

`sync:` commits are produced by the reconcile hook, which runs:

* at startup, for every instance directory (`AppState::bootstrap`);
* after a successful compose apply (`docker compose up -d`), so every mutation
  that regenerates `config.yaml` also records it.

The first boot after the upgrade creates the repository and records one
`sync:` commit per instance, naming every versioned file it found on disk.

## Log, diff, status

`GET .../git/log` returns the newest commits first:

```json
{"commits":[{"id":"9f2c…","short":"9f2c1ab","author":"Cleeryy",
             "email":"cleeryy@example.test","at":"2026-11-09T12:04:11Z",
             "message":"ui: data/SOUL.md","paths":["data/SOUL.md"]}]}
```

`path` accepts a repository path (`data/SOUL.md`) or a virtual one
(`/SOUL.md`) and filters on the paths a commit changed. `limit` is 1..500 and
defaults to 50. A path outside the whitelist is `400 untracked_path`.

`GET .../git/diff` returns unified patch text. `to` defaults to `HEAD`; `from`
defaults to the first parent of `to` (the empty tree for the root commit), so
the call without arguments shows what the last commit changed. Both resolved
ids are echoed in the response (`from` is empty when there is no parent).

`GET .../git/status` compares the work tree with `HEAD`, whitelist only:

```json
{"modified":["data/SOUL.md"],"untracked":["data/notes.md"]}
```

`modified` includes a versioned path deleted on disk. Anything the whitelist
ignores (a `.bak`, a database, `healthcheck.py`) never appears, so the console
can show a clean tree even though the agent keeps writing its runtime files.

## Revert

```json
POST /api/instances/alice/git/revert
{"commit":"9f2c1ab","paths":["/SOUL.md"],"restart":true}
```

* `commit` accepts a full or short id, or any revision `revparse` understands
  (`HEAD`, `HEAD~2`); an unknown one is `404`.
* `paths` is optional: omitted (or empty), every path the commit itself changed
  is restored. A path present in the commit is written back; a path deleted
  since the commit is deleted again.
* every overwritten or deleted file is first copied to a
  `.bak-YYYYMMDD-HHMMSS` sibling, like any other write;
* a path that goes through a symlink is refused (`400 path_escape`), exactly
  like the editor;
* the restore is committed as a `ui:` commit attributed to the caller, so the
  revert itself is in the history (and can be reverted);
* `restart: true` restarts the instance after the files are back; the response
  carries `restarted` and the resulting status;
* the audit trail gets an `instance.git.revert` row with the source commit, the
  restored paths and the new commit id.

```json
{"source":"9f2c1ab…","paths":["data/SOUL.md"],
 "commit":{"id":"1a2b…","short":"1a2b3c4","author":"Cleeryy",
           "email":"cleeryy@example.test","at":"2026-11-09T12:05:02Z",
           "message":"ui: data/SOUL.md","paths":["data/SOUL.md"]},
 "restarted":true,"status":"running"}
```

## Limits

* **No garbage collection.** Nothing calls `git gc`; libgit2 writes loose
  objects and small pack files. A `.git` of an instance edited every day stays
  in the low megabytes for a long time, but it only grows. `fleett backups`
  carries it (it archives the whole data root), and a future release can run a
  bounded `pack` on demand.
* **One reconcile per compose apply**, not per file write outside the API: a
  change made over SSH is recorded at the next start/stop/patch/share change or
  at the next startup, not while fleett is idle.
* **Every commit is a full snapshot of the whitelist**: the index is rebuilt
  from disk, so a commit cannot "forget" a path that did not change. This is
  also what makes a reconcile cheap to reason about.
* **No branches, tags or remotes.** `HEAD` is `refs/heads/main`; history is
  linear (`ui:`/`sync:` commits only).
* **A clone does not inherit the history**: `POST .../clone` copies `data/` and
  the clone starts its own repository. An imported archive does the same.
* **History is host-local**: it is not part of an export, and nothing is pushed
  anywhere.
