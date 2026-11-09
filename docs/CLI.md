# fleett CLI

The `fleett` binary is both the control plane (`fleett`, `fleett serve`) and a client for a
running control plane. Client commands talk to the HTTP API, so they work from any machine
that can reach the bind address, without a browser and without the local data root.

## Configuration

Client commands read `~/.config/fleett/config.toml`:

```toml
[server]
url = "http://127.0.0.1:8090"
token = "flt_0123456789abcdef0123456789abcdef"
```

- `url`: base URL of the control plane, with or without a trailing slash. `http://` and
  `https://` only.
- `token`: bearer token (`FLEETT_TOKEN`, or a named token created with `POST /api/tokens`).
  It is optional: a server started with `FLEETT_ALLOW_NO_AUTH=1` answers without one.
- The file is read for client commands only. `fleett serve`, `migrate`, `egress`, `secrets`,
  `backup` keep reading the server environment (`~/.fleett/fleett.env`).

Environment variables win over the file, field by field:

| Variable | Overrides |
|---|---|
| `FLEETT_URL` | `[server] url` |
| `FLEETT_TOKEN` | `[server] token` |

A missing configuration file is fine when both variables are set. A missing URL is an
error before any request is sent. Keep the file `0600` when it holds a token:

```bash
install -d -m 700 ~/.config/fleett
install -m 600 /dev/null ~/.config/fleett/config.toml
```

## Commands

```
fleett ls
fleett ps [--all]
fleett logs <id> [--tail N] [--since DURATION|TIMESTAMP] [--grep REGEX] [--level error|warn]
fleett create --id ID [--name NAME] [--template TEMPLATE] [--model MODEL]
               [--base-url URL] [--api-key-env VAR] [--gate-token TOKEN]
               [--channel telegram|discord|none] [--channel-token TOKEN]
               [--allow-lan] [--cpu N] [--mem MB] [--note TEXT]
fleett start <id>
fleett stop <id>
fleett restart <id>
fleett delete <id> [--purge]
fleett usage
fleett cost [--from DATE] [--to DATE] [--group-by instance|model|day]
fleett pricing [list | set MODEL --input X --output Y [--currency C] [--effective-from DATE]]
fleett budget <id> [--set USD]
fleett images
fleett exec <id> [--timeout N] -- <argv...>
fleett exec-policy
fleett upgrade --image TAG [--parallel N] [--dry-run] [--timeout N]
fleett say <to> <body> [--kind ask|tell|result] [--from ID] [--correlation ID] [--ttl S]
fleett messages <id> [--view inbox|outbox|all] [--status queued|delivered|read] [--limit N]
fleett ack <id> <message-id>
fleett templates
fleett template get <id>
fleett template create [-f file.yaml] [--id ID --name NAME --description TEXT]
                       [--parent PARENT] [--default-model MODEL] [--channel telegram|discord|none]
                       [--config FILE] [--soul FILE] [--defaults FILE] [--env KEY]…
fleett template from-instance <instance> --id <new> [--name NAME]
fleett template rm <id>
fleett template versions <id>
fleett template restore <id> <version>
fleett schedules <id>
fleett schedule add <id> --name NAME --cron EXPR --prompt TEXT
                [--enabled|--disabled] [--overlap skip|queue] [--timeout S]
                [--timezone TZ] [--catchup]
fleett schedule rm <schedule-id>
fleett schedule run <schedule-id>
fleett runs <id> [--limit N]
fleett incidents [--status open|closed] [--instance ID] [--limit N]
fleett heal <id> --policy off|restart|recreate
              [--unhealthy-after N] [--max-restarts N] [--backoff SECONDS]
fleett incident close <incident-id>
fleett doctor
fleett mcp [--http ADDR]
```

Every command accepts the global `--json` flag (before or after the subcommand). Human
output is plain aligned text with no terminal UI dependency.

### `ls`

Lists every instance: id, name, status, health, template, model, channel and CPU limit.

```bash
fleett ls
fleett --json ls | jq '.[].id'
```

### `ps`

Shows the instance containers with their short container id and uptime, like `docker ps`.
Stopped instances are hidden unless `--all` is given.

```bash
fleett ps
fleett ps --all --json
```

### `logs`

Prints the logs of one instance. `--tail` defaults to 200 (maximum 2000). `--since` accepts a
Docker duration (`30s`, `10m`, `2h`, `1h30m`) or an RFC 3339 timestamp. `--grep` is a regex,
`--level warn` keeps warnings **and** errors. With `--json` the API returns
`[{"time":"…","message":"…"}]` or `[{"line":"…"}]` entries.

```bash
fleett logs alice --tail 50
fleett logs alice --since 10m --grep 'gateway|health'
fleett logs alice --level error --json
```

