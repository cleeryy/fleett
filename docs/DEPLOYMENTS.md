# Maintenance windows, canary upgrades and the deployment journal

`fleett upgrade` has three safety layers on top of the rolling upgrade of
[docs/UPGRADE.md](UPGRADE.md):

- **maintenance windows** — destructive operations are refused outside the
  hours the operator declared;
- **canary + soak** — one (or N) instance moves first and must stay healthy
  before the rest of the fleet follows;
- **deployment journal** — every run, immediate or scheduled, is a durable row
  of `GET /api/deployments`, with the per-instance outcome and the rollback
  flag.

Related: `API.md` (HTTP contract), `docs/CLI.md` (client commands),
`docs/UPGRADE.md` (single-image rollback semantics).

## Maintenance windows

A window is a recurring UTC day/time range. It is stored in `fleet.db`
(`maintenance_windows`) and managed over the API or with `fleett windows`:

```json
{
  "id": "6a1c…",
  "name": "nightly",
  "days": ["mon", "tue", "wed", "thu", "fri"],
  "start_minute": 120,
  "end_minute": 300,
  "enabled": true,
  "created_at": "2026-11-09T03:00:00Z",
  "updated_at": "2026-11-09T03:00:00Z"
}
```

| Field | Meaning |
|---|---|
| `days` | days the window **starts** on, `mon`..`sun` (full names and `tues`/`thurs` are accepted and normalised) |
| `start_minute` | UTC minute since midnight, inclusive (`0..=1439`) |
| `end_minute` | UTC minute since midnight, exclusive (`0..=1439`) |
| `enabled` | a disabled window never opens |

Rules:

- **Day boundaries are UTC.** Put the time you mean in UTC, not in the server
  timezone.
- **Overnight windows** have `end_minute < start_minute` and belong to the day
  they start: `sat 22:00 → 02:00` is `{"days":["sat"],"start_minute":1320,
  "end_minute":120}` and is open on Saturday 23:30 and Sunday 01:00, not on
  Sunday 23:30.
- **Full-day windows** have `start_minute == end_minute` (`0` / `0`). Useful for
  "all weekend allowed" style policies.
- Two windows may overlap; being inside any enabled window is enough.

### Enforcement

The gate is checked **before anything is pulled, written or stopped**:

| Operation | Refused outside a window |
|---|---|
| `POST /api/upgrade` | yes |
| `DELETE /api/instances/{id}` | yes |

Everything else (start/stop/restart, config patches, logs, reads) stays
available: it is what an operator needs to diagnose a fleet at 3 a.m.

The refusal is:

```json
{"error":{"code":"maintenance_window",
          "message":"no maintenance window is open; the next opening is 2026-11-10T02:00:00Z",
          "details":{"next_open":"2026-11-10T02:00:00Z"}}}
```

`details.next_open` is `null` only when no window can ever open (all disabled,
or the named window disappeared).

Enforcement is **off while no enabled window exists**, so a fresh install is
never locked out. `FLEETT_MAINTENANCE_ENFORCE=0` (also `false`, `no`, `off`)
disables it entirely, whatever the windows. `GET
/api/maintenance-windows/status` reports the live state:

```bash
curl -fsS "$FLEETT_URL/api/maintenance-windows/status" \
  -H "Authorization: Bearer $FLEETT_TOKEN" | jq
# {"open": false, "next_open": "2026-11-10T02:00:00Z", "enforce": true}
```

`open` is `true` when a destructive operation would be allowed right now (no
enforcement, no enabled window, or one is open).

### Managing windows

```bash
# nightly maintenance, 02:00–05:00 UTC, Monday to Friday
fleett windows add --name nightly --days mon,tue,wed,thu,fri --start 02:00 --end 05:00

# whole weekend
fleett windows add --name weekend --days sat,sun --start 00:00 --end 00:00

fleett windows                    # list
fleett windows list --json | jq   # machine-readable
fleett windows rm 6a1c…           # delete
```

