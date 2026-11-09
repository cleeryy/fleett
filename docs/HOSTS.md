# Multiple Docker hosts

`fleett` normally drives the Docker daemon of the machine that runs the control
plane. Since 0.6 an instance can be placed on another daemon, reached through
Docker's own SSH transport (`DOCKER_HOST=ssh://…`) or a Docker context. The
control plane stays the source of truth: it keeps every compose file, the
database and the instance data on its own filesystem.

## Transports

| `transport` | `address` | Selector set on every `docker` / `docker compose` call |
|---|---|---|
| `local` (default) | empty | none: the process environment is inherited (local socket) |
| `ssh` | `user@host` or `ssh://user@host[:port]` | `DOCKER_HOST=ssh://user@host` |
| any | — | `docker_context` adds `DOCKER_CONTEXT=<name>` when configured |

The SSH endpoint is used by the Docker CLI itself: `fleett` does not open SSH
sessions, does not copy a key and never stores a password. The user running the
control plane must already be able to run `docker -H ssh://user@host version`
(or have the host in `~/.ssh/config`), and the daemon on the target must accept
API calls over SSH. `DOCKER_HOST=ssh://` needs the Docker CLI's SSH support,
which is present in every recent Docker CLI.

The registry is the `hosts` table (one migration, `docs/DEPLOY.md` upgrade is a
simple restart):

```text
id            stable slug used by `instances.host` (`local` is reserved)
name          human label, unique
transport     local | ssh
address       user@host or ssh://user@host (empty for local)
docker_context optional Docker context name
status        unknown | ok | unreachable (last probe)
last_check_at / last_error   last probe result
```

`local` is inserted by the migration; every pre-existing instance gets
`host='local'`, so an upgrade changes no behaviour. The local row cannot be
renamed, moved or deleted.

## The path requirement (important)

A bind mount in a compose file is resolved by the **daemon that runs the
container**, not by the CLI that sends the compose file. A remote host
therefore needs the exact same absolute paths as the controller:

```yaml
services:
  agent:
    volumes:
      - /opt/fleett/instances/alice/data:/opt/data
      - /opt/fleett/shares/findings:/opt/data/shared/findings
```

With `DOCKER_HOST=ssh://pve3`, Docker on `pve3` mounts
`/opt/fleett/instances/alice/data` **from pve3's own filesystem**. The directory
must exist there with the same contents. Two setups satisfy this:

1. **Shared storage** (NFS, CephFS, a shared ZFS dataset mounted at the same
   path): nothing else to do; `fleett instances move` with `--skip-transfer`
   only changes the routing.
2. **Per-host local storage**: the operator copies the instance directory to the
   target before starting it there. `fleett instances move` does it with
   `scp`/`ssh` (see below).

`fleett` never pretends a remote volume is shared. `GET /api/usage`'s
`disk_bytes` is measured on the controller's copy: if the remote storage is not
shared, that number describes the controller side, not the target.

Shares (`shares/<name>`) follow the same rule: a remote instance that attaches a
share needs that share directory on its host too.

## API

See `API.md` (`## Docker hosts`). The short version:

```bash
curl -s -H "Authorization: Bearer $FLEETT_TOKEN" \
  -d '{"name":"PVE 3","id":"pve3","transport":"ssh","address":"cle@10.1.20.5"}' \
  "$FLEETT_URL/api/hosts"
curl -s -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/hosts/pve3/test"
```

`GET /api/hosts` probes every host in parallel (`docker info`), records
`status`/`last_error`, and reports `capacity` (`ncpu`, `mem_total_bytes`,
`docker_version`) plus `free_mem_bytes`/`free_cpu` after subtracting the
declared limits of the instances already on that host. A dead host never fails
the list.

Creating an instance on a host:

```bash
curl -s -H "Authorization: Bearer $FLEETT_TOKEN" \
  -d '{"id":"alice","model":"deepseek-v4.1-flash","base_url":"…",
       "gate_token":"…","host":"pve3"}' \
  "$FLEETT_URL/api/instances"
```

