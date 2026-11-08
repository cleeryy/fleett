# Hardening the agent container

Every instance runs one container rendered from `backend/src/render.rs`. Its
security posture is chosen by `FLEETT_HARDENING` and applied to the next compose
file written (create, patch, upgrade, rollback):

| `FLEETT_HARDENING` | Root filesystem | Capabilities | `no-new-privileges` | `tmpfs` |
|---|---|---|---|---|
| `standard` (default) | writable | `cap_drop: [NET_RAW]` | yes | `/tmp` |
| `strict` | **`read_only: true`** | **`cap_drop: [ALL]`** | yes, exactly once (`security_opt` and the flag below) | `/tmp`, `/run`, `/var/tmp` |

Both modes always keep:

```yaml
security_opt:
  - no-new-privileges:true
```

`standard` is byte-identical to what every release before issue #18 wrote: the
`read_only` key is simply absent. Switching an instance from `strict` back to
`standard` rewrites the file without the key, so the diff is always explicit.

`strict` additionally turns the root filesystem read-only and drops **every**
capability. An agent needs none: it talks to its model endpoint over the
network, writes under `/opt/data` (the bind mount) and `/tmp`, and never mounts
the Docker socket. Dropping `ALL` also removes `NET_RAW`, so the standard-mode
`cap_drop: [NET_RAW]` entry disappears rather than being duplicated.

## What `strict` gives

- **`read_only: true`** — no write anywhere outside the declared mounts. A
  compromised agent cannot drop a binary in `/usr/local/bin`, rewrite its own
  entrypoint, add a cron job for the image's user or plant a file that a later
  container start would execute.
- **`cap_drop: [ALL]`** — no `CAP_CHOWN`, `CAP_SETUID`, `CAP_SETGID`,
  `CAP_DAC_OVERRIDE`, `CAP_NET_RAW`… A process that escapes the agent runs as an
  unprivileged UID with no way to change that.
- **`no-new-privileges`** — setuid binaries cannot raise the privilege of a
  process, even inside the writable mounts.
- **`tmpfs` on `/tmp`, `/run`, `/var/tmp`** — the scratch directories stay
  writable, are memory-backed (so they vanish with the container) and do not
  count against the image layer.

The container already has no Docker socket, a `pids_limit` of 256, a memory
limit, an `unless-stopped` restart policy and a read-only-by-default egress
firewall (see [EGRESS.md](EGRESS.md)). `strict` is the last layer: a read-only
rootfs and an empty capability set.

## The s6 / capability analysis

The stock Hermes image (`hermes-agent:0.22.2-local`, Hermes 0.22.2) is **not**
`USER 10000` from the start:

- it starts as **root**;
- its s6 entrypoint `chown`s `/opt/data` (and the mounted shares) to
  `HERMES_UID`/`HERMES_GID` — `10000:10000` in a fleett compose file — then
  drops privileges with `s6-setuidgid`;
- `setuid`/`setgid` needs `CAP_SETUID` and `CAP_SETGID`, and `chown` needs
  `CAP_CHOWN` — all three are **gone** under `cap_drop: [ALL]`;
- s6 also writes its supervision tree and service state under `/run`, which is
  why `strict` mounts a `tmpfs` there (without it, s6 cannot create
  `/run/s6/services` and the container exits during startup).

Consequences, in order of how likely they are to bite:

1. **Under `cap_drop: [ALL]`, `chown /opt/data` fails** ("Operation not
   permitted"). The entrypoint may tolerate it (the directory is usually already
   owned by `10000:10000` because fleett created it, and the compose file mounts
   a host directory), or it may abort.
2. **Under `cap_drop: [ALL]`, `s6-setuidgid` fails** if the drop happens in the
   container rather than through the image's own `USER`. The agent then keeps
   running as root inside the container — restricted by the read-only rootfs and
   the empty capability set, but not by UID.

So `strict` is an **opt-in** posture: test it against the exact image tag you
run before enabling it fleet-wide. If the image needs one of those capabilities,
either

- keep `standard` (the default) and rely on the read-only-by-default egress
  firewall plus the absence of the Docker socket, or
- pre-provision the data directory so the `chown` is a no-op *and* confirm the
  UID drop happens through the image's `USER`/`GROUP` rather than `CAP_SETUID`.

Never widen the capability set without recording why: `cap_add` in a rendered
compose file is the one line that voids most of this document.

## Verify before enabling

Run these on a copy of **your** image tag, not on a generic `debian` image:

```bash
# 1. Capabilities the image starts with (as root, before any drop).
docker run --rm --entrypoint sh hermes-agent:0.22.2-local -c 'command -v capsh >/dev/null && capsh --print || echo "no capsh in the image"'

# 2. Does the entrypoint still come up with every capability dropped and a
#    read-only rootfs? Use the same command a fleett instance runs.
docker run --rm \
  --read-only \
  --tmpfs /tmp --tmpfs /run --tmpfs /var/tmp \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -v /tmp/fleett-probe/data:/opt/data \
  -e HERMES_UID=10000 -e HERMES_GID=10000 \
  --entrypoint gateway hermes-agent:0.22.2-local run

# 3. A rendered compose file can be probed the same way.
docker compose -f /opt/fleett/instances/<id>/compose.yaml up
```

Step 2 is the real gate: it either prints the gateway startup logs and stays up,
or exits with `Operation not permitted` / a permission error on `/run`.

## Verify after enabling

```bash
# No capability at all. `capsh --print` prints "Current: = ..." (empty set) and
# "Bounding set =" with nothing after it.
docker exec -it fleett-<id> sh -c 'capsh --print | sed -n "1,6p"'
# Expected (the important lines):
#   Current: =
#   Bounding set =
#   NoNewPrivs:     1

# The root filesystem is read-only: this must fail with
# "Read-only file system".
docker exec fleett-<id> sh -c 'touch /usr/local/bin/probe' || echo "read-only rootfs: OK"

# /tmp is writable and memory-backed.
docker exec fleett-<id> sh -c 'touch /tmp/probe && echo "tmpfs: OK"'

# The compose file itself: read_only, cap_drop ALL, the three tmpfs mounts and a
# single no-new-privileges flag.
docker inspect fleett-<id> --format '{{.HostConfig.ReadonlyRootfs}}'          # true
docker inspect fleett-<id> --format '{{.HostConfig.CapDrop}}'                 # [ALL]
docker inspect fleett-<id> --format '{{.HostConfig.SecurityOpt}}'             # [no-new-privileges:true]
docker inspect fleett-<id> --format '{{json .HostConfig.Tmpfs}}'              # /run,/tmp,/var/tmp
```

`docker inspect` on a running instance is the authority: the compose file is
what fleett wrote, the inspect payload is what Docker enforced.

## Rolling it out

`FLEETT_HARDENING` is read at startup and applied when a compose file is
rewritten:

```bash
# 1. Probe the image (see above) — do not skip this step.
# 2. Flip the setting and restart the control plane.
FLEETT_HARDENING=strict
# 3. Re-render one instance and watch it come up.
fleett restart <id>          # rewrites nothing: start/stop reuse the file
#    A PATCH, an upgrade or a `fleett upgrade --image` rewrites it. To re-render
#    the same settings through the API:
curl -fsS -X PATCH -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:8090/api/instances/<id>
docker inspect fleett-<id> --format '{{.HostConfig.ReadonlyRootfs}}'   # true
# 4. Only then touch the rest of the fleet.
```

An instance that fails to start after the flip keeps its compose file: set
`FLEETT_HARDENING=standard`, restart the control plane and PATCH the instance
again to go back to the previous bytes.
