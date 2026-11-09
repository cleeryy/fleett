# Exec — allowlisted one-shot commands inside an instance

`fleett exec` and `POST /api/instances/{id}/exec` run **one** command inside the
agent container of an instance, without SSH and without `docker exec` on the
host. The result is captured stdout, stderr and an exit code, and every call is
written to the audit trail.

This is a diagnostic console, not a remote shell: there is no stdin, no PTY and
no session state. A command either finishes or is killed at its timeout.

## Security model

Execution is **argv based**. The compose driver runs:

```
docker compose -f instances/<id>/compose.yaml -p <project> exec -T agent <argv…>
```

No shell is involved at any point, so `|`, `;`, `&&`, `$(…)`, `>` and quotes are
ordinary characters — they never expand, chain or redirect. The only gate
between an operator and code inside the container is the policy below, applied
before the driver is called.

| Layer | Rule |
|---|---|
| Allowlist | Command **basenames** accepted by default: `ls cat tail head grep env ps df du date which pwd id uname uptime free echo`. Configurable with `FLEETT_EXEC_ALLOWLIST`. |
| Denylist | Always refused, **even when the allowlist is `*`**: `rm rmdir mv dd mkfs sh bash zsh fish python python3 perl ruby node nc ncat curl wget socat chmod chown mount umount kill killall reboot shutdown docker systemctl su sudo apt apk yum`. |
| Basename only | `argv[0]` must be a plain name (`[A-Za-z0-9._+-]`). A path (`/bin/ls`, `./ls`, `bin/cat`) and a name starting with `-` are refused, so the policy cannot be pointed at another binary. |
| `env` guard | In allowlist mode `env` accepts no argument: `env <cmd>` would execute an unvetted command. `env` alone still dumps the container environment. |
| argv bounds | At most 32 arguments, each at most 4096 bytes, no NUL byte. |
| Timeout | `timeout_s` between 1 and 300 seconds (default 30). The child is killed with `kill_on_drop` when the deadline expires. |
| Output cap | `stdout` and `stderr` are capped separately at 256 KiB; a truncated stream ends with `[fleett: output truncated at 262144 bytes]` and the response sets `"truncated": true`. |
| Scope | The command runs **inside the agent container**, as the agent user, with the instance mounts. It cannot manage the host or other instances. |
| Roles | `admin` only. A `viewer` token gets `403 forbidden`. |
| Audit | Every accepted **and** refused call writes `instance.exec` (actor, role, target, `argv`, timeout, exit code, duration, truncated, `result` = `ok\|refused\|timeout\|error`, and `reason` when refused). |

### Configuring the allowlist

```bash
# add htop and stat to the defaults? No: FLEETT_EXEC_ALLOWLIST replaces it.
FLEETT_EXEC_ALLOWLIST=ls,cat,grep,df,du,htop

# explicit, audited "anything but the hard denylist" mode
FLEETT_EXEC_ALLOWLIST=*
```

The value is a comma-separated list of basenames. It replaces the default list
(it does not extend it). An absent or blank value keeps the default. Entries
that are not plain basenames are ignored with a startup warning: a typo must
fail closed. `GET /api/exec/policy` and `fleett exec-policy` show the effective
lists, their source (`default` or `FLEETT_EXEC_ALLOWLIST`) and whether the
wildcard is active.

`FLEETT_EXEC_ALLOWLIST=*` is logged at startup. It is a deliberate operator
decision, and every call still records `"wildcard": true` in the audit entry.

## HTTP API

`POST /api/instances/{id}/exec`

```json
{"argv": ["ls", "/opt/data"], "timeout_s": 5}
```

or, for a simple command line (split on whitespace, **no shell syntax**):

```json
{"cmd": "df -h"}
```

`cmd` refuses `; | & $ ` < > ( ) \ " '` and newlines with `400 invalid_exec`;
use `argv` for an argument that contains a space or a quote.

Response `200`:

