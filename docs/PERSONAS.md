# Personas — reusable personalities, versioned and assignable

A persona is the character an agent runs with: a Markdown template for
`data/SOUL.md` plus the variables that template is rendered with. Write it once,
assign it to three instances, and a correction propagates everywhere instead of
being repeated file by file.

A persona is not a template: a template decides the Hermes `config.yaml`, the
environment schema and the resource defaults of a new instance. A persona only
decides the voice, and can be swapped on a running instance without recreating
it.

## Object

```
$FLEETT_ROOT/personas/
├── .git/                    # one repository for the whole directory
└── house/
    ├── persona.yaml         # metadata and variables
    └── SOUL.md              # template
```

`persona.yaml`:

```yaml
name: House
description: The shared voice of the fleet
variables:
  - key: tone
    description: How direct the agent is
    default: direct
  - key: language
    description: Answer language
    default: français
tags: [ops, french]
```

| Field | Required | Bound |
|---|---|---|
| `id` (directory name) | yes | `[a-z0-9-]{2,32}`, must start with a letter or a digit |
| `name` | yes | 128 characters |
| `description` | no | 4096 characters |
| `variables` | no | 64 entries |
| `variables[].key` | yes | `^[a-z][a-z0-9_]{0,31}$`, unique |
| `variables[].description` | no | 1024 characters |
| `variables[].default` | no | 4096 characters |
| `tags` | no | 32 tags, 32 characters each, deduplicated |
| `SOUL.md` | yes | 64 KiB of UTF-8, no NUL byte |

The directory is written `0700`, both files `0600`. The files are the source of
truth: a persona edited over SSH is picked up on the next call, exactly like a
custom template. The database only records which persona each instance applies.

## Variables

A placeholder is `{{key}}` — inner spaces are allowed (`{{ tone }}`) — where
`key` matches the variable grammar above. Anything else is left untouched, so
`{{MODEL}}`, `{{BASE_URL}}`, `{{AGENT_NAME}}` and `{{API_KEY_ENV}}` keep working
in a persona template that also feeds the instance renderer, and a stray
`{{ not a key }}` is plain text.

A key is resolved in this order:

1. `agent_name` — the instance name, when an instance is known (preview with
   `instance`, assignment, apply);
2. the instance values (`persona_vars`, or the `apply` overrides);
3. the persona default (`variables[].default`).

An unresolved key is reported in `missing` (sorted), kept verbatim in the
rendered text, and refuses an apply: an agent never receives a literal
`{{tone}}`. Declaring a variable without a default is therefore how a persona
requires a value — the console shows it as a required field.

The instance stores only the values it overrides. A default changed on the
persona changes every instance that did not pin that key, and the next render
picks it up.

## Assignment

An instance carries three persona fields, on `GET /api/instances` and
`GET /api/instances/{id}`:

| Field | Meaning |
|---|---|
| `persona` | persona id, or `null` |
| `persona_vars` | the values this instance overrides (`{}` when none) |
| `persona_state` | `applied`, `diverged` or `none` |

Assigning at creation:

```bash
curl -X POST https://fleett.example/api/instances \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H "Content-Type: application/json" \
  -d '{"id":"probe","name":"Probe","template":"guest",
       "persona":"house","persona_vars":{"tone":"dry"}}'
```

`POST /api/instances` applies `persona` in this order: the request value, an
explicit `null` (no persona), then the template metadata
(`TemplateInfo.persona`, optional; an instance saved as a template keeps its
persona as the template default). An explicitly requested unknown persona is a
`404`, a missing variable a `400 missing_persona_variables`, both before any file
is written. A *template* default that no longer resolves (the persona was
deleted) is ignored instead of blocking the creation.

Assigning on a running instance:

```bash
curl -X PATCH https://fleett.example/api/instances/probe \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H "Content-Type: application/json" \
  -d '{"persona":"house","persona_vars":{"tone":"warm"}}'
```

A `PATCH` that mentions `persona` or `persona_vars` renders the template, snapshots
the instance (`auto: persona`, issue #38), writes `data/SOUL.md` (previous bytes
copied to `SOUL.md.bak-YYYYMMDD-HHMMSS`), commits the file to the instance
history as `ui: data/SOUL.md`, stores the binding and restarts the container.
`persona_vars` is merged into the current values and a `null` value deletes one
key.

`persona: null` detaches: the binding is cleared and `SOUL.md` is **not**
touched, so the agent keeps its current text until someone edits or replaces it.

## Divergence

`persona_state` is computed on every read, never stored:

* `none` — no persona assigned;
* `applied` — the current `data/SOUL.md`, byte for byte, equals a fresh render
  with the currently stored values;
* `diverged` — everything else: a hand edit through the file browser or SSH, a
  persona whose template changed since the last application, a deleted persona,
  a missing `SOUL.md`, or values that no longer resolve.