Over HTTP the same thing (admin role required for mutations):

```bash
curl -fsS -X POST "$FLEETT_URL/api/maintenance-windows" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"nightly","days":["mon","tue","wed","thu","fri"],
       "start_minute":120,"end_minute":300}' | jq

curl -fsS -X PATCH "$FLEETT_URL/api/maintenance-windows/<id>" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"enabled":false}' | jq
```

`days` accepts the JSON array or the `"mon,tue"` string form. `PATCH` changes
only the fields present.

### Pinning one window to an upgrade

`POST /api/upgrade` accepts `"window": "<id or name>"`: the run is then allowed
only while **that** window is open (`403 maintenance_window` with its next
opening otherwise), even if another window is open at the same time. An unknown
or disabled window is refused with `400 invalid_window`. The CLI intentionally
does not expose it: the fleet-wide policy is the usual lever.

## Canary upgrades and the soak

```json
{"image":"hermes-agent:0.23.0-local","canary":1,"soak_s":600,"timeout_s":180}
```

| Field | Default | Meaning |
|---|---|---|
| `canary` | `0` | number of instances moved first. Capped at the fleet size; `0` keeps the plain rolling upgrade |
| `soak_s` | `600` | seconds the canary instances must stay healthy before the rest moves (`0` skips the soak, max `3600`) |

The canary instances are the first `canary` ids in `GET /api/instances` order
(sorted by id). The phases:

1. **pending** — the journal row exists, the image is pulled once.
2. **canary** — the canary instances are upgraded one at a time with the normal
   per-instance health wait and rollback logic. If one does not become healthy,
   it is rolled back and the deployment **stops**: the remaining instances are
   left untouched (`pending`, with `error: "not attempted: the canary phase
   failed (…)"`).
3. **soak** — every 10 s (capped by `soak_s` for a short soak) the control
   plane probes each healthy canary. A canary that reports `unhealthy`, or
   whose container is `stopped`/`error`, fails the soak. The healthy canaries
   are then **rolled back to their previous image** — compose file, `up -d`
   and `last_error` included — and the rest of the fleet is left untouched.
4. **running** — the remainder is upgraded with the existing sequential /
   `parallel` logic (canary instances are not upgraded twice).
5. **ok / rolled_back / failed** — the job and the journal converge.

A failed canary is a `rolled_back` deployment (`rollback: true`) unless one of
the instances could not even be restored, which makes it `failed`.

The job finishes as soon as the canary fails: there is no second pull, no
restart of the untouched instances, and `GET /api/upgrade` reports
`finished_at` with the reason on every skipped result.

```bash
fleett upgrade --image hermes-agent:0.23.0-local --canary 1 --soak 10m
fleett upgrade --image hermes-agent:0.23.0-local --canary 2 --soak 300 --parallel 2
fleett --json upgrade --image "$TAG" --canary 1 --soak 1m | jq '.deployment_id'
```

`--soak` accepts seconds (`600`), `30s`, `10m` or `1h`. The server validates
`0..=3600` (`400 invalid_soak`).

## Scheduled deployments

```bash
fleett upgrade --image hermes-agent:0.23.0-local --canary 1 --soak 10m --at "sun 04:00"
fleett upgrade --image hermes-agent:0.23.0-local --at 2026-11-15T04:00:00Z
```

`at` accepts:

- an RFC 3339 timestamp with any offset (`2026-11-15T05:00:00+01:00`), converted
  to UTC;
- a `<day> HH:MM` expression, always UTC (`sun 04:00`, `Sunday 4:00`). The
  **next** occurrence strictly in the future is used, so `mon 04:00` typed on a
  Monday at 05:00 schedules the following Monday. Days accept `mon`..`sun` and
  full names.

A past RFC 3339 timestamp is refused with `400 invalid_at`; `at` combined with
`dry_run` is refused too (a dry run has nothing to defer).