### `create`

Creates an instance and starts it. `--id` is required (`[a-z0-9-]{2,32}`); everything else is
optional and validated by the server. `--base-url` defaults to
`https://opencode.ai/inference/go/openai/v1`, the same value the web console pre-fills.
`--gate-token` is stored as `HERMES_GATE_PROFILE_TOKEN`; `--channel-token` is stored as
`TELEGRAM_BOT_TOKEN` or `DISCORD_BOT_TOKEN` depending on `--channel` (it requires
`--channel`).

```bash
fleett create --id test --template solo \
  --model muse-spark-1.3-contributor \
  --base-url https://opencode.ai/inference/go/openai/v1 \
  --gate-token "$FLEETT_GATE_TOKEN" \
  --channel telegram --channel-token "$TELEGRAM_BOT_TOKEN" \
  --cpu 1.5 --mem 2048 < /dev/null
```

The fleet caps `FLEETT_MAX_TOTAL_CPU` / `FLEETT_MAX_TOTAL_MEM_MB` are enforced by the server:
an instance over the cap fails with `409 cpu_cap_exceeded` or `409 mem_cap_exceeded` and the
detail `{used, requested, cap}`.

### `start` / `stop` / `restart` / `delete`

```bash
fleett stop alice
fleett restart alice
fleett delete alice            # keeps instances/alice/data
fleett delete alice --purge    # erases the data directory too
```

### `usage`

CPU, memory and disk usage of the fleet, read from `docker stats` and `du -sb` on the data
directories, plus the fleet totals and the configured caps (`0` means unlimited).

```bash
fleett usage
fleett --json usage | jq '.totals, .caps'
```

A stopped instance reports zero CPU/memory usage; its disk usage is still measured. Docker
failures answer `502 usage_unavailable` with the Docker error.

### `cost`

Token usage and cost of the fleet ([docs/COST.md](COST.md)), aggregated by `instance`
(default), `model` or `day`. The range defaults to the current month up to now; `--from` and
`--to` accept `YYYY-MM-DD` or an RFC 3339 timestamp (a bare date for `--to` includes the whole
day).

```bash
fleett cost
fleett cost --from 2026-11-01 --to 2026-11-30
fleett cost --group-by model
fleett --json cost --group-by day | jq '.groups[] | {key, cost_usd}'
```

The table lists `KEY`, `PROMPT`, `COMPLETION`, `TOTAL`, `COST` and `EVENTS`, followed by the
top agents, the per-instance budgets and the month projection (linear estimate from the
elapsed fraction of the month). `--json` prints the raw `CostReport`.

### `pricing`

Shows or changes the price of a model, in currency per million tokens:

```bash
fleett pricing
fleett pricing set deepseek-v4.1-flash --input 0.27 --output 1.10
fleett pricing set claude-sonnet-4 --input 3 --output 15 \
  --currency USD --effective-from 2026-12-01
```

Prices are versioned by effective date: a new `--effective-from` adds a version and the
historical cost keeps the price that was effective on each event day. Re-setting the same
model and date replaces that version. Without `--effective-from`, the price applies from
today (UTC). `fleett pricing` lists the latest version of each model.

### `budget`

Shows or sets the monthly ceiling of one instance, in USD:

```bash
fleett budget alice             # configured limit, month-to-date and projection
fleett budget alice --set 25    # 25 USD per month
fleett --json budget alice --set 25 | jq '{monthly_usd, percent, exceeded}'
```

