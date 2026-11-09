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
| `fleett_instance_health` | gauge | `instance` | `1` when the recorded container health is `healthy`, `0` otherwise (unhealthy, stopped or unknown). |
| `fleett_instance_restarts_total` | counter | `instance` | Restarts requested through the control plane, counted from the `instance.restart` audit entries. Docker-driven restarts (a crashloop) are not included. |
| `fleett_disk_quota_bytes` | gauge | `instance` | Per-instance disk quota in bytes; `0` until the quota feature (issue #47) is installed. |
| `fleett_disk_used_bytes` | gauge | `instance` | Data directory size measured against the quota; `0` when the instance has no quota. |
| `fleett_secrets_expiring_days` | gauge | `instance` | Days until the earliest secret of the instance expires. `-1` means the expiry is unknown (the current schema has no expiry column); instances without secrets are omitted. |
| `fleett_incidents_open` | gauge | — | Incidents the watchdog registry still reports as open; `0` when the registry (issue #32) is not installed. |
| `fleett_mcp_up` | gauge | `server` | `1` when an MCP server is running and its port answers the supervision probe, `0` otherwise. |
| `fleett_mcp_tools` | gauge | `server` | Tools discovered on an MCP server by the last successful `tools/list` (`0` before the first probe). |
| `fleett_mcp_servers` | gauge | `status`, `health` | Number of MCP servers per lifecycle status and recorded health; the instance status/health series are seeded at `0`. |
| `fleett_mcp_probes_total` | counter | `result` | MCP supervision probes, per result (`ok`, `error`); both series always exist. |
| `fleett_api_requests_total` | counter | `method`, `status` | HTTP requests served by the API, per method and status code. |
| `fleett_compose_actions_total` | counter | `action`, `result` | Compose actions, per verb (`up`, `start`, `stop`, `restart`, `down`, `recreate`, `upgrade`, `rollback`) and result (`ok`, `error`). |
| `fleett_upgrade_jobs_total` | counter | `result` | Finished rolling upgrade jobs, per final result (`success`, `rolled_back`, `failed`). |
| `fleett_upgrades_total` | counter | `status` | Same counter as `fleett_upgrade_jobs_total`, with the `status` label the alert rules use; the three statuses keep a `0` series before the first job. |
| `fleett_rollbacks_total` | counter | — | Rolling upgrade jobs that rolled at least one instance back. |
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
- **Label names differ on purpose.** The historical per-instance series use
  `id`; the series added for the alert bundle use `instance`
  (`fleett_instance_health`, `fleett_instance_restarts_total`,
  `fleett_disk_quota_bytes`, `fleett_disk_used_bytes`,
  `fleett_secrets_expiring_days`).
- **Optional features are detected at scrape time.** Incidents, disk quotas and
  secret expiry are read by inspecting `PRAGMA table_info` and catching errors.
  A missing table or column yields the stable fallback (`0`, empty series or
  `-1`) instead of a failed scrape, so the same binary works before and after
  issues #32 and #47 land.

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

## Grafana bundle: dashboard, alerts and demo stack

Everything lives in [`docs/grafana/`](grafana/):

| File | Purpose |
|---|---|
| [`fleett-dashboard.json`](grafana/fleett-dashboard.json) | Importable Grafana dashboard (schema version 39): fleet overview, CPU/RAM/disk per instance, API error rate, upgrades/rollbacks, quotas, incidents and secrets. |
| [`fleett-alerts.yaml`](grafana/fleett-alerts.yaml) | Prometheus alerting rules: `InstanceDown`, `InstanceUnhealthy`, `InstanceRestarting`, `DiskQuotaHigh`, `FleettScrapeFailing`, `UpgradeRolledBack`. |
| [`docker-compose.yaml`](grafana/docker-compose.yaml) | Prometheus + Grafana, with the datasource, dashboard and rules provisioned. |
| [`prometheus.yml`](grafana/prometheus.yml) | Scrape config and `rule_files` used by the demo stack. |
| [`grafana/provisioning/`](grafana/grafana/provisioning/) | Grafana datasource and dashboard providers. |

### One-command demo

```bash
cd docs/grafana
docker compose up -d
# Grafana:    http://localhost:3000  (anonymous viewer, admin / fleett)
# Prometheus: http://localhost:9090  (Status → Targets, Status → Alerts)
```

The demo scrape target defaults to `host.docker.internal:8090/metrics` and can be
overridden without editing a file:

```bash
FLEETT_METRICS_URL=host.docker.internal:9100 docker compose up -d
```

`FLEETT_METRICS_URL` is a `host:port` (no scheme). fleett's own default metrics
listener is `127.0.0.1:9100`; the demo defaults to `8090` because that is the
address the deployment exposes. A container cannot reach the host loopback, so
the control plane must bind a reachable address and, because that address is not
loopback, set a token:

```bash
FLEETT_METRICS_BIND=0.0.0.0:9100 \
FLEETT_METRICS_TOKEN="$(openssl rand -hex 32)" \
fleett
# Then, in the demo stack:
FLEETT_METRICS_URL=host.docker.internal:9100 \
FLEETT_METRICS_TOKEN="<the same token>" \
docker compose up -d
```

The compose entrypoint substitutes both values into a temporary
`/tmp/prometheus.yml`; the tracked file keeps placeholders so
`promtool check config` works without a live environment.

### Import the dashboard by hand

1. Grafana → **Dashboards** → **New** → **Import**.
2. Upload `docs/grafana/fleett-dashboard.json` (or paste it).
3. When Grafana asks for the **Prometheus** input (`DS_PROMETHEUS`), pick the
   datasource that scrapes fleett. The dashboard uses
   `datasource: {"type":"prometheus","uid":"${DS_PROMETHEUS}"}`, so the same
   JSON works against any Prometheus instance.
4. Save. The default time range is six hours, refreshed every 30 s.

With the demo stack the datasource is provisioned with UID `prometheus`, and
the dashboard's `DS_PROMETHEUS` variable defaults to it: no manual step.

### Validate the rules

```bash
promtool check rules docs/grafana/fleett-alerts.yaml
# Checking docs/grafana/fleett-alerts.yaml
#   SUCCESS: 6 rules found
```

`promtool check config docs/grafana/prometheus.yml` cannot run from the repo
root as-is: `rule_files` points at `/etc/prometheus/fleett-alerts.yaml`, the path
the demo container mounts. Check both files in the image instead:

```bash
docker run --rm --entrypoint promtool \
  -v "$PWD/docs/grafana/prometheus.yml":/work/prometheus.yml:ro \
  -v "$PWD/docs/grafana/fleett-alerts.yaml":/etc/prometheus/fleett-alerts.yaml:ro \
  prom/prometheus:v2.55.1 check config /work/prometheus.yml
#   SUCCESS: 1 rule files found
#   SUCCESS: /work/prometheus.yml is valid prometheus config file syntax
#   SUCCESS: 6 rules found
```

CI can run the same command when `promtool` is available, and the rule file is a
plain YAML list (`groups[].rules[]`, each with `alert`, `expr`, `for`,
`labels.severity`, `annotations.summary` and `annotations.description`), so a
small Python `yaml.safe_load` check can gate a runner without the binary.

The rules and what they mean:

| Alert | Expression (short) | `for` | Severity |
|---|---|---|---|
| `InstanceDown` | `up{job="fleett"} == 0` | 2m | critical |
| `InstanceUnhealthy` | `fleett_instance_health{instance=~".+"} == 0` | 5m | warning |
| `InstanceRestarting` | `increase(fleett_instance_restarts_total[15m]) > 0` | 0m | warning |
| `DiskQuotaHigh` | disk used / quota > 90 %, quota > 0 | 10m | warning |
| `FleettScrapeFailing` | `fleett_usage_ok == 0` or `up{job="fleett"} == 0` | 5m | critical |
| `UpgradeRolledBack` | `increase(fleett_rollbacks_total[1h]) > 0` | 0m | warning |

`InstanceUnhealthy` reports any instance that is not currently healthy,
including one that was stopped on purpose. Silence it for a planned stop, or
use the `fleett_instances{status="running"}` aggregate to scope a dashboard: the
per-instance lifecycle status is not a series yet.

### Queries behind the panels

Fleet status and unhealthy count:

```promql
sum by (status, health) (fleett_instances)
sum(fleett_instances{health="unhealthy"})
```

CPU, memory and disk per instance:

```promql
fleett_instance_cpu_percent
fleett_instance_mem_used_bytes
fleett_instance_disk_bytes
```

API error rate, in percent of the requests:

```promql
100 * (
  sum(rate(fleett_api_requests_total{status=~"5.."}[5m]))
  /
  clamp_min(sum(rate(fleett_api_requests_total[5m])), 0.001)
)
```

Upgrades and rollbacks:

```promql
sum by (status) (increase(fleett_upgrades_total[$__rate_interval]))
increase(fleett_rollbacks_total[24h])
```

Quota usage (silent until issue #47 lands, because the denominator is 0):

```promql
100 * fleett_disk_used_bytes / on(instance) (fleett_disk_quota_bytes > 0)
```

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
