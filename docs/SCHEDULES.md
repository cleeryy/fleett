# Schedules

Every agent can hold its own cron schedules. A schedule sends a prompt to the
instance gateway at fixed times, records the output, the exit code and the
duration of every attempt, and exposes the history through the API and the CLI.
This page documents the semantics; the routes are listed in [API.md](../API.md)
and the commands in [CLI.md](CLI.md).

## Model

One schedule is `{name, cron, prompt, enabled, timezone, overlap, timeout,
catchup}` on one instance. `name` is unique per instance (`409 schedule_exists`
otherwise), so the same name may be used on two agents.

| Field | Default | Meaning |
|---|---|---|
| `name` | required | Operator-facing label, ≤ 64 characters, unique per instance |
| `cron` | required | Five-field expression, UTC |
| `prompt` | required | Prompt appended to the run command, ≤ 16 KiB |
| `enabled` | `true` | A disabled schedule is never selected by the tick |
| `timezone` | `UTC` | Accepted and stored; **evaluation is UTC for now** |
| `overlap` | `skip` | `skip` or `queue` when the previous run is still going |
| `timeout` | `300` | Execution budget in seconds, `1..=3600` |
| `catchup` | `false` | Replay one missed occurrence after a restart |

The response uses `timeout` too; the database column is `timeout_s`.

## Cron semantics

The parser accepts exactly five fields:

```
minute hour day-of-month month day-of-week
0-59   0-23 1-31         1-12  0-7 (0 and 7 are Sunday)
```

Supported syntax in each field: `*`, `*/n`, lists (`1,15,30`), ranges (`9-17`)
and stepped ranges (`9-17/2`). A single value with a step (`5/10`) means "from 5
to the field maximum, every 10". Names (`MON`, `JAN`) and `@daily` macros are
refused.

Day-of-month and day-of-week follow Vixie cron: when **both** are restricted, a
day matching either field fires. For example `0 0 1 * 1` runs on the first of
the month *and* every Monday. When one of them is `*`, only the other one
restricts.

`next_run_at` is computed from the current time when the schedule is created, or
when its cron expression or enabled flag changes. An expression that can never
fire (`0 0 31 2 *`) is refused with `400 invalid_cron`.

The `timezone` field is a forward-compatible placeholder: it is validated
(non-empty, ≤ 64 characters, no control characters) and returned, but every
evaluation and timestamp is UTC. Do not rely on it to shift a schedule yet.

## Execution

The server starts a background loop in `serve` only (never in tests). Every
15 seconds it selects the enabled schedules whose `next_run_at` is due and runs
them **sequentially**, one at a time.

The command is built from `FLEETT_RUN_COMMAND`, a whitespace-separated argv
prefix defaulting to `gateway ask`; the prompt is always the final argument. It
runs inside the agent container with:

```
docker compose -f instances/<id>/compose.yaml -p <project> exec -T agent <argv...>
```

`FLEETT_RUN_COMMAND` is read at run time on the control-plane host, so changing
it takes effect on the next run without a restart. There is no shell: every
word is one argv element, and the prompt is never interpolated.

Before the command starts, the schedule stores `last_run_at` and advances
`next_run_at` by one occurrence, and a `running` row is inserted. A non-zero
exit code is a recorded result, not a driver failure:

| Status | Meaning |
|---|---|
| `ok` | The command exited with code 0 |
| `error` | Non-zero exit code, a Docker/compose failure, or an unknown instance |
| `timeout` | The command was killed after `timeout` seconds |
| `skipped` | The overlap policy refused this occurrence |
| `running` | The command is in flight (a row left by a crash is recovered at startup) |

Output (stdout, then `[stderr]` and the stderr text) is capped at 64 KiB; the
history stores the truncated value. A stopped container or a missing image
therefore produces a visible `error` run with the Docker detail rather than a
silent failure.

### Overlap

- `skip` (default): when the previous run of the same schedule is still
  `running`, the occurrence is recorded as `skipped` and the schedule advances.
- `queue`: the occurrence stays due and starts on the first tick after the
  running command finishes. Missed occurrences collapse into one run; they are
  not replayed as a burst.

Overlap is per schedule. Two schedules of the same instance may run at the same
time, and disabling one schedule never affects another instance.

### Catch-up and restart recovery

On startup, before the first tick:

1. Every `running` row is turned into `error` with the note
   `interrupted by a control-plane restart`.
2. With `catchup: false` (default), an enabled schedule whose `next_run_at` is
   in the past is moved to its next future occurrence: a control plane that was
   down for a day does not fire 288 runs.
3. With `catchup: true`, the missed occurrence is left due, so exactly one
   catch-up run fires immediately (the next one is then the next future
   occurrence).

## Manual runs

`POST /api/schedules/{sid}/run` (`fleett schedule run <sid>`) executes the
schedule immediately, whatever its enabled flag, and returns the recorded run.
It answers `409 run_in_progress` while a run of that schedule is running. It
does not wait for `next_run_at` and does not enable a disabled schedule.

## Limits and retention

- Prompt: 16 KiB. Name: 64 characters. `timeout`: `1..=3600` seconds.
- Run history is kept indefinitely; the API pages it (`?limit=`, `1..=500`,
  default 50) and `GET /api/schedules` reports `runs_total`, `runs_failed` and
  `failure_rate` (failures are `error` and `timeout`; `skipped` and `running`
  are not counted).
- Deleting an instance deletes its schedules and run history. Deleting a
  schedule deletes its history.
- The global view is admin-only, like the global message view; per-instance
  reads are available to every authenticated role, and mutations follow the
  usual `viewer` restriction.

## Audit

`schedule.create`, `schedule.update`, `schedule.delete` and `schedule.run` are
appended to the audit log with the instance, the name and the cron expression
(prompts are not copied into the audit trail).

## API sketch

```bash
# Create a schedule that runs every five minutes.
fleett schedule add alice --name every-five --cron '*/5 * * * *' \
  --prompt 'summarize the incidents since the last run' --timeout 120

# Inspect it, force one run, read the history.
fleett schedules alice
fleett schedule run <schedule-id>
fleett runs alice --limit 20
fleett --json runs alice | jq '.[] | select(.status != "ok")'
```

```json
POST /api/instances/alice/schedules
{"name":"every-five","cron":"*/5 * * * *","prompt":"summarize",
 "enabled":true,"timezone":"UTC","overlap":"skip","timeout":300,"catchup":false}
```

```json
GET /api/instances/alice/runs?limit=2
[{"id":"…","schedule_id":"…","instance_id":"alice",
  "started_at":"2026-11-09T10:05:00Z","finished_at":"2026-11-09T10:05:14Z",
  "status":"error","exit_code":2,"output":"gateway: provider timeout",
  "duration_ms":14200,"created_at":"2026-11-09T10:05:00Z"}]
```