```json
{"exit_code": 0, "stdout": "…", "stderr": "", "timed_out": false,
 "duration_ms": 12, "truncated": false}
```

A timeout is **not** an HTTP error: the call answers `200` with
`"timed_out": true` and `"exit_code": null`. The streams are empty because a
killed child cannot be drained reliably. Refusals answer `403`:

```json
{"error": {"code": "exec_refused",
  "message": "command refused by the exec policy (denied_command): \"rm\" is permanently refused by the exec denylist",
  "details": {"reason": "denied_command", "argv": ["rm", "-rf", "/"],
              "allowlist": ["cat", "…"], "denylist": ["apt", "…"],
              "wildcard": false}}}
```

`reason` is one of `empty_argv`, `too_many_arguments`, `argument_too_long`,
`invalid_argument`, `empty_command`, `invalid_command`, `path_not_allowed`,
`denied_command`, `not_allowlisted`, `unsafe_arguments`.

Malformed bodies answer `400` with `invalid_exec` or `invalid_timeout`; an
unknown instance answers `404`; a Docker failure answers `502 driver_error`.

`GET /api/exec/policy` (admin) returns the effective policy:

```json
{"wildcard": false, "source": "default",
 "allowlist": ["cat", "…"], "denylist": ["apt", "…"],
 "max_args": 32, "max_arg_bytes": 4096, "max_output_bytes": 262144,
 "timeout_s": {"default": 30, "min": 1, "max": 300}}
```

### SSE variant

`GET /api/instances/{id}/exec/stream?cmd=ls%20%2Fopt%2Fdata&timeout_s=5`
returns `text/event-stream`:

```
event: stdout
data: {"line":"MEMORY.md"}

event: stderr
data: {"line":"…"}

event: exit
data: {"exit_code":0,"timed_out":false,"duration_ms":12,"truncated":false}
```

**One-shot, not interactive**: the control plane runs the command to
completion, then emits the whole captured output and closes the stream. Events
arrive together; there is no stdin, no keystroke echo and no resize. A refusal
is a JSON `403 exec_refused` before the first SSE byte. `EventSource` cannot
send an `Authorization` header, so the console (and any client behind OIDC)
should read it with `fetch` and a bearer token/cookie.

## CLI

```bash
fleett exec alice -- ls /opt/data
fleett exec alice --timeout 10 -- df -h
fleett exec alice -- cat /opt/data/config.yaml
fleett --json exec alice -- ls /opt/data | jq -r .stdout
fleett exec-policy
```

The command and its arguments come **after** `--` and are sent as an argv array;
the local shell only splits them, exactly like `docker exec`. stdout and stderr
are printed on the matching streams, and the exit code of the remote command
becomes the exit code of `fleett exec` (124 after a timeout). `--timeout`
defaults to 30 and is bounded to 1..=300.

## Audit

Each call appends one `instance.exec` row (newest first in `/api/audit`):

```json
{"action":"instance.exec","target":"alice","actor":"bootstrap",
 "details":{"argv":["ls","/opt/data"],"timeout_s":5,"result":"ok",
            "exit_code":0,"duration_ms":12,"truncated":false,"wildcard":false}}
```

A refusal carries `"result":"refused"` and `"reason":"denied_command"`; a
timeout carries `"result":"timeout"`; a Docker failure `"result":"error"` with
the driver detail in `reason`.

## Limitations (v1)

- No interactive terminal: no stdin, no PTY, no `top`/`less`/`vi`.
- No streaming while the command runs: the SSE route replays the captured
  output after completion.
- The output cap applies per stream; a command emitting gigabytes is cut, not
  buffered.
- `docker compose exec` requires the container to be running; a stopped
  instance answers `502 driver_error` with Docker's message.
- The policy filters the executable name only: an allowlisted tool that can
  itself execute another program (`find -exec`, `xargs`, `awk 'system()'`) is
  not in the default allowlist for that reason. Adding one with
  `FLEETT_EXEC_ALLOWLIST` is equivalent to allowing everything that tool can
  run.
