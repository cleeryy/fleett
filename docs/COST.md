# Tokens and costs — accounting per agent

`fleett` reads the token usage the Hermes gateway prints in the container logs,
prices it per model and compares it with a monthly budget per instance. The
result is available as `GET /api/cost` and `fleett cost`.

```
docker compose logs --tail 2000   ──►  parse_line()  ──►  usage_events
                                                              │
model_pricing (versioned by effective_from) ────────────────► cost
cost_budgets ────────────────────────────────────────────────► budget status / alert
```

## Ingestion

Two paths feed `usage_events`:

1. **Automatic scan.** `fleett serve` tails the last 2000 lines of every
   instance (`Driver::logs`) once at startup, then every
   `FLEETT_COST_SCAN_INTERVAL_S` seconds (default `300`). `0` disables the
   scanner; the API stays available.
2. **Explicit ingestion.** `POST /api/usage/ingest` accepts either raw log
   lines (`lines` or `text`) or the token counts directly
   (`prompt_tokens`, `completion_tokens`, `total_tokens`). This is what an
   agent, a test or an external collector uses.

The scan never fails the control plane: an unreachable container is counted in
the scan report and skipped.

### Recognised line shapes

The parser looks at every line independently. A leading RFC 3339 timestamp is
used as the event time (`docker compose logs` prefixes are skipped); without
one, the ingestion time is used.

| Shape | Example |
|---|---|
| OpenAI JSON usage | `{"model":"muse-spark","usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}` |
| Anthropic JSON usage | `{"model":"claude-sonnet-4","usage":{"input_tokens":100,"output_tokens":50}}` |
| Nested JSON payload | `INFO response: {"response":{"model":"m","usage":{...}}}` |
| Python repr (single quotes) | `usage={'prompt_tokens': 7, 'completion_tokens': 3}` |
| Plain key/value | `model=deepseek-v4.1-flash prompt_tokens=1200 completion_tokens=300 total_tokens=1500` |

Rules:

- `prompt_tokens` and `input_tokens` are synonyms, as are `completion_tokens`
  and `output_tokens`.
- `total_tokens` defaults to `prompt + completion`.
- A line where every count is zero, or without any token key, is ignored.
- Lines longer than 16 KiB are ignored (never truncated), so a stack trace
  cannot smuggle a partial number into the ledger.
- The model of the event is the one on the line; without one, the model of the
  instance is used.

### Idempotency

Each event carries `fingerprint = SHA-256(instance_id, exact line)`, unique in
`usage_events`. Re-scanning the same tail, replaying a request or ingesting the
same line twice inserts nothing the second time (`inserted: 0`,
`duplicates: 1`). An explicit event (`POST` with token counts) is fingerprinted
on its instance, timestamp, model and counts.

## Pricing

Prices live in `model_pricing`, in **currency per million tokens**:

```bash
fleett pricing set deepseek-v4.1-flash --input 0.27 --output 1.10
fleett pricing set claude-sonnet-4 --input 3 --output 15 --effective-from 2026-12-01
fleett pricing
```

`POST /api/models/pricing` body:

```json
{"model":"deepseek-v4.1-flash","input_per_mtok":0.27,"output_per_mtok":1.10,
 "currency":"USD","effective_from":"2026-11-01"}
```

`effective_from` is the first day the price applies to (`YYYY-MM-DD` or an RFC
3339 timestamp; default: today, UTC). The table is **versioned**:

- setting the same model and effective date again replaces that version;
- a new effective date adds a version (`PRIMARY KEY (model, effective_from)`);
- the cost of an event uses the latest version whose effective date is on or
  before the event **day** — a price change never rewrites yesterday's cost;
- `GET /api/models/pricing` lists only the latest version of each model;
- `DELETE /api/models/pricing/{model}` drops every version of that model.

An event whose model has no price costs `0` (tokens are still counted). The
`currency` is carried by the price rows and echoed in the report; totals are
**not converted** — a non-USD price is assumed to be in the same unit as the
budgets only if the operator uses it that way.

## Budgets

```bash
fleett budget alice --set 25     # 25 USD per month
fleett budget alice              # status, month-to-date and projection
```

A budget is a monthly ceiling in USD per instance
(`cost_budgets.instance_id`, `monthly_usd`, `enabled`). `PUT
/api/instances/{id}/budget` with `{"monthly_usd":25,"enabled":true}` sets it;
`enabled:false` keeps the value but disarms the alert. `monthly_usd: 0` means
"no ceiling" and never fires.

Status (`GET /api/instances/{id}/budget`, `GET /api/cost/budgets` and the
`budgets` array of `GET /api/cost`):

- `used_usd`: cost of the events since the first day of the current month
  (UTC), at the price effective on each event day;
- `percent`: `used / monthly_usd * 100`;
- `exceeded`: `enabled && monthly_usd > 0 && used >= monthly_usd`;
- `projected_month_usd`: `used / elapsed_fraction`, where
  `elapsed_fraction` is the share of the current month already elapsed
  (clamped to `0.001..=1`, so the first minutes of a month do not project
  absurd numbers).
- `projection` (top level of `GET /api/cost`) repeats the fleet-wide
  month-to-date and projection.

### Alert hook

`cost::check_budgets` reports a crossing **once per process and month**. It is
evaluated after every scan and after every successful ingestion, so an agent
that reports its own usage is checked immediately. The notification/incident
pipeline is not merged yet, so the alert goes through
`cost::events::emit("cost.budget", payload)`, which writes a structured WARN
line:

```
WARN fleett cost event: event="cost.budget" payload={"instance_id":"alice",
     "month":"2026-11","monthly_usd":25.0,"used_usd":25.4,"percent":101.6,...}
```

When the incident module lands, only the sink of `events::emit` has to change.
A budget that falls back below its limit (a price edit, a lower usage) re-arms
the alert for the next crossing.

## Retention

- `usage_events` is append-only. There is no automatic pruning yet: one event
  is a few hundred bytes, so a fleet producing thousands of requests a day
  stays in the megabyte range for years. Deleting an instance **keeps** its
  usage history (the rows are keyed by the id, not by a foreign key) so a
  deleted agent does not disappear from past reports.
- `cost_budgets` rows also survive an instance deletion; they are reused if the
  id is recreated.
- `model_pricing` keeps every version, because historical costs depend on them.
  `DELETE /api/models/pricing/{model}` is the explicit way to drop them.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_COST_SCAN_INTERVAL_S` | `300` | Seconds between two automatic log scans; `0` disables the scanner entirely (startup scan included). |

`POST /api/usage/ingest` limits: 5000 lines or 4 MiB of text per call, 16 KiB
per line, 128 characters per model name, prices and budgets up to `1e6`.

## API summary

See [API.md](../API.md#tokens-and-costs) for payloads.

| Method | Route |
|---|---|
| GET | `/api/cost?from=&to=&group_by=instance\|model\|day` |
| GET | `/api/cost/budgets` |
| POST | `/api/usage/ingest` |
| GET / POST | `/api/models/pricing` |
| DELETE | `/api/models/pricing/{model}` |
| GET / PUT | `/api/instances/{id}/budget` |
