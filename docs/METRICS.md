# Metrics — scraping fleett with Prometheus

The control plane exposes a Prometheus endpoint on a **dedicated listener**, not
on the API port. A scrape therefore never needs an API token, and the API port
stays closed to the monitoring stack.

```
GET http://127.0.0.1:9100/metrics
Content-Type: text/plain; version=0.0.4; charset=utf-8
```

The exposure text is rendered by the control plane itself (no client library, no
extra dependency): every metric below is a plain `# HELP` / `# TYPE` block
followed by one sample per line.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_METRICS_BIND` | `127.0.0.1:9100` | Listen address of the metrics listener. **Empty** (or `off`, `none`, `disabled`, `false`) disables it. |
| `FLEETT_METRICS_TOKEN` | *(unset)* | Bearer token a scrape must send. **Required** as soon as the bind is not a loopback address; the control plane refuses to start otherwise. |

```bash
# Local Prometheus on the same host, no token: the default.
FLEETT_METRICS_BIND=127.0.0.1:9100

# Scraped from another machine: the token becomes mandatory.
FLEETT_METRICS_BIND=10.0.0.5:9100
FLEETT_METRICS_TOKEN=<long random token>

# No metrics at all.
FLEETT_METRICS_BIND=
```

A non-loopback bind without a token fails at startup with:

```
FLEETT_METRICS_TOKEN is required when FLEETT_METRICS_BIND (0.0.0.0:9100) is not
a loopback address: without it anyone who can reach the port reads the fleet layout
```

The listener starts once the fleet state is loaded (just before the API
listener) and stops with the process: it is not tied to the API's graceful
shutdown. A failed bind is fatal — the operator asked for metrics, so a closed
port must not go unnoticed.

## Metrics

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `fleett_build_info` | gauge | `version` | Always `1`; the version is in the label. |
| `fleett_up` | gauge | — | Always `1` while the process answers a scrape. |
| `fleett_instances` | gauge | `status`, `health` | Number of instances per lifecycle status (`created`, `running`, `stopped`, `error`, `unknown`) and recorded container health (`healthy`, `unhealthy`, `none`). |
| `fleett_instance_cpu_percent` | gauge | `id` | CPU usage of one container, in percent of one core (`docker stats`). |
| `fleett_instance_mem_used_bytes` | gauge | `id` | Memory used by one container. |
| `fleett_instance_mem_limit_bytes` | gauge | `id` | Memory limit of one container. |
| `fleett_instance_disk_bytes` | gauge | `id` | Size of the instance data directory (`du`). |
| `fleett_api_requests_total` | counter | `method`, `status` | HTTP requests served by the API, per method and status code. |
| `fleett_compose_actions_total` | counter | `action`, `result` | Compose actions, per verb (`up`, `start`, `stop`, `restart`, `down`, `upgrade`, `rollback`) and result (`ok`, `error`). |
| `fleett_upgrade_jobs_total` | counter | `result` | Finished rolling upgrade jobs, per final result (`success`, `rolled_back`, `failed`). |
| `fleett_rate_limited_total` | counter | `class` | Requests refused by the API hardening, per class (`api`, `auth`, `heavy`, `lockout`). All four series are always exposed, at zero when nothing was refused. See [HARDENING.md](HARDENING.md#api-and-control-plane-hardening). |
| `fleett_usage_ok` | gauge | — | `1` when the last utilisation sample could be read from Docker, `0` otherwise (the per-instance usage series are then stale or absent). |

Notes:

- **Counters are per process.** A restart resets them; that is what Prometheus
  expects from a `_total` series (`rate()` handles the reset).
- **Utilisation is cached for 10 s.** A sample costs one `docker stats` plus one
  `du` per data directory, so a 15 s scrape interval hits Docker at most every
  10 s. Every lifecycle action (create, start, stop, restart, delete, upgrade,
  rollback) drops the cache, so a stop is visible on the next scrape.
- **`health` is what the control plane recorded.** An instance whose container
  health probe failed keeps `health="unhealthy"` until a probe answers again
  (this is the same information `last_error` carries in the API). A non-running
  instance reports `none`.
- **Unseen statuses keep their series** (`0`), so a dashboard query never turns
  empty when the last instance of a given status disappears.

## Prometheus scrape config

```yaml
# prometheus.yml
scrape_configs:
  - job_name: fleett
    scrape_interval: 15s
    scrape_timeout: 5s
    static_configs:
      - targets: ["127.0.0.1:9100"]

  # Non-loopback bind: the token is mandatory.
  # - job_name: fleett
  #   static_configs:
  #     - targets: ["10.0.0.5:9100"]
  #   authorization:
  #     type: Bearer
  #     credentials_file: /etc/prometheus/fleett-metrics.token
```

Check the output by hand before wiring Prometheus:

```bash
curl -fsS http://127.0.0.1:9100/metrics | promtool check metrics
# with a token:
curl -fsS -H "Authorization: Bearer $FLEETT_METRICS_TOKEN" \
  http://10.0.0.5:9100/metrics | promtool check metrics
```

`promtool check metrics` exits silently when the exposition is valid and prints
one line per problem otherwise.

## Grafana: three panels

**1. Fleet status (time series or stat, instant query).**

```promql
sum by (status, health) (fleett_instances)
```

One line per status/health pair. A panel that only shows `running` should use
`sum(fleett_instances{status="running"})`; the number of instances that need
attention is `sum(fleett_instances{health="unhealthy"})`.

**2. API error rate, in percent of the requests (time series).**

```promql
100 * (
  sum(rate(fleett_api_requests_total{status=~"5.."}[5m]))
  /
  clamp_min(sum(rate(fleett_api_requests_total[5m])), 0.001)
)
```

Alert on `> 5` for ten minutes. The denominator is clamped so a fleet with no
traffic draws `0` instead of a gap.

**3. Memory pressure of the fleet (time series or gauge).**

```promql
100 * (
  sum(fleett_instance_mem_used_bytes)
  /
  clamp_min(sum(fleett_instance_mem_limit_bytes), 1)
)
```

Pair it with `sum(fleett_compose_actions_total{result="error"})` (or its
`rate()`) to see whether the containers that are eating memory are also the ones
failing lifecycle actions.

## Security

- **Loopback by default.** `127.0.0.1:9100` is only reachable from the host
  running the control plane; a local Prometheus scrapes it without a token.
- **A public bind requires a token.** `FLEETT_METRICS_BIND=0.0.0.0:9100` (or any
  non-loopback address) without `FLEETT_METRICS_TOKEN` is a startup error. The
  token is compared in constant time and never logged (`MetricsConfig` redacts it
  in `Debug`).
- **What a scrape reveals.** Instance ids, statuses, resource usage and the API
  call volume. No secret, no environment variable, no compose file.
- **What it does not allow.** `/metrics` is read-only: there is no write verb on
  the metrics listener, and it has no route other than `GET /metrics`.
- **Reverse proxies.** If the port is published through a proxy, terminate TLS
  there and keep the token: the endpoint has no rate limit and no other
  protection.
