# Skills — one versioned registry, mounted read-only

A skill is a small tree of text files an agent loads from
`/opt/data/skills/<name>`: a `SKILL.md` plus whatever it references. Skills used
to be copied into every instance that needed them; the registry keeps one copy,
versions it, and mounts it read-only where it is declared.

A skill is not a template and not a persona. A template decides the Hermes
`config.yaml` of a **new** instance, a persona decides the voice, a skill is
reference material an agent reads while it works. Several skills can be mounted
at once, and the same skill can be shared by any number of instances.

## Layout

```
$FLEETT_ROOT/skills/
├── .git/                    # one repository for the whole directory
├── homelab/
│   ├── latest               # "v2" — what followers mount
│   ├── v1/
│   │   ├── skill.yaml       # name, description
│   │   └── SKILL.md
│   └── v2/
│       ├── skill.yaml
│       ├── SKILL.md
│       └── notes/proxmox.md
└── web/
    ├── latest
    └── v1/
        ├── skill.yaml
        └── SKILL.md
```

| Field | Required | Bound |
|---|---|---|
| `name` (directory) | yes | `[a-z0-9-]{2,32}`, starts with a letter or a digit |
| `version` (directory) | yes | `^v[1-9][0-9]{0,8}$` (`v1`, `v2`, …) |
| `skill.yaml` `name` | yes in an import | must equal the published name |
| `skill.yaml` `description` | no | 4096 characters |
| `SKILL.md` | yes | non-empty, UTF-8, no NUL byte |
| any other file | no | relative path, text, 256 KiB, 256 files, 1 MiB per version |

Directories are written `0755` and files `0644`: a skill is mounted read-only
and holds no secret. The files are the source of truth — a skill copied in by
hand is listed and mountable on the next read — and the database only records
which instance mounts which skill, with which pin.

Paths are relative and are validated component by component: no absolute path,
no `..`, no `.`, no empty component, no backslash, no NUL byte. Generated files
(`skill.yaml`, `latest`) cannot be uploaded.

## Versions and pinning

Publishing never overwrites: a version that exists is a `409
skill_version_exists`. Publish `v2`, look at it, then move the pointer.

The first published version becomes `latest` automatically; later publishes do
not. An instance declares:

```json
"skills": [{"name": "homelab"}, {"name": "web", "version": "v1"}]
```

* `{"name": "homelab"}` — **follows latest**: the compose file names the
  resolved version (`../../skills/homelab/v2`), so a promote has to restart it;
* `{"name": "web", "version": "v1"}` — **pinned**: it keeps `v1` across any
  promote.

`GET /api/instances/{id}` reports both values per skill:

```json
"skills": [{"name": "homelab", "version": null, "effective_version": "v2"}]
```

`version` is the pin (`null` when following latest), `effective_version` is what
the compose file actually mounts. A reference that cannot be resolved (unknown
skill, pinned version deleted) is refused at create/patch time (`400
unknown_skill`, `400 unknown_skill_version`); a hand-edited compose that names
one is dropped from the render instead of letting Docker create an empty
root-owned directory.

## Read-only mount

Every declared skill adds one volume to `services.agent.volumes`:

```yaml
- ../../skills/homelab/v2:/opt/data/skills/homelab:ro
```

The mount is always `:ro` and there is no write variant: the agent cannot
modify the registry. The registry itself is `0755`, so the container user only
ever reads it. Changing `skills` on an instance rewrites the compose file,
writes the instance files and runs `docker compose up -d` — the normal PATCH
path — and appends an `instance.skills` audit row listing the new bindings.

`skills` is also inherited by a clone (skills are configuration, not secrets)
and carried in an instance archive; an archive imported on a fleet that does not
know a skill drops that reference instead of mounting an empty directory.

## Promote

```bash
curl -X POST https://fleett.example/api/skills/homelab/promote \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H "Content-Type: application/json" \
  -d '{"version":"v2"}'
```

```json
{"latest":"v2",
 "results":[{"id":"probe","status":"restarted"},
            {"id":"tchouniz","status":"failed","error":"timed out after 60s waiting for a healthy container"}]}
```

`promote`:

1. refuses an unknown skill (`404`) or an unpublished version (`404`);
2. writes `latest` and commits it (`ui: skill <name>@<version>`);
3. lists the instances that follow latest (`version IS NULL` in
   `instance_skills`) — pinned instances are not touched;
4. handles them **sequentially**, each under its own instance lock: rewrite the
   compose file with the new mount, `docker compose up -d`, then poll health for
   up to 60 seconds;
5. reports one row per instance; a failure never aborts the remaining ones.

The call appends one `skill.promote` audit row summarising the version and the
outcome of every instance; a failed instance also gets an `instance.skills` row
with `result: failed`.

## Import

```bash
curl -X POST https://fleett.example/api/skills/import \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -F "file=@web.zip"
```

The archive must contain `skill.yaml` (with `name` and at least optionally
`description`) and `SKILL.md`, at its root or inside a single wrapping folder
(`web/SKILL.md` → `SKILL.md`). The published version is the next free one (`v1`
then `v2`), so importing the same archive twice never conflicts.

Every entry goes through the same validation as a JSON publish — zip-slip
(`../x`), absolute paths, symlink entries, non-UTF-8 content, NUL bytes and
oversized entries abort the whole import before anything is written. The upload
is read in memory: 8 MiB compressed, 1 MiB expanded.

## JSON publish

```bash
curl -X POST https://fleett.example/api/skills \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"homelab","version":"v2","description":"Homelab notes",
       "files":[{"path":"SKILL.md","content":"# Homelab\n"},
                {"path":"notes/proxmox.md","content":"CT 119\n"}]}'
```

The response is the skill detail (metadata, versions, files of `latest`). The
version is written into a temporary sibling directory and renamed into place, so
a failed write never leaves a half-published version.

## API

| Method | Route | Role | Notes |
|---|---|---|---|
| GET | `/api/skills` | any | `[{name, description, latest, versions, instances_count, created_at, updated_at}]` |
| GET | `/api/skills/{name}` | any | detail: `versions[]` (files count, size, published_at), `files` of `latest`, `instances` |
| POST | `/api/skills` | admin | `201`; `409 skill_version_exists`; `400 invalid_skill` |
| POST | `/api/skills/import` | admin | multipart `file`; `201`; `400 invalid_skill` |
| POST | `/api/skills/{name}/promote` | admin | `{"version"}`; report per instance |
| DELETE | `/api/skills/{name}` | admin | `204`; `409 skill_in_use` with `details.instances` |
| GET | `/api/skills/{name}/git/log?limit=50` | any | commits touching this skill, newest first |
| GET | `/api/skills/{name}/git/diff?from=&to=` | any | unified patch, restricted to `{name}/` |

The viewer role is read-only: it lists, reads and browses history but cannot
publish, import, promote or delete. Errors use the usual envelope:
`400 invalid_skill` (name, version, path, size, missing `SKILL.md`),
`400 unknown_skill` / `400 unknown_skill_version` / `400 duplicate_skill` on an
instance, `404` for an unknown skill, version or commit, `409
skill_version_exists` / `409 skill_in_use`, `403 forbidden` for a viewer
mutation.

## Limits

| Limit | Value |
|---|---|
| Files per version | 256 |
| Bytes per file | 256 KiB |
| Bytes per version | 1 MiB |
| Path length / component | 200 / 64 characters |
| Directory nesting | 8 levels |
| Description | 4096 characters |
| Skills per instance | 32 |
| Zip upload (compressed) | 8 MiB |
| Health wait after a promote restart | 60 s |

## Git

`$FLEETT_ROOT/skills/.git` is one repository whose work tree is the skills
directory. `git2` (libgit2) is used directly, like the instance history, so the
`.deb` and the container image need no `git` binary.

Every publish and every promote is a commit authored by the operator (`ui:
skill <name>@<version>`); a delete commits the removal as `@deleted`. Only
whitelisted paths ever reach the index — a valid skill name, its `latest` file
and the regular files of a valid version directory — so a stray `.env` dropped
in a skill directory can never be committed. `GET /api/skills/{name}/git/log`
and `.../git/diff` expose the history of one skill only.