`--set 0` keeps a zero ceiling (it never fires); a crossing is reported as a `cost.budget`
event (see [COST.md](COST.md#alert-hook)).

### `images`

Lists the container images present on the host, with the instances that use each tag:

```bash
fleett images
fleett --json images | jq '.[] | select(.instances | length > 0)'
```

Columns: `TAG` (`repository:tag`), `ID` (first 12 hex characters), `SIZE`, `CREATED` and
`INSTANCES` (comma-separated ids, `-` when the tag is unused). Dangling images (`<none>:<none>`)
are omitted. A Docker failure answers `502 images_unavailable` with the Docker error.

### `exec`

Runs one allowlisted command **inside the instance container**, without SSH and without a
host-side `docker exec`. Full security model in [docs/EXEC.md](EXEC.md).

```bash
fleett exec alice -- ls /opt/data
fleett exec alice --timeout 10 -- df -h
fleett exec alice -- cat /opt/data/config.yaml
fleett --json exec alice -- ls /opt/data | jq -r .stdout
```

The command and its arguments come after `--` and are sent as an argv array: no shell is
involved, locally or remotely. stdout goes to stdout, stderr to stderr. `--timeout` defaults
to 30 s and is bounded to 1..=300. The exit code of the remote command becomes the exit code
of `fleett exec` (`124` after a timeout), so scripts can branch on it. `--json` prints the
full `{"exit_code","stdout","stderr","timed_out","duration_ms","truncated"}` object.

The default allowlist is `ls cat tail head grep env ps df du date which pwd id uname uptime
free echo`; the server answers `403 exec_refused` with the reason and the effective allowlist
for anything else, and a hard denylist refuses `rm`, shells, package managers and other
destructive binaries even when `FLEETT_EXEC_ALLOWLIST=*`. Every call is audited as
`instance.exec` (`result`, `argv`, exit code, duration, truncation).

### `exec-policy`

Prints the effective policy of the control plane: source, wildcard flag, allowlist, denylist,
timeout bounds and argument/output caps.

```bash
fleett exec-policy
fleett --json exec-policy | jq .allowlist
```

Admin only. On the server, `FLEETT_EXEC_ALLOWLIST` (comma-separated basenames) replaces the
default allowlist; `*` disables it while the hard denylist stays in force.

### `upgrade`

Moves every agent to another image, one instance at a time by default, and rolls back the
instances that never become healthy. See `docs/UPGRADE.md` for the procedure and the rollback
semantics.

```bash
fleett upgrade --image hermes-agent:0.23.0-local --dry-run
fleett upgrade --image hermes-agent:0.23.0-local
fleett upgrade --image hermes-agent:0.23.0-local --parallel 2 --timeout 300
fleett --json upgrade --image hermes-agent:0.23.0-local | jq '.results[] | {id, status}'
```

The command posts the job, polls `GET /api/upgrade` every 2 s and prints a line whenever an
instance changes state, then a final table (`ID`, `FROM`, `TO`, `STATUS`) with the error of
each instance that did not keep the new image. `--dry-run` only prints the plan
(`id  from -> to`), the server writes nothing. Exit code `1` when at least one instance ends
`rolled_back` or `failed`; the table is still printed. `--json` prints the final job and
nothing intermediate.

### `say` / `messages` / `ack`

Talk to another instance over the persistent bus ([docs/BUS.md](BUS.md)):

```bash
fleett say tchouniz "summarize today's incidents" --kind ask --correlation incidents-1
fleett messages tchouniz
fleett messages tchouniz --view outbox --status delivered
fleett ack tchouniz 3f2a…
```

The sender defaults to `--from`, then to `FLEETT_INSTANCE`. `--json` prints the raw
`Message` objects.

### `templates` / `template`

Manage the versioned instance templates ([docs/TEMPLATES.md](TEMPLATES.md)). `templates`
prints one row per template (id, name, parent, read-only, channel, model, env entries,
instances); `template get` adds the env schema and the defaults. Mutations require the admin
role.

```bash
fleett templates
fleett template get house

# Full definition (the POST /api/templates body) — the only way to set
# descriptions, examples, required/secret flags and defaults in one go.
fleett template create -f house.yaml

# Quick creation from flags.
fleett template create --id house --parent guest --channel telegram \
  --config config.yaml --soul SOUL.md --defaults defaults.yaml \
  --env HOUSE_FLAG --env TELEGRAM_ALLOWED_USERS

# Save a running instance (config, SOUL.md, limits and env schema).
fleett template from-instance tchouniz --id tchouniz-template

fleett template versions house
fleett template restore house 1   # rewrites the files, appends a new version
fleett template rm house          # 409 template_in_use while an instance uses it
```

`fleett template rm` refuses a built-in (`409 read_only`) and a template still used by an
instance or a child template (`409 template_in_use`). `--json` prints the raw API payload.
### `schedules` / `schedule` / `runs`

Manage the cron schedules of an instance ([docs/SCHEDULES.md](SCHEDULES.md)):

```bash
fleett schedule add alice --name every-five --cron '*/5 * * * *' \
  --prompt 'summarize the incidents' --timeout 120
fleett schedules alice
fleett schedule run <schedule-id>
fleett schedule rm <schedule-id>
fleett runs alice --limit 20
```

`schedules` prints one row per schedule: `NAME`, `CRON`, `ENABLED`, `NEXT`,
`LAST`, `OVERLAP`, `TIMEOUT`. `schedule add` accepts `--name`, `--cron` and
`--prompt` (required), plus `--enabled` (the default), `--disabled`,
`--overlap skip|queue`, `--timeout` (1..=3600 s), `--timezone` (stored, UTC
evaluation for now) and `--catchup`. `schedule run` executes the schedule
immediately and prints `run <id>: <status> (<duration> ms), exit <code>`; a
non-`ok` run prints its output indented. `runs` prints `STARTED`, `STATUS`,
`EXIT`, `DURATION` and the first 80 characters of `OUTPUT`. Every command
accepts `--json` and prints the raw `Schedule` / `ScheduleRun` objects.

### `memory`

Read and write the shared knowledge base ([docs/MEMORY.md](MEMORY.md)):

```bash
fleett memory set instance:probe runbook "restart the gateway after an upgrade" --tag ops
fleett memory search "gateway upgrade"
fleett memory history 12
fleett memory restore 12 1
fleett memory promote 12 --namespace global
```

Namespaces: `global`, `shared`, `instance:<id>`, `project:<name>`. `global` writes require
the admin role. `--json` prints the raw `MemoryEntry` / `MemoryVersion` objects.
### `incidents` / `heal` / `incident close`

Read the watchdog incident registry and change the self-healing policy
([docs/WATCHDOG.md](WATCHDOG.md)):

```bash
fleett incidents
fleett incidents --status open --instance alice
fleett heal alice --policy restart --unhealthy-after 2 --max-restarts 3 --backoff 15
fleett heal alice --policy off
fleett incident close 3f2a…
```

`incidents` prints `OPENED`, `INSTANCE`, `CAUSE`, `STATUS`, `RESTARTS` and
`DETAIL`, newest first. `--json` prints the raw incident array. `heal` prints the
resulting policy (`id: heal=restart unhealthy_after=2 max_restarts=3 backoff=15s`);
`incident close` prints the closed incident. Both mutations require an admin
token and are written to the audit log.

### `doctor`

Prints a checklist and exits non-zero when a check fails:

- `api`: the control plane answers `GET /api/health`
- `auth`: the configured token is accepted (`GET /api/auth/me`)
- `docker`: the Docker engine answers
- `secrets`: an encryption key is configured (`fleett secrets init`)
- `egress`: `FLEETT_EGRESS=restrict` has a fresh firewall marker (`fleett egress install`);
  `FLEETT_EGRESS=off` is reported as a warning
- instance warnings: one `warn` line per warning reported by `GET /api/instances/{id}`
  (missing template allowlist, plaintext `secrets.env`, LAN access, …), prefixed with the
  instance id. A clean fleet adds no line; warnings never change the exit code.

```bash
fleett doctor
fleett --json doctor
```

A `guest-discord` instance created without an allowlist therefore reads:

```
  warn  warnings alice: DISCORD_ALLOWED_USERS is required by the guest-discord template but is
                       not set: the gateway will refuse every message until it is
```

### `mcp`

Runs a Model Context Protocol server that exposes the fleet as nineteen strict tools
(`list_instances`, `get_instance`, `create_instance`, `start/stop/restart/delete_instance`,
`logs`, `env_set/env_unset`, `read_file/write_file`, `git_log/git_status/git_revert`,
`list_providers`, `test_provider`, `usage`, `run_pipeline`). Every tool is an HTTP API call
made with the same URL and token as the other client commands, so the API keeps enforcing the
token role and auditing every mutation; the server adds no privilege. See
[docs/MCP.md](MCP.md) for the full catalog, the Hermes configuration and the HTTP mode.

```bash
fleett mcp                              # stdio: one JSON-RPC message per line
fleett mcp --http 127.0.0.1:8091        # Streamable HTTP: POST /mcp
```

Stdout only carries JSON-RPC responses (diagnostics go to stderr). The global `--json` flag
is ignored: tool results are MCP `content` + `structuredContent`, and execution errors are
`isError` results carrying the API code. A token without the required role is refused by the
API and the refusal is returned as-is (for example `code: "forbidden", status: 403`).

## Output and exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | API error, connection error, configuration error, a failed `doctor` check, or an `upgrade` with a rolled-back/failed instance |
| `2` | invalid command line (clap) |
| `3..255` | `fleett exec`: the remote command's exit code is mirrored |
| `124` | `fleett exec`: the command was killed at its timeout |

API errors print the API `error.message` and `error.code` on stderr:

```
fleett: instance "ghost" not found (code: not_found)
```

With `--json`, errors are machine-readable on stderr and leave stdout untouched:

```json
{"error":{"code":"not_found","message":"instance \"ghost\" not found"}}
```

## Scripting

```bash
# Fail a cron job when doctor reports a problem.
fleett doctor >/dev/null || echo "fleett is degraded" | mail -s fleett admin@example.test

# Create an agent and print its id.
id=$(fleett --json create --id ci-agent --template guest --gate-token "$TOKEN" | jq -r .id)
fleett stop "$id"

# Fail a deployment when a rolling upgrade did not stick.
fleett upgrade --image "$TAG" || echo "some agents were rolled back" >&2
```