`host` defaults to `local`. `host:"auto"` places the instance on the reachable
host with the most free memory, then free CPU, then the fewest instances; hosts
without room for the requested limits are skipped. An unknown host is a `400
unknown_host` before any file is written.

## CLI

```bash
fleett hosts
fleett host add --name "PVE 3" --id pve3 --transport ssh --address cle@10.1.20.5
fleett host test pve3
fleett host rm pve3                 # refused while instances still run there

fleett create --id alice --model … --gate-token … --host pve3
fleett instances move alice --to pve3
```

`fleett create` gained `--host <id|auto>`; `fleett ls`/`fleett hosts` show the
placement.

### `fleett instances move`

`POST /api/instances/{id}/move` stops the instance, writes the new host in the
database and starts it on the target. If the start fails, the label is rolled
back and the instance is restarted on its source host.

The CLI adds the data transfer for a remote host:

1. `POST /stop` (consistent snapshot);
2. `GET /api/instances/{id}/export` to a local temp archive;
3. `scp` to `user@host:/tmp/fleett-move-<id>-<pid>.tgz`;
4. `ssh user@host 'mkdir -p <instance-dir> && tar -xzf … -C <instance-dir> data'`
   and a best-effort `chown -R 10000:10000 data`;
5. `POST /api/instances/{id}/move {to, start}`.

Both steps use the operator's SSH configuration (`~/.ssh/config`, agent, keys);
`fleett` never stores an SSH secret. Options:

| Option | Effect |
|---|---|
| `--skip-transfer` | Trust that the target already sees the same path (shared storage). |
| `--no-start` | Leave the instance stopped after the relabel. |
| `--remote-instance-dir <path>` | Write the data somewhere else than the controller path (the compose bind mount must match it). |
| `--keep-archive` | Keep the archive copied to the target. |

If `scp`/`ssh` are missing or fail, the command restarts the source, prints the
exact manual steps and exits non-zero. The API itself never copies data: it only
moves the routing.

## Placement and capacity

* Capacity comes from `docker info` (`NCPU`, `MemTotal`, `ServerVersion`).
* Placement uses declared limits, not live usage, so it is deterministic.
* A host whose `MemTotal` is `0` (unknown) scores as zero free memory; it is only
  chosen when no better host exists. An explicit `host` always wins, even
  overcommitted.
* `409 host_in_use` on delete lists the instances still placed on the host.

## Limitations

* **Not verified against a real SSH daemon in CI.** The unit and API tests use
  `MockDriver`, which records the selected host and answers `probe_host` with
  canned capacity. The `DOCKER_HOST`/`DOCKER_CONTEXT` wiring is covered by a
  command-construction test and `docker info` parsing tests, but a real
  `ssh://` round trip (up, logs, exec, stop) must be validated on the target
  infrastructure. Issue #40's first acceptance criterion remains to be checked
  on real hardware.
* Compose files and data directories are controller-side. Only shared storage
  or an explicit transfer makes a remote bind mount work.
* `fleett migrate` (legacy `hermes-<id>` projects) remains local-only: legacy
  projects never existed on a remote host.
* Images are ensured on every instance host before an upgrade; `GET /api/images`
  aggregates reachable hosts and silently skips an unreachable one (logged).
* The egress firewall (`fleett egress install`) is installed per host. A
  restricted instance created on a remote host still goes through the local
  marker/subnet check, which cannot prove that the remote host enforces the
  rules: create remote instances with `allow_lan=true` (explicit opt-in) or
  `FLEETT_EGRESS=off` until per-host egress provisioning is added.
* Placement lives in the `instances.host` column, i.e. in `fleet.db`. The
  compose file stays host-agnostic on purpose, so losing the database (not the
  data root) resets every adopted instance to `local`; restore the database or
  move the instances again. Include `fleet.db` in backups, as `fleett backup`
  already does.
* `fleett instances move` does not move share contents; copy the shares
  separately or use shared storage.
