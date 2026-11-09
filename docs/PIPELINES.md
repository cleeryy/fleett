# Pipelines (DAG)

A pipeline is a small directed acyclic graph (DAG) of agent prompts. It is what
turns a fleet from *N independent agents* into a workflow: `produce` on one
instance, `verify` on another, `publish` on a third, with the output of each
step available to its dependents.

Pipelines are stored in SQLite (`pipelines`, `pipeline_runs`, `pipeline_steps`).
Every execution is a new run with a fresh UUID and its own step rows: relaunching
a pipeline never overwrites a previous run. See issue #31.

## Definition

A pipeline is a YAML (or JSON) document:

```yaml
name: release
trigger: manual          # manual | cron | webhook

steps:
  - id: produce
    instance: alice
    prompt: "Draft the release notes from the last 20 commits."

  - id: verify
    instance: bob
    depends_on: [produce]
    prompt: |
      Review this draft and answer with the corrected text only:
      {{steps.produce.output}}
    retries: 1
    timeout_s: 300
    on_failure: stop

  - id: publish
    instance: carol
    depends_on: [verify]
    prompt: "Publish: {{steps.verify.output}}"
```

| Field | Meaning |
|---|---|
| `name` | Unique pipeline name (`[a-z0-9][a-z0-9_-]{1,62}`) |
| `trigger` | `manual` (default), `cron` or `webhook` |
| `cron` | Standard five-field cron expression, required by `trigger: cron` |
| `steps[].id` | Unique id inside the pipeline (`[A-Za-z0-9_-]{1,64}`) |
| `steps[].instance` | Instance that runs the prompt; checked when the run starts |
| `steps[].prompt` | Prompt text, 16 KiB max (placeholders are substituted) |
| `steps[].depends_on` | Step ids that must finish `ok` before this one starts |
| `steps[].retries` | Extra attempts after a failure, `0..=5` (default `0`) |
| `steps[].timeout_s` | Timeout of one attempt, `1..=3600` (default `300`) |
| `steps[].on_failure` | `stop` (default) or `continue` |

The definition is validated before it is stored: unique step ids, existing
dependencies, no cycle (depth-first search), at most 50 steps. Instance ids are
resolved at run time: a pipeline may be created before its agents exist, and
starting a run against a missing instance answers `400 unknown_instance`.

The canonical form is compact JSON. `fleett pipeline get` returns the fields
back; `fleett pipeline apply` accepts YAML or JSON.

## Execution

The engine groups the graph into waves: a step is runnable once every dependency
is `ok`. Runnable steps are launched in parallel, at most four at a time
(`MAX_PARALLEL_STEPS`), and each step state transition is persisted:

`pending → running → ok | failed | skipped`

A step whose dependency failed or was skipped is itself `skipped` (it can never
run), recursively. A run is `ok` when every step is `ok`, `failed` when at least
one step failed, and `cancelled` when an operator cancelled it.

### Failure semantics

- `on_failure: stop` (default): the first failed step stops the run. Every step
  that has not started becomes `skipped`, and the run is `failed`.
- `on_failure: continue`: the independent branches keep running; the dependents
  of the failed step are skipped. The run is still `failed` (a failure is never
  hidden), but the other outputs are kept.

`retries: N` gives a step `N` extra attempts. The attempt counter is stored on
the step row; the last error is kept. Retries are immediate (no backoff) and do
not apply to cancellation.

### Prompt placeholders

Before a step starts, its prompt is rendered with:

| Placeholder | Value |
|---|---|
| `{{steps.<id>.output}}` | stdout of a dependency (truncated to 64 KiB) |
| `{{context}}` | the run context as compact JSON (`""` when absent) |
| `{{run.id}}` | UUID of the current run |
| `{{pipeline.name}}` | pipeline name |
| `{{step.id}}` | id of the current step |

An unknown placeholder is left verbatim, so a typo stays visible in the prompt
instead of silently becoming an empty string.

The prompt is executed inside the step instance container through the driver:
`docker compose … exec -T agent <command>`, where `<command>` is
`FLEETT_RUN_COMMAND` (default `gateway ask`) followed by the rendered prompt as
the last argument. The prefix is split on whitespace and never passed through a
shell. A non-zero exit code or a timeout fails the attempt; stdout is the step
output, stderr is the error detail.

