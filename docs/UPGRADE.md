# Rolling image upgrades

`fleett upgrade` moves every agent of the fleet to another container image **one instance at a
time** and puts an instance back on its previous tag when the new one never becomes healthy.
The operation is driven by the control plane: nothing has to be done container by container.

Related: `docs/CLI.md` (client commands), `API.md` (HTTP contract), `docs/DEPLOY.md`
(deployment), `docs/BACKUP.md` (take a backup before a big jump),
`docs/DEPLOYMENTS.md` (maintenance windows, canary phase, scheduled runs and the deployment
journal).

## What the job does

Per instance, in order:

1. **make sure the image is local** — `docker image inspect <tag>`, then `docker pull <tag>`
   when the tag is absent. A pull failure answers `502 image_unavailable` before the job
   starts (the same error is retried per instance if the registry disappears mid-job).
2. **record `from`** — the image currently declared by the compose file.
3. **write the new image** — `services.agent.image` becomes the new tag and the old tag is
   kept as the `fleett.previous_image` label of the compose file.
4. **`docker compose up -d`** — the container is recreated with the new image; the data
   directory (`/opt/data`) is a bind mount and survives untouched.
5. **wait for `healthy`** — `docker compose ps` is polled until the container healthcheck
   answers (the generated probe asks the Hermes gateway to identify itself over
   `/opt/data/gateway.sock`). An `unhealthy` status fails immediately, a container that stays
   `starting`/`none` fails at `timeout_s`.
6. **roll back on failure** — the previous tag is written back, `up -d` runs again, and the
   instance ends `rolled_back` with
   `last_error = "upgrade failed: <reason>; rolled back to <old>"`.

A healthy instance ends `healthy`, its database image is updated, `last_error` is cleared and
the compose file keeps the label:

```yaml
services:
  agent:
    image: hermes-agent:0.23.0-local
    labels:
      fleett.previous_image: hermes-agent:0.22.2-local
```

The label is the reproducibility record of the last **successful** upgrade. It is rewritten by
the next successful upgrade and left alone by a rollback (the tag before the failed attempt is
kept). The compose file is the source of truth: the database image column follows
`services.agent.image` at startup, so a hand edit or a restored backup is picked up.

## Before upgrading

1. **Build or pull the new image** where the Docker daemon runs. `fleett` pulls it for you,
   but the tag must exist in a registry the host can reach, or already be loaded locally
   (for example with `docker load`).
2. **Check what is running**:

   ```bash
   fleett images
   fleett --json images | jq '.[] | select(.instances | length > 0)'
   ```

   `INSTANCES` tells which agents use each tag; a tag with no instance is safe to prune.
3. **Take a backup** when the jump is big or the data matters (see `docs/BACKUP.md`):

   ```bash
   sudo fleett backup --keep 3
   ```

## Procedure

```bash
# 1. inspect the plan; nothing is pulled, written or restarted
fleett upgrade --image hermes-agent:0.23.0-local --dry-run

# 2. upgrade sequentially (the default), 180 s of health timeout per instance
fleett upgrade --image hermes-agent:0.23.0-local

# 3. two instances at once on a large fleet, with a longer timeout
fleett upgrade --image hermes-agent:0.23.0-local --parallel 2 --timeout 300

# 4. one instance first, watched for 10 minutes, then the rest
fleett upgrade --image hermes-agent:0.23.0-local --canary 1 --soak 10m
```

When at least one maintenance window is enabled, the same command is refused outside the
window with `403 maintenance_window` and the next opening (`docs/DEPLOYMENTS.md`). With
`--canary N`, the first N instances are moved first and must stay healthy for `--soak`
seconds; a canary that fails rolls back on its own and stops the deployment, leaving the
other instances on their current tag. Every run — immediate or scheduled with `--at` — is
recorded in the deployment journal (`fleett deployments`).

The command polls the job every 2 s, prints one line per state change, then the final table:

```
ID     FROM                            TO                              STATUS
alice  hermes-agent:0.22.2-local       hermes-agent:0.23.0-local       healthy
bob    hermes-agent:0.22.2-local       hermes-agent:0.23.0-local       rolled_back
  bob: upgrade failed: the container reported unhealthy; rolled back to hermes-agent:0.22.2-local
```

Exit code `1` when at least one instance ended `rolled_back` or `failed`; the table is printed
either way. `--json` prints the final job only (no intermediate lines), so a script can run:

```bash
fleett --json upgrade --image "$TAG" | jq -r '.results[] | "\(.id)\t\(.status)"'
```

The same thing over HTTP (`API.md`):

