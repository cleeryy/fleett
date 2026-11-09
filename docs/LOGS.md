# Logs — persistent, searchable log center

Until v0.5, `/api/instances/{id}/logs` ran `docker logs` on demand: no
cross-instance search, no retention, no way to correlate two agents at the same
instant. The log center fixes that: a background task tails every instance
container, redacts known secrets, stores the lines in SQLite with an FTS5 index
and serves them through search, live stream and export endpoints.

```
docker compose logs --tail 500 (every 10 s)
        │
        ▼
  redaction (tokens, bearer, bot tokens)
        │
        ├──► log_entries (SQLite) ──► log_fts (FTS5)
        ├──► broadcast channel ─────► GET /api/logs/stream (SSE)
        └──► hourly purge (age + row cap)
```

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_LOG_RETENTION` | `7d` | Maximum age of a stored line. Accepts `s`, `m`, `h`, `d` and `w` units, optionally compound (`1h30m`), or a bare number of seconds. Must be positive. |
| `FLEETT_LOG_MAX_ROWS` | `500000` | Maximum number of rows kept. `0` disables the row cap (age still applies). |

```bash
FLEETT_LOG_RETENTION=24h FLEETT_LOG_MAX_ROWS=200000 fleett serve
```

The settings are read at startup like every other `FLEETT_*` variable (process
environment first, then `$FLEETT_ROOT/fleett.env`). An invalid value is a
startup error, never a silent fallback: a typo in a retention window must not
quietly keep months of logs.

## Ingestion

`serve` starts one task that, every **10 seconds**, reads the last 500 lines of
each instance (`docker compose logs --tail 500`) and ingests them. A failing
container is counted and skipped; one broken instance never stops the tail of
the others. The ingestion text is bounded to 4 MiB per instance, the same bound
as the on-demand logs endpoint.

Tests never start the background task: they call `logstore::tick(&state)`
directly, which runs exactly one tail + retention pass.

Every tick reads the same tail again, so storage must be idempotent. A line is
identified by `(instance_id, sha256(raw line))`; the unique index makes a
replayed line a no-op. This means an agent that prints the exact same line twice
without a timestamp (for example `retry`) is stored once — that is the price of
making a blind tail safe, and it is why the line hash covers the raw line
including any timestamp the agent prints.

### Parsing

| Field | Source |
|---|---|
| `at` | Leading RFC 3339 timestamp (after the `docker compose logs` container prefix), normalised to UTC milliseconds. Without one, the ingestion time is used. |
| `level` | A JSON line is read first (`level`, `severity` or `lvl`). Otherwise `ERROR`/`FATAL`/`CRITICAL`/`PANIC`, `WARN`/`WARNING` and `INFO` keywords are scanned and the most severe wins. Default: `info`. |
| `message` | The line without the container prefix and leading timestamp, redacted. This is the text searched by FTS. |
| `raw` | The whole line, redacted. Kept for exports/diagnostics; not returned by the search API. |

## Redaction

Known secret patterns are replaced **before** the row is written, so a token in
a container log never reaches the database, a search response, an export or the
SSE stream:

| Pattern | Replaced by |
|---|---|
| `flt_[0-9a-f]{32}` (fleett API token) | `flt_[REDACTED]` |
| `sk-[A-Za-z0-9_-]{20,}` (OpenAI-style key) | `sk-[REDACTED]` |
| `Bot <token>` (Discord bot token) | `Bot [REDACTED]` |
| `Bearer <token>` | `Bearer [REDACTED]` |
| `token=<value>` (query string or key-value) | `token=[REDACTED]` |

Redaction is best-effort by design: it covers the patterns fleett itself hands
to containers (`HERMES_GATE_PROFILE_TOKEN`, provider keys, channel tokens) and
the common bearer forms. An agent that invents another secret format is not
covered. The rule remains: never log a secret.

## Search

```
GET /api/logs?instances=alice,bob&q=allowlist&level=warn&since=24h&until=2026-11-09T12:00:00Z&limit=100
```

- `instances`: comma-separated ids, empty or absent means **every** instance.
  At most 100 ids per request.
- `q`: FTS5 query. A partial word (`allow` in `allowlist`) is not an FTS token,
  so when FTS returns no row the same filter is retried as a `LIKE '%q%'`
  substring search. A syntactically invalid FTS query also falls back to LIKE
  instead of failing the request.
- `level`: `info` (everything), `warn` (warnings **and** errors) or `error`.
- `since` / `until`: RFC 3339 timestamp or relative window (`30m`, `24h`, `7d`)
  meaning "now minus the window". `since` must be before `until`.
- `after_id`: only rows whose `id` is strictly greater than this cursor. The
  live follow uses it to resume: ids are monotonic, so a burst that is larger
  than one page cannot silently lose the lines in between (a timestamp cursor
  would). It combines with every other filter and never changes the ordering.
  A negative or non-numeric value answers `400 invalid_after_id`.
- `limit`: `1..=500`, default 100.

Rows are sorted by `at` **descending**, then by id descending for lines sharing
the same timestamp. A search never returns the `raw` column.

```bash
curl -sS -H "Authorization: Bearer $FLEETT_TOKEN" \
  'http://127.0.0.1:8090/api/logs?q=allowlist&level=warn&since=24h' | jq .