### Cancellation

`POST /api/pipelines/{id}/runs/{run}/cancel` marks the run `cancelled` and marks
its pending steps `skipped` immediately. The engine checks the flag between
launches: a step already running is allowed to finish (its output is kept),
then the run is finalized `cancelled`. Cancelling a finished run answers
`409 run_finished`.

## Run history

Each run keeps its pipeline id, trigger (`manual` for an API run), context,
timestamps, final error and one row per step with its status, attempt counter,
output and error. Deleting a pipeline deletes its history; nothing else ever
removes a run.

## API

All mutation endpoints require the admin role; reading requires any
authenticated role.

| Method | Route | Body | Response |
|---|---|---|---|
| GET | `/api/pipelines` | — | `PipelineView[]` |
| POST | `/api/pipelines` | definition (+ `enabled`) | `201 PipelineView` |
| GET | `/api/pipelines/{id}` | — | `PipelineView` |
| PATCH | `/api/pipelines/{id}` | partial definition / `enabled` | `PipelineView` |
| DELETE | `/api/pipelines/{id}` | — | `204` |
| POST | `/api/pipelines/{id}/run` | `{"context":{…}}` (optional) | `202 PipelineRunDetail` |
| GET | `/api/pipelines/{id}/runs?limit=50` | — | `PipelineRun[]`, newest first |
| GET | `/api/pipelines/{id}/runs/{run}` | — | `PipelineRunDetail` |
| POST | `/api/pipelines/{id}/runs/{run}/cancel` | — | `PipelineRunDetail` |

`PipelineView` flattens the definition and adds `id`, `enabled`, `created_at`
and `updated_at`. `PipelineRunDetail` is the run plus its `steps` array.

```bash
# Create from JSON
curl -sS -X POST "$FLEETT_URL/api/pipelines" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"release","steps":[{"id":"produce","instance":"alice","prompt":"draft"}]}'

# Run it, then read the report
curl -sS -X POST "$FLEETT_URL/api/pipelines/<id>/run" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -d '{"context":{"version":"0.6.0"}}'
curl -sS "$FLEETT_URL/api/pipelines/<id>/runs/<run>" \
  -H "Authorization: Bearer $FLEETT_TOKEN"
```

A disabled pipeline answers `409 pipeline_disabled` when started. Validation
errors are `400` with codes: `invalid_name`, `no_steps`, `too_many_steps`,
`duplicate_step_id`, `invalid_step_id`, `unknown_dependency`, `cycle_detected`,
`empty_prompt`, `prompt_too_large`, `empty_instance`, `invalid_retries`,
`invalid_timeout`, `invalid_trigger`, `invalid_cron`, `unknown_instance`.
Duplicate names answer `409 pipeline_exists`.

Every mutation is audited: `pipeline.create`, `pipeline.update`,
`pipeline.delete`, `pipeline.run`, `pipeline.cancel`.

## Cron and webhook triggers

`trigger: cron` stores the schedule and exposes it in `GET /api/pipelines`. This
release deliberately does **not** start a scheduler loop: issue #30 owns it.
When the scheduler exists it calls `pipelines::run_due(db, now)`, which returns
the enabled cron pipelines whose expression matches the minute and which have
not already started a run in that minute. `trigger: webhook` is metadata for
now: wire an external caller to `POST /api/pipelines/{id}/run`.

## CLI

```bash
fleett pipelines
fleett pipeline get <id>
fleett pipeline run <id>            # 202, prints the run id
fleett pipeline run <id> --wait     # follows the run, exit 1 when it failed
fleett pipeline runs <id> --limit 20
fleett pipeline apply -f release.yaml
fleett pipeline rm <id>
```

`pipeline apply` looks the pipeline up by `name`: it creates it when absent and
replaces it otherwise. `pipeline run --wait` prints the final step table and
exits `1` when the run is `failed` or `cancelled`. Every command accepts
`--json`.

## Limits

- 50 steps per pipeline; 4 steps running at once.
- 16 KiB per prompt; 64 KiB of output kept per step (the rest is truncated).
- 5 retries and 1 h of timeout per attempt.
- 500 runs per history page.
- No cross-run state: use the run context or a share for data that outlives a
  single run.