```bash
curl -fsS -X POST "$FLEETT_URL/api/upgrade" \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"image":"hermes-agent:0.23.0-local"}' | jq
curl -fsS "$FLEETT_URL/api/upgrade" -H "Authorization: Bearer $FLEETT_TOKEN" | jq '.results'
```

## Dry run

`--dry_run` (API) / `--dry-run` (CLI) returns a finished job whose results are all `pending`,
with `from` and `to` filled in. It **never** pulls an image, writes a compose file, restarts a
container or appends to the audit trail. Use it to check the instance list, the current tags
and the target tag before a real run.

## Rollback semantics

| Outcome | Instance ends | `last_error` | Compose file |
|---|---|---|---|
| healthy | `healthy` (status `running`) | cleared | new tag + `fleett.previous_image: <old>` |
| unhealthy / timeout | `rolled_back` | `upgrade failed: <reason>; rolled back to <old>` | old tag, previous label unchanged |
| pull or `up` failure, instance untouched | `failed` | the Docker detail | unchanged |
| rollback itself fails | result `failed`, instance `error` | `upgrade failed: <reason>; the rollback failed: <detail>` | old tag (rewritten, not started) |

`rolled_back` means the agent is running again on its previous tag; `failed` means it is not
in the state the job wanted and needs an operator. Both are audited:
`instance.upgrade` for a successful move, `instance.upgrade.rollback` for a restore (the audit
entry carries `from`, `to`, `status` and the reason).

The job is in memory: restarting the control plane loses the last job but never changes a
container. A job that was running when the control plane stopped is simply gone — run
`fleett ps` and re-run the upgrade; instances already on the new tag are upgraded again (a
no-op `up -d` on the same image) and stay healthy. The deployment journal
(`GET /api/deployments`) keeps the durable trace of the run; scheduled deployments survive a
restart and fire on the next tick once due.

## Parallelism

Sequential is the default because each instance is briefly unavailable while its container is
recreated (restarting a Hermes gateway drops its in-flight messages). `--parallel N` (max 16)
upgrades N instances at once; failures are still isolated per instance. The per-instance health
timeout (`--timeout`, default 180 s, max 3600 s) starts after `up -d`.

Poll interval is derived from the timeout: `timeout/20`, bounded to `100 ms .. 2 s`, so a
short timeout still probes several times.

## Verifying and rolling back by hand

```bash
fleett ls                                  # status + health of every agent
fleett --json images | jq '.[] | {tag, instances}'
fleett logs alice --tail 50

# manual rollback of one instance: rewrite the tag and restart
$EDITOR ~/.fleett/instances/alice/compose.yaml   # image: hermes-agent:0.22.2-local
fleett restart alice                             # `up -d` is what recreates the container
```

A hand edit of `compose.yaml` is picked up by the index at the next control-plane start (and by
the next `PATCH`), so the database keeps agreeing with the file. Prefer
`fleett upgrade --image <old>` for a fleet-wide rollback: it applies the same health checks and
records the label.

## Troubleshooting

- **`502 image_unavailable`** — the registry is unreachable, the tag does not exist, or the
  host is not logged in (`docker login`). The message carries the Docker stderr.
- **Everything rolls back** — check the image itself: `docker run --rm <tag> gateway --version`
  or the container logs (`fleett logs <id>`). A missing health probe, a broken gateway socket
  or a crash loop all show as `unhealthy`.
- **`409 upgrade_in_progress`** — a job is still running (`GET /api/upgrade` shows it). Wait,
  or restart the control plane to clear a job left behind by a crash.
- **`403 forbidden`** — the token is a `viewer`; upgrading is an admin mutation.
- **`403 maintenance_window`** — an enabled maintenance window is closed at the moment; the
  message and `error.details.next_open` carry the next opening (`fleett windows`,
  `docs/DEPLOYMENTS.md`). Nothing was pulled or restarted.
- **Health never turns healthy** — the healthcheck has a 60 s `start_period` and 3 retries at
  30 s; a heavy first start can exceed the default 180 s timeout. Re-run with `--timeout 300`.

## Tests

The suite covers the whole path without Docker: the mock driver tracks the image of each
simulated container, can declare a tag unhealthy and can delay `up` (parallelism). The
integration tests assert the healthy move (image + label), the rollback and its `last_error`,
the dry run writing nothing, the parallel limit, the `409` of a concurrent job, the audit rows
and the `403` of a viewer. See `backend/tests/upgrade.rs`. The windows, canary/soak phase,
scheduled deployments and the journal are covered by `backend/tests/deployments.rs` (see
`docs/DEPLOYMENTS.md`).
