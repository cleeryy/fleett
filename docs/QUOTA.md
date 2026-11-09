# Disk quotas and cleanup

Fleett bounds what one agent can write. Without a quota, a runaway agent is
enough to fill the host filesystem and take the whole fleet down with it
(issue #47).

Everything below is per instance, measured on `<root>/instances/<id>/data`,
and enforced by a loop that samples the fleet every 60 seconds.

## Model

| Column | Meaning |
|---|---|
| `disk_quota_mb` | Size bound of the data directory, in MB. `0` means unlimited and is the default, so instances created before this release never change behaviour. |
| `disk_policy` | What a breach at 100 % does: `warn`, `freeze` or `stop`. Default `warn`. |
| `disk_events` | Journal of quota crossings and cleanups (`id`, `instance_id`, `at`, `action`, `before_bytes`, `after_bytes`, `detail`). |

Both columns are added by migration v11; an existing database keeps every
instance unlimited until an operator sets a quota.

## Levels

The measured size is compared with `disk_quota_mb * 1 MiB`:

| Level | Threshold | Effect |
|---|---|---|
| `ok` | `< 80 %` | Nothing. |
| `warn` | `>= 80 %` | One `warn` event **per crossing**. Staying above 80 % never repeats it; dropping below and climbing again does. |
| `exceeded` | `>= 100 %` | The policy is applied **once per crossing**. |

The in-memory monitor (`quota::QuotaMonitor`) remembers the last level per
instance, so the loop never re-stops a container on every tick and never
restarts it. A control-plane restart re-evaluates the fleet once, which is
intentional: an operator upgrading the binary gets a fresh verdict.

## Policies

| Policy | At 100 % |
|---|---|
| `warn` | Alert only. The instance keeps running. |
| `freeze` | `docker compose stop`, status `stopped`, and `last_error` set to `disk quota exceeded: …`. The instance stays stopped until an operator frees space and starts it again. |
| `stop` | `docker compose stop` and status `stopped`. `last_error` is preserved. |

A stopped or already-stopped instance never produces a restart loop: the
crossing is recorded once. Raising or lowering the quota resets the monitor,
so the new threshold is evaluated on the next tick.

## Sample loop

`fleett serve` runs one tick at startup, then one every `FLEETT_QUOTA_INTERVAL_S`
seconds (default `60`; `0` disables the loop, the startup tick still runs).
The sample reuses the driver usage path: `docker stats` plus one `du -sb` per
data directory, so a tick costs about one `du` per instance.

The loop is exposed as `fleett::quota::tick(&AppState)`. Tests call it directly;
no background task is ever started in tests.

Quota crossings are written to `disk_events` and logged. This build has no
webhook or chat notifier: `fleett disk events` (or `GET /api/disk/events`) is
the notification surface, and a future notifier can consume the same journal.

## Host reserve on create

Before any file is written, `POST /api/instances` checks the free space of the
filesystem holding `FLEETT_ROOT`:

```bash
df -B1 --output=avail /opt/fleett
```

The create is refused with `409 insufficient_disk` when the remaining space is
below:

```text
max(FLEETT_MIN_FREE_DISK_MB, 2 * disk_quota_mb)
```

`FLEETT_MIN_FREE_DISK_MB` defaults to `2048` (2 GiB); `0` disables the floor
(the `2 * quota` rule still applies). `details.free_mb` and
`details.required_mb` tell the caller exactly what failed:

```json
{"error":{"code":"insufficient_disk","message":"…",
  "details":{"free_mb":1024,"required_mb":2048}}}
```

When `df` cannot be read, the free space counts as `0`: the conservative
choice. Set `FLEETT_MIN_FREE_DISK_MB=0` (and leave the quota at `0`) to make
creates independent of the host probe.

## Cleanup

```bash
fleett quota cleanup alice
curl -X POST -H "Authorization: Bearer $FLEETT_TOKEN" \
     http://127.0.0.1:8090/api/instances/alice/quota/cleanup
```

A cleanup removes, under the instance data directory only:

1. `tmp/` (recursively, then the directory itself);
2. `cache/` (same);
3. **rotated** logs older than 7 days: `*.log.1`, `*.log.2026-11-01`,
   `*.log.gz`, `*.log.zst`, anywhere under the data directory.

The *current* `*.log` file is deliberately kept: Docker or the agent may still
hold it open, and unlinking an open file frees no space on Linux until the
writer closes it. Symlinks are never followed.

The response reports `before_bytes`, `after_bytes`, `freed_bytes`,
`removed_files` and the relative `removed_paths`. A `clean` event is appended
to `disk_events` and to the audit log. If the cleanup brings the instance back
below 80 %, the next tick sees `ok`; if it is still above 100 %, the policy is
not re-applied until the next crossing (no stop loop after a partial cleanup).

### Docker logs

Docker's own `json-file` driver is already capped by the compose file fleett
renders:

```yaml
logging:
  driver: json-file
  options:
    max-size: 10m
    max-file: '3'
```

So a container holds at most ~30 MB of logs; they live in
`/var/lib/docker/containers/<id>/<id>-json.log`, not in the data directory, and
are not touched by a cleanup. `GET /api/instances/{id}/usage/detail` reports
the current `LogPath` and its size through `docker inspect`.

## Usage detail

`GET /api/instances/{id}/usage/detail` returns the data-directory breakdown
(`total`, `tmp`, `cache`, `logs`, `other`), the Docker log path and size, the
image tag and size, the quota view, and the last journal entry. It works with
the Docker daemon down: the data size then falls back to a filesystem walk.

## CLI

```bash
fleett quota alice                       # quota, usage, level, last event
fleett quota alice --set 2048            # quota only, policy kept
fleett quota alice --policy freeze       # policy only, quota kept
fleett quota alice --set 4096 --policy stop
fleett quota cleanup alice
fleett usage --detail
fleett disk events --instance alice --limit 20
```

All client commands accept `--json`.

## Tests

`backend/tests/quota.rs` covers the warn-once crossing, `freeze` and `stop`
against `MockDriver`, the `warn` policy at 100 %, cleanup freeing the data
directory, the host-reserve refusal (with the free bytes injected through
`quota::ensure_capacity` and the API), and the read/write routes with their
role checks.
