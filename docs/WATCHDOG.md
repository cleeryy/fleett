# Watchdog — self-healing and incidents

`restart: unless-stopped` restarts a container that dies, but it says nothing
about an agent that is **alive and mute**: a gateway blocked on its socket, a
health check that has been `unhealthy` for ten minutes, a crash loop, an image
that vanished from the host. The watchdog samples every instance, keeps a
registry of what it saw, and — only when the instance opted in — applies a
bounded remediation.

Everything here is opt-in. An upgraded database starts with `heal = 'off'` on
every instance: the control plane records incidents but never restarts a
container until an operator sets a policy.

## Sampling

`fleett serve` starts one background task. Every `FLEETT_WATCHDOG_INTERVAL_S`
seconds (default **30**, `0` disables the loop) it reads, for every instance:

| Signal | Source |
|---|---|
| lifecycle status and health | `docker compose ps` (`driver.ps`) |
| `OOMKilled`, `ExitCode`, `Error`, `RestartCount` | `docker inspect` (`driver.inspect`) |
| local image store (only when a container is dead) | `docker images` |

The loop takes the per-instance lifecycle lock (`AppState::locks`) before
acting, so a heal never races an operator's `start`, `stop` or `restart`, and it
re-reads the instance row inside the lock so a policy change applies on the next
pass.

The sampler never acts on a blind read: if `docker compose ps` fails, the tick
is skipped for that instance.

## Policy per instance

```json
PATCH /api/instances/alice/heal
{"heal": "restart", "unhealthy_after": 3, "max_restarts": 5, "heal_backoff_s": 30}
```

| Field | Values | Default | Meaning |
|---|---|---|---|
| `heal` | `off`, `restart`, `recreate` | `off` | `off` observes only; `restart` runs `docker compose restart`; `recreate` runs `down` then `up -d`. |
| `unhealthy_after` | 1…100 | `3` | Consecutive `unhealthy` samples before an incident is opened and, with healing on, before the first action. |
| `max_restarts` | 0…100 | `5` | Heal attempts allowed per incident. `0` records incidents without ever touching the container. |
| `heal_backoff_s` | 1…3600 | `30` | Base delay between two attempts. Doubled after each attempt, capped at 30 minutes. |

A policy change resets the current streak, the attempt budget and the backoff of
that instance; the change is written to the audit log (`instance.heal`, with the
before/after values).

```bash
fleett heal alice --policy restart --unhealthy-after 2 --max-restarts 3 --backoff 15
fleett heal alice --policy off          # back to observe-only
```

## Incident lifecycle

One incident per `(instance, cause)` while it is open — a second unhealthy
sample refreshes the existing row (`last_seen_at`, `detail`) instead of opening
another one.

| Cause | Opened when |
|---|---|
| `unhealthy` | `unhealthy_after` consecutive samples report health `unhealthy`. |
| `oom` | `docker inspect` says the container was OOM-killed. Wins over `crashloop`. |
| `crashloop` | The container is not running and `docker inspect` shows a failure (non-zero exit, Docker `Error`, state `dead`/`restarting`) and it was not an OOM kill. |
| `image_missing` | The container is dead **and** its image is absent from the local store. Healing is exhausted immediately: a restart cannot pull an image. |
| `heal_failed` | A `restart`/`recreate` action returned an error. |

A **clean stop** (`stopped`/`created`, exit code 0, no Docker error, no OOM) opens
no incident and is never healed. An incident opened *before* the stop stays
open: the operator still has to see why the instance was unhealthy.

An incident closes **automatically** on the first sample that reports the
instance running and not unhealthy (`healthy`, or `none` when the container has
no health check). `POST /api/incidents/{id}/close` closes it by hand; that also
resets the heal budget so a fresh incident can be attempted.

```bash
fleett incidents                       # open and closed, newest first
fleett incidents --status open
fleett incidents --instance alice
fleett incident close 3f2a…
```

## Healing

Healing only happens for `heal = restart|recreate`. The order of a pass is:

1. open or refresh the incident;
2. if the attempt budget is exhausted (or `max_restarts = 0`, or the crash count
   already reached `max_restarts`, or the cause is `image_missing`), mark the
   instance `error` with a `last_error` explaining that healing stopped, and
   stop — the incident stays open;
3. if the backoff has not elapsed, do nothing this pass;
4. run the action, append it to the incident's `actions` (with `result` `ok` or
   `error`), increment the attempt counter and reset the unhealthy streak;
5. after `max_restarts` attempts, mark the instance `error`; the incident stays
   open until the instance is healthy again or an operator closes it.

Backoff for attempt *n* is `min(heal_backoff_s × 2^(n−1), 1800 s)`. With the
defaults (30 s, 5 attempts) the delays are 30 s, 60 s, 120 s, 240 s, 480 s.

A `crashloop` whose observed `RestartCount` already reached `max_restarts` is
never healed: a container that Docker restarted five times without staying up is
a real failure, and restarting it again would only hide it.

## API

| Method | Route | Meaning |
|---|---|---|
| GET | `/api/incidents?status=open\|closed&instance=&limit=` | page of incidents, newest first (limit 1…500, default 100) |
| GET | `/api/instances/{id}/incidents?limit=` | every incident of one instance |
| POST | `/api/incidents/{id}/close` | manual close (admin) — `409 incident_closed` when already closed |
| PATCH | `/api/instances/{id}/heal` | policy change (admin) |

An incident is returned as:

```json
{
  "id": "6f1c…",
  "instance_id": "alice",
  "cause": "unhealthy",
  "status": "open",
  "opened_at": "2026-11-09T10:00:00Z",
  "detail": "container health check failed for 3 consecutive sample(s) (threshold 3)",
  "actions": [
    {"at": "2026-11-09T10:00:30Z", "action": "restart", "result": "ok"}
  ],
  "restart_count": 0,
  "last_seen_at": "2026-11-09T10:00:30Z"
}
```

`closed_at` is present once the incident is closed. `actions` is the audit trail
of the attempt(s), not a promise that the container recovered.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_WATCHDOG_INTERVAL_S` | `30` | Sampling interval, in seconds. `0` disables the background loop; the registry and the API stay available. Values above 86400 are clamped. |

The watchdog shares the control plane's Docker access: run it in the same
process (the default) or not at all. There is no separate timer to install.

## Limits and non-goals

- The sampler reads the Docker health check; it does not probe the gateway
  socket itself. An image without `HEALTHCHECK` reports `none`, so only a crash
  or a missing image can be detected.
- `OOMKilled` is only visible for the *last* exit: an OOM kill followed by an
  automatic restart may be missed by the next sample.
- Watchdog streaks and backoff live in memory. Incidents are persisted, and the
  observed `RestartCount` keeps bounding a crash loop across a control-plane
  restart, but the exact backoff phase restarts from zero.
- Deleting an instance deletes its incidents; the registry is not a
  fleet-wide long-term log (the audit trail keeps the operator actions).