```

## Live stream (SSE)

```
GET /api/logs/stream?instances=alice,bob
Accept: text/event-stream
```

The endpoint subscribes to the ingestion broadcast channel and emits one SSE
event per newly inserted line:

```
event: log
id: 42
data: {"id":42,"instance":"alice","at":"2026-11-09T10:00:00.000Z","level":"error","message":"gateway died"}

: heartbeat
```

- The SSE `id:` field mirrors the JSON `id`. It is informational: the server
  does not replay from `Last-Event-ID`; resume a gap with `after_id` on the
  search endpoint.
- Latency after ingestion is immediate; the 10 s tail cadence is the only
  delay between the container writing and the line reaching a connected console.
- A `:` comment heartbeat is sent every 15 s when idle, so proxies do not close
  the connection.
- A slow client that lags behind the 1024-event channel buffer receives a gap;
  the server logs the number of skipped events (`the log SSE client fell
  behind`) and keeps streaming. Re-run a search to recover the gap.
- The stream is authenticated like every `/api/*` route. Browsers connect with
  the `fleett_session` cookie; scripts use `Authorization: Bearer`.

## Live follow in the console

The per-instance log panel opens the SSE stream and, whenever the stream is
down or reconnecting, falls back to a **1-second poll** of
`GET /api/logs/instances/{id}`. Both paths feed the same buffer:

- the first page loads `limit` rows; afterwards the poll sends the newest entry
  id as `after_id`, so the lines written between two ticks are picked up even
  when they share the newest timestamp (one tick still returns at most 500
  rows);
- every line is deduplicated by id, because the stream and the poll overlap on
  purpose (that overlap is what closes the reconnect gap);
- the buffer keeps the newest 2000 lines, "N new lines" counts what arrived
  while the operator was scrolled up, and a hidden tab pauses both paths.

The fleet-wide **Follow** toggle on the Logs page polls `/api/logs` every
second with the same `after_id` cursor and the current filters. The log-center
read routes belong to the ordinary `api` rate-limit bucket for that reason;
only the legacy `/api/instances/{id}/logs` (a real `docker logs`) is `heavy`.

## Export

```
GET /api/logs/export?format=json|csv&<same filters as /api/logs>
```

`json` returns the array of entries; `csv` returns a header row
`at,instance,level,message` with RFC 4180 quoting. Both carry
`Content-Disposition: attachment`. The export uses the same 500-row `limit`, so
it is a download of the current result set, not a full-database dump.

## Retention

Once an hour the background task runs one transaction that:

1. deletes the rows older than `FLEETT_LOG_RETENTION` and their FTS documents;
2. deletes the oldest rows over `FLEETT_LOG_MAX_ROWS` (when the cap is non-zero).

The reported counts (`removed_old`, `removed_overflow`, `remaining`, `at`) are
logged at INFO. `logstore::purge_retention` returns the same report and is what
the tests assert. Search stays on the `(at)`, `(instance_id, at)` and `(level)`
indexes; an FTS query touches the inverted index first.

## CLI

```bash
fleett logs search --q allowlist --instance alice --level warn --since 24h --limit 50
fleett logs tail --instance alice
fleett logs export --format csv -f /tmp/logs.csv --q allowlist
```

`fleett logs alice` (the classic on-demand per-instance view) is unchanged.
See [CLI.md](CLI.md#logs--log-center) for output details.

## Limits and failure modes

| Bound | Value |
|---|---|
| Lines read per instance per tick | 500 |
| Raw text scanned per instance per tick | 4 MiB |
| Rows returned by one search/export | 500 |
| Instances in one filter | 100 |
| FTS query length | 512 characters |
| SSE channel buffer | 1024 events |

- **A lost database loses history, never an agent.** `log_entries` is not
  reconciled from disk; deleting `fleet.db` starts an empty log center.
- **No live tail without `serve`.** Maintenance commands do not tail anything.
- **Mock driver.** Under `FLEETT_DRIVER=mock` the tail reads the simulated log of
  each instance; tests use it to exercise ingestion end to end.
- **Search cost.** The FTS path is index-backed. The LIKE fallback is a scan
  bounded by the other filters and the 500-row limit; it is meant for partial
  words, not for querying millions of rows.