The stored `persona_hash` (SHA-256 of the last applied render) is informational:
divergence compares the file with a live render. Editing the persona template
therefore marks every assigned instance `diverged` — even though its own file
was never touched — until the persona is applied again.

To converge again, apply the persona (individually or through the bulk route
below). Applying is idempotent: an instance already in sync is rewritten with
the same bytes, committed only if the tree changed, and restarted.

## Propagation

```bash
curl -X POST https://fleett.example/api/personas/house/apply \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H "Content-Type: application/json" \
  -d '{"instances":["probe","tchouniz"],"variables":{"language":"english"}}'
```

```json
{"results":[{"id":"probe","status":"applied"},
            {"id":"tchouniz","status":"failed","error":"unknown instance \"tchouniz\""}]}
```

Instances are processed **sequentially**, in the request order, duplicates
collapsed (200 maximum). Each one goes through the same path as a single
assignment: render, snapshot, write `SOUL.md` (backup + instance git commit),
store the binding, restart. A failure is reported in its own row and never
aborts the remaining instances — a stopped host or a missing variable does not
block the rest of the fleet.

Every attempt appends an `instance.persona.apply` audit row (success `ok`,
failure `failed` with the error code and message). The call itself appends one
`persona.apply` row summarising how many instances were applied. The `variables`
of the request override the stored values of every targeted instance and are
persisted there.

## API

| Method | Route | Role | Notes |
|---|---|---|---|
| GET | `/api/personas` | any | gallery: id, name, description, tags, variables, `instances_count`, `excerpt`, timestamps |
| POST | `/api/personas` | admin | `201`; `409 persona_exists`; `400 invalid_persona` |
| GET | `/api/personas/{id}` | any | detail: `soul_md`, variables, `instances` |
| PATCH | `/api/personas/{id}` | admin | partial; `description:""` clears it |
| DELETE | `/api/personas/{id}` | admin | `204`; `409 persona_in_use` with `details.instances` |
| POST | `/api/personas/{id}/preview` | any | `{"variables":{…}?,"instance":"probe"?}` → `{"rendered","missing":[…]}` |
| POST | `/api/personas/{id}/apply` | admin | per-instance report, `variables` optional |
| GET | `/api/personas/{id}/git/log?limit=50` | any | commits touching this persona, newest first |
| GET | `/api/personas/{id}/git/diff?from=&to=` | any | unified patch, restricted to `{id}/` |

`preview` seeds the overrides from the instance when one is named (its
`persona_vars` and its name as `agent_name`), then the request `variables` win.
It never writes anything. Errors share the usual envelope:
`400 invalid_persona` (id, key, size), `400 invalid_persona_vars` (bad key in
`persona_vars`), `400 missing_persona_variables` (apply only, with
`details.missing`), `404` for an unknown persona, instance or commit, `409
persona_in_use` / `persona_exists`, and `403 forbidden` for a viewer mutation.

The viewer role is read-only: it can list, read, preview and browse history but
cannot create, edit, delete or apply.

## Git

`$FLEETT_ROOT/personas/.git` is one repository whose work tree is the personas
directory. `git2` (libgit2) is used directly, like the instance history: the
`.deb` and the container image need no `git` binary.

* Every save (create, update) commits as `ui: persona <id>`, attributed to the
  authenticated actor (`Name <email>` for an OIDC session,
  `<token> <token@fleett.local>` for a bearer token). A save that changes nothing
  creates no commit.
* A deletion is committed too (files removed from the index), so the history of
  a deleted persona still shows the removal in the commits of the others.
* Only `personas/<valid-id>/persona.yaml` and `personas/<valid-id>/SOUL.md`
  ever reach the index. A `secrets.env`, a note, a nested directory or a file in
  a directory whose name is not a valid id can never be committed, even if it
  sits inside the work tree.
* `GET .../git/log` filters the commits to the paths of one persona;
  `GET .../git/diff` restricts the patch to its directory. Without `from`, the
  diff shows what the last commit changed; without `to`, it ends at `HEAD`.

## Template metadata

`template.yaml` (and `POST/PUT /api/templates`) accepts an optional `persona`:
the persona a new instance created from that template inherits when the create
request does not name one. Built-in templates have none, and an explicit
`persona: null` in the create request opts out.

## Operational notes

* Applying creates a restore point first (`snapshots::auto_before`, reason
  `persona`), so a wrong propagation can be rolled back like any other change.
* `data/SOUL.md` is written `0600` and, when the process may, owned by the agent
  uid/gid (`10000:10000`); the previous file is kept next to it as
  `SOUL.md.bak-YYYYMMDD-HHMMSS`.
* Persona files are not encrypted: they must never contain a secret. The git
  whitelist enforces it for the persona repository.
* There is no CLI command for personas yet; use the API or the console.
