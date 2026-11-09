# GitOps: declarative fleet state

`fleett` can converge a whole fleet from one versioned document instead of a
sequence of API calls. The document describes providers and instances; `plan`
shows what would change, `apply` executes it, and the stored copy becomes the
drift baseline.

```
fleet.yaml  ──plan──▶  create / update / delete
            ──apply─▶  providers + instances + desired_state
                              │
                              └──drift──▶ plan recomputed from the stored copy
```

Every mutation goes through the same store, render and driver code as the HTTP
API, so a declarative instance is indistinguishable from one created with
`POST /api/instances`.

## Document

```yaml
# fleet.yaml — secret values never appear in this file.
providers:
  - name: shared
    kind: openai-compatible          # openai-compatible | anthropic | google
    base_url: https://opencode.ai/inference/go/openai/v1
    api_key_env: HERMES_GATE_PROFILE_TOKEN
    secret_ref: env:FLEETT_PROVIDER_KEY

instances:
  - id: alice
    template: guest                  # default: guest
    name: Alice                      # default: the id
    model: deepseek-v4.1-flash       # default: template default model
    provider: shared                 # default: no provider
    mem_limit_mb: 1536               # 256..8192, default 1536
    cpu_limit: 1.0                   # 0.1..8.0, default 1.0
    allow_lan: false                 # default false
    notes: "first agent"
    env:
      - key: DISCORD_ALLOWED_USERS
        value: "123,456"             # non-secret values only
      - key: TELEGRAM_BOT_TOKEN
        secret_ref: env:ALICE_BOT_TOKEN
    shares:
      - name: findings
        mode: rw                     # rw | ro

# Accepted for the upcoming scheduler; validated but not reconciled yet.
schedules:
  - instance: alice
    cron: "0 3 * * *"
    command: "consolidate memory"
```

Unknown keys are rejected (`deny_unknown_fields`), so a typo fails the plan
instead of being ignored.

### Defaults and full replacement

The file is the whole fleet:

* an instance or provider it does not declare is a **deletion candidate**;
* an instance's `env` is its complete environment — a key removed from the
  file is removed from the instance;
* omitted scalar fields take their stated default (for example `name` falls
  back to the id, `template` to `guest`), not the current value;
* `image` and `channel` are outside the schema for this version: they are kept
  as they are, so `fleett upgrade` and the messaging tokens do not fight with
  the document.

### Providers

A provider is referenced by **name**. On the first apply it is created with a
generated uuid; later applies find it by name and update `kind`, `base_url` and
`api_key_env` in place.

`secret_ref` is required to create a provider. On an existing provider it is
optional: when present, the referenced secret is resolved and compared with the
stored one, and only a real difference rotates the key (restarting the attached
instances); when absent, the stored secret is left untouched.

Instances adopt the provider's `base_url` and `api_key_env`. An instance
declared without a provider keeps its current endpoint on update, and falls
back to `https://opencode.ai/inference/go/openai/v1` +
`HERMES_GATE_PROFILE_TOKEN` at creation.

## Secret references

A value never belongs in the document or in a plan.

| Reference | Resolution |
|---|---|
| `env:NAME` | environment of the control plane process (or of `FLEETT_ROOT/fleett.env`) |
| `file:/path/to/secret` | absolute file contents, surrounding whitespace trimmed |
| `infisical:…` | **refused**: not implemented, inject the value with `env:` |

References are resolved **at apply time only**. `fleett plan` never reads a
secret, so a plan is safe to paste into a pull request or an issue. An
unresolvable reference fails the apply with `invalid_secret_ref` and nothing is
written for that resource.

## Plan

`GET`-equivalent, read-only:

```bash
fleett plan -f fleet.yaml
fleett --json plan -f fleet.yaml
```

```
plan: 1 create, 1 update, 0 delete, 1 refused
  + instance alice
  ~ instance bob
      model: deepseek-v4.1-flash -> another-model
  - instance carol (refused: pass --allow-delete)
```

The JSON form (`--json`, or `POST /api/desired-state/plan`) is a `Plan`:

```json
{
  "create": [{"action":"create","resource":"instance","id":"alice","name":"alice"}],
  "update": [{"action":"update","resource":"instance","id":"bob","name":"bob",
              "fields":[{"field":"model","from":"deepseek-v4.1-flash","to":"another-model"}]}],
  "delete": [],
  "refused": [{"action":"delete","resource":"instance","id":"carol","name":"carol"}]
}
```

An equivalent document produces an **empty plan**: applying twice is
idempotent (`create`, `update`, `delete` and `refused` all empty).

## Apply and deletion

```bash
fleett apply -f fleet.yaml                 # creates and updates
fleett apply -f fleet.yaml --dry-run       # same as plan
fleett apply -f fleet.yaml --allow-delete  # also removes what the file omits
```

Creates and updates always run. Deletions are only executed with
`--allow-delete` (API: `allow_delete: true`); without it they are reported in
`refused` and the instance stays. This is what protects an instance removed
from the file by mistake.

A provider deletion is refused with `409 gitops_conflict` while an instance
still references it — declare the provider again, or delete the instance in the
same document.

Apply order: providers (create, update), instances (create, update), instance
deletions, provider deletions. The document is stored only after every mutation
succeeded; a failed apply leaves the previous drift baseline untouched.

Every apply is audited as `gitops.apply` with the counts and the source.

## Drift

The applied document is stored in `desired_state` (one row). Drift is the plan
recomputed from that stored copy against the live index:

* a model, limit, share, env or provider changed by hand (or by another tool)
  appears as an `update` with field-level diffs;
* an instance created outside the file appears as a `refused` deletion;
* re-applying the document clears the drift.

Secret values cannot be compared without resolving them: drift only checks the
presence and secret flag of secret variables, and the metadata of a provider
(never its key).

```bash
fleett desired-state            # stored document + drift
curl -H "Authorization: Bearer $FLEETT_TOKEN" \
     http://127.0.0.1:8090/api/desired-state/drift
```

`GET /api/desired-state` returns the raw text (`document_text`), a JSON view
(`document`) and the drift. `GET /api/desired-state/drift` returns only the
recomputed plan.

## Reconciliation

```bash
fleett reconcile --watch -f fleet.yaml --interval 60
```

The command applies the document every 60 seconds and stops on SIGINT. Errors
are printed and the loop continues: a transient Docker or API failure must not
stop the reconciler. Run it under systemd (or a container) on the control plane
host, with the secret environment variables in the unit. Without `--watch` it
performs a single apply.

## API

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/desired-state/plan` | body = document (or `{"document":…}`), returns the `Plan` |
| `POST` | `/api/desired-state/apply` | body = `{"document":…,"allow_delete":false}`, returns the `ApplyReport`, admin only |
| `GET` | `/api/desired-state` | stored text, JSON view and drift |
| `GET` | `/api/desired-state/drift` | recomputed drift plan |

`document` may be the raw YAML text or a JSON object. `allow_delete` may also be
passed as `?allow_delete=true`; `source` is recorded verbatim (default `api`).
The CLI sends `cli` (or `reconcile`), which is what `desired_state.source`
shows.

## Limitations (this version)

* `schedules` is parsed and validated but not reconciled: no scheduler exists
  yet.
* Notifications and maintenance windows are not in the schema.
* `image` and the messaging channel are not managed by the document.
* Deletion never purges an instance `data/` directory; `DELETE
  /api/instances/{id}?purge=true` remains the explicit way to erase it.
* `infisical:` references are refused; use `env:`.
