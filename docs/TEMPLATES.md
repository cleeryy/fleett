# Templates — custom, inherited, versioned

A template is the starting point of an instance: the Hermes `config.yaml`, an
optional `SOUL.md`, a schema for the environment variables the agent needs, and
the resource limits a new instance should get.

The three built-in templates (`guest`, `guest-discord`, `solo`) stay compiled
constants and are **read-only**. Everything else lives on disk and can be
created, edited, inherited, saved from a running instance and restored.

## Layout

```
$FLEETT_ROOT/templates/<id>/
├── template.yaml   # metadata (required once the directory is custom)
├── config.yaml     # Hermes config with {{MODEL}} {{BASE_URL}} {{AGENT_NAME}} {{API_KEY_ENV}}
├── soul.md         # starting SOUL.md (optional; SOUL.md is still read)
└── defaults.yaml   # instance-creation defaults (optional)
```

The files are the source of truth. `fleet.db` only indexes them
(`templates`, `template_versions`) and the index is rebuilt from disk at every
startup: a template written over SSH is picked up without an API call.

`template.yaml`:

```yaml
name: House
description: Our house template
parent: guest            # optional
default_model: deepseek-v4.1-flash
channel: telegram        # telegram | discord | none
env:
  - key: HOUSE_FLAG
    description: Feature switch read by the agent
    example: "1"
    required: true       # POST /api/instances must supply it
    format: boolean      # free-form console hint
    secret: false        # store encrypted, never return the value
source_instance: tchouniz # set by from-instance, informational
```

`defaults.yaml`:

```yaml
model: deepseek-v4.1-flash
base_url: https://opencode.ai/inference/go/openai/v1
mem_limit_mb: 2048
cpu_limit: 2.0
allow_lan: false
notes: house policy
```

Every field is optional. `POST /api/instances` uses them only when the request
leaves the matching field out; an explicit request value always wins. When the
request has no `channel`, the template metadata decides (`telegram`, `discord`
or `none`).

The API never writes a template with mode 0755: the directory is `0700` and the
files are `0600`. Template ids follow the instance grammar
(`[a-z0-9-]{2,32}`, starting with a letter or a digit).

## Inheritance

A template can extend another one with `parent`. Resolution merges:

| Field | Merge rule |
|---|---|
| `name`, `description`, `default_model`, `channel` | child value, otherwise the nearest parent's |
| `config.yaml` | the child file when it has one, otherwise the parent's (live inheritance) |
| `soul.md` | the child file when it has one, otherwise the parent's |
| `env` | merged by key, the child entry replacing the parent entry in place |
| `defaults.yaml` | field by field, the child winning |

Inheritance is **live**: a child that does not write `config.yaml` keeps reading
its parent's file, so fixing the parent fixes every child. A child that wants a
snapshot must pass `config_yaml` (or use `from-instance`).

A built-in can be a parent (`parent: guest`) but never the child. Cycles are
detected before any file is written and answered with `400 template_cycle`; the
chain is capped at 32 levels.

A root template (no parent) must have a `config.yaml`. When created through the
API without one, it is seeded from the `solo` built-in config and soul so the
result is immediately usable.

## Environment schema

`env` entries extend the pre-0.6 suggestions with three optional fields:

| Field | Default | Meaning |
|---|---|---|
| `key` | required | `^[A-Z_][A-Z0-9_]{0,63}$`, not a reserved container name |
| `description` | `""` | console help text |
| `example` | `null` | placeholder value |
| `required` | `false` | a missing entry blocks `POST /api/instances` |
| `format` | `null` | free-form hint (`boolean`, `comma-separated`, `user-id`, …) |
| `secret` | `false` | store encrypted, never return the value |

Instance creation:

- every `required` entry must be present in the request `env` list, or supplied
  by `gate_token` / `channel.token` / the attached provider key. A missing one
  is `400 missing_required_env` with
  `details: [{"key":"HOUSE_FLAG","field":"env"}]`;
- an entry that omits `secret` takes the schema flag; an explicit `secret` in
  the request wins;
- the gate and platform tokens are always secret and win over an entry with the
  same key, as before.

## Versions and restore

Every create, update and delete appends a row to `template_versions`:

```
+----+-------------+---------+--------------------------------------+-------+------------+--------+
| id | template_id | version | snapshot (JSON)                      | actor | at         | action |
+----+-------------+---------+--------------------------------------+-------+------------+--------+
```

The snapshot holds the full metadata and file contents (`config.yaml`,
`soul.md`, `defaults.yaml`) plus their SHA-256 are exposed by the API:

```json
{"version": 2, "action": "update", "actor": "admin",
 "at": "2026-11-09T02:00:00Z",
 "sha256": {"config.yaml": "9f2c…", "soul.md": "1ab4…"}}
```

`POST /api/templates/{id}/restore {"version": 2}` rewrites every file from the
snapshot and appends a new `update` version — nothing is erased, so a bad
restore is itself restorable. Deleting a template keeps its history in the
database; re-creating the same id continues the numbering (the old versions
stay readable and restorable).

No git repository is involved: this is a small index table with the same
history semantics as `docs/GIT.md`, without shelling out to git.

## Save an instance as a template

`POST /api/templates/from-instance` (`fleett template from-instance`):

- copies `instances/<id>/data/config.yaml` verbatim into
  `templates/<new>/config.yaml` (the renderer normalises the model block on the
  next instance creation);
- copies `SOUL.md` into `soul.md` when the instance has one;
- derives the env schema from the instance variables (`key`, `secret`), with no
  `required` entry: a template saved from a working instance never blocks a new
  creation;
- records the instance limits and model in `defaults.yaml` (`mem_limit_mb`,
  `cpu_limit`, `allow_lan`, `notes`, `model`, `base_url`);
- sets `channel` from the instance and `source_instance` in the metadata.

That is the "save as template" acceptance path: create a new instance from the
saved template and it starts with the same limits and the same starting soul.

## Limits and errors

| Limit | Value |
|---|---|
| template id | `[a-z0-9-]{2,32}` |
| inheritance depth | 32 |
| name | 128 characters |
| description | 4096 characters |
| env description / example | 1024 characters |
| env format hint | 64 characters |
| `template.yaml` / `defaults.yaml` size | bounded by the API body limit |

| Error | When |
|---|---|
| `400 invalid_template` | bad metadata, YAML or defaults |
| `400 template_cycle` | `parent` chain loops or is too deep |
| `400 missing_required_env` | a `required` env key is absent at creation |
| `400 unknown_template` | an instance references a missing template |
| `404 not_found` | unknown template, instance or version |
| `409 template_exists` | id already taken by a directory, row or built-in |
| `409 read_only` | mutation or deletion of a built-in |
| `409 template_in_use` | an instance (or child template) still uses it; `details.instances` / `details.templates` list them |

## API and CLI

- API: `API.md` § Templates (`GET/POST /api/templates`,
  `GET/PUT/DELETE /api/templates/{id}`,
  `POST /api/templates/from-instance`,
  `GET /api/templates/{id}/versions`,
  `POST /api/templates/{id}/restore`).
- CLI: `docs/CLI.md` § `templates` / `template`
  (`fleett templates`, `fleett template get|create|from-instance|rm|versions|restore`).

All mutations require the admin role and are written to the audit log
(`template.create`, `template.update`, `template.delete`, `template.restore`).