The response is `202` with the **journal row**, not an `UpgradeJob`:

```json
{"id":"0b7e…","image":"hermes-agent:0.23.0-local","status":"scheduled",
 "instances":["alice","bob"],"canary":1,"soak_s":600,
 "started_at":"2026-11-15T04:00:00Z","finished_at":null,
 "result":{"request":{"image":"hermes-agent:0.23.0-local","canary":1,"soak_s":600,
                      "timeout_s":180}},
 "error":null,"rollback":false,"created_by":"cleeryy"}
```

`started_at` carries the **planned** time while the row is `scheduled`; it is
rewritten with the real start when the server fires the job. The deferred
request is kept under `result.request` so the row is self-describing
(`parallel`, `timeout_s` and `window` survive the wait).

The server ticks every 30 s (`maintenance::TICK_INTERVAL`):

- scheduling is allowed outside a window — the gate is applied when the
  deployment **fires**, not when it is accepted, so "next Sunday 04:00" can be
  booked from a Tuesday afternoon;
- a due row while an upgrade is already running stays `scheduled` and is
  retried at the next tick;
- the **maintenance window is evaluated at firing time**, not when the request
  was accepted. A window that closed in between changes nothing on Docker: the
  row becomes `cancelled` with the reason and the next opening in `error`;
- a failed pull or an invalid deferred request becomes `failed` with the error;
- the fired job reuses the row id (`GET /api/upgrade` → `id` == journal `id`,
  also exposed as `deployment_id`).

**Restart semantics:** the due list lives in SQLite, so restarting the control
plane does not lose a scheduled deployment. What a restart loses is an
in-memory *running* job (as before): a row claimed at the exact moment of the
crash stays `pending`. Check `fleett deployments` after a restart and re-run
the upgrade if needed.

## Deployment journal

`GET /api/deployments?limit=50&status=ok` (newest first, `limit` `1..=500`) and
`GET /api/deployments/{id}` (with the per-instance `result`).

| Column | Meaning |
|---|---|
| `id` | deployment id, also the job id of the run |
| `image` | target tag |
| `status` | `pending`, `canary`, `running`, `scheduled`, `ok`, `failed`, `rolled_back`, `cancelled` |
| `instances` | ids captured when the deployment was accepted |
| `canary`, `soak_s` | the canary policy of the run |
| `started_at` | planned time (`scheduled`) or actual start |
| `finished_at` | end of the run, `null` while it runs |
| `result` | per-instance array (same shape as `UpgradeJob.results`), or `{"request":…}` while `scheduled` |
| `error` | why the deployment never ran (`failed`, `cancelled`) |
| `rollback` | `true` when at least one instance was restored |
| `created_by` | actor of the request (named token or session email); `scheduler` for the tick |

```bash
fleett deployments
fleett deployments --limit 20 --status rolled_back
fleett deployment get 0b7e…
fleett --json deployment get 0b7e… | jq '.result[] | select(.status != "healthy")'
```

The journal is never pruned automatically: one row per upgrade is tiny, and
"what was deployed, when, and did it roll back" is exactly the history an
operator needs. Delete rows with SQL if a retention policy is required.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_MAINTENANCE_ENFORCE` | on | `0`/`false`/`no`/`off` disables the windows; any other value keeps enforcement. Enforcement only bites once an enabled window exists. |

## Tests

`backend/tests/deployments.rs` covers the window CRUD and validation, the
`403 maintenance_window` with `details.next_open` before any pull, the disabled
enforcement switch, the canary-first ordering, a failed canary leaving the rest
untouched, a canary degrading during the soak (rolled back alone), the soak
duration, the journal rows and filters, a scheduled deployment that is journaled
and fired by `maintenance::tick_scheduled`, and a due row cancelled because the
window was closed when the time came. Unit tests in
`backend/src/maintenance.rs` cover open/closed/overnight/full-day windows,
`next_open`, day/time normalisation and the `at` parser.
