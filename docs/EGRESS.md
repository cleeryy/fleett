# Egress firewall and proxy — keeping agents off the LAN

An agent container joins the `fleett` bridge network. That network has no LAN
scoping by itself: from `172.18.0.0/16` (or whatever Docker picked) an agent can
reach every host the Docker daemon can reach — Proxmox `:8006`, OPNsense, the
bastion, Infisical, anything on `10.0.0.0/8`, `172.16.0.0/12` or
`192.168.0.0/16`. For a guest agent, that is the real risk — well before the
Docker socket, which is never mounted.

Egress is controlled by two independent layers:

| Layer | Set by | Enforced by | Scope |
|---|---|---|---|
| LAN switch | `allow_lan` + `FLEETT_EGRESS` | host `DOCKER-USER` rules | "can this agent reach private ranges?" |
| Destination policy | `PUT /api/instances/{id}/egress` | an **operator-run** Squid proxy | "which FQDNs/CIDRs may this agent reach?" |

An instance **without** an explicit policy keeps the historical behaviour: it
joins `fleett`, and the host firewall drops the three RFC 1918 ranges. That is
reported as `policy: legacy` (or `none` when `FLEETT_EGRESS=off`, `legacy` when
`allow_lan` is on) by `GET /api/egress/summary`, and the console shows a grey
badge explaining it. A destination policy is only in force once its proxy is
deployed: `PUT` now refuses (`422 proxy_unreachable`) to point a container at a
proxy that does not answer.

## Two networks

| Network | Created | Joined by |
|---|---|---|
| `fleett` (restricted) | by the app | every instance |
| `fleett-lan` (unrestricted) | on demand, when an instance sets `allow_lan: true` | that instance only |

`DOCKER-USER` rules drop LAN destinations for the `fleett` subnet, while leaving
Internet egress (and the `fleett` subnet itself) reachable. `fleett-lan` is not
matched by those rules, so an `allow_lan` instance keeps full LAN access; the
API returns a warning in `warnings` for every such instance, and the console
shows it.

## Install the host firewall

```bash
sudo fleett egress install
```

The command:

1. reads the real subnet of the `fleett` network with
   `docker network inspect fleett` (IPv4 only);
2. appends the rules below to `DOCKER-USER`, **each tagged with
   `-m comment --comment fleett-egress`**, skipping the ones already present
   (`iptables -C`), so it is idempotent;
3. writes `/etc/fleett/egress.json` (mode 0644) with the subnet it installed
   for, the timestamp and the fleett version.

Rules, in evaluation order (order matters):

```bash
iptables -A DOCKER-USER -m comment --comment fleett-egress -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A DOCKER-USER -m comment --comment fleett-egress -s <subnet> -d <subnet> -j ACCEPT
iptables -A DOCKER-USER -m comment --comment fleett-egress -s <subnet> -d 10.0.0.0/8 -j DROP
iptables -A DOCKER-USER -m comment --comment fleett-egress -s <subnet> -d 172.16.0.0/12 -j DROP
iptables -A DOCKER-USER -m comment --comment fleett-egress -s <subnet> -d 192.168.0.0/16 -j DROP
```

`fleett egress install` needs root (or `CAP_NET_ADMIN`) and access to the Docker
socket, which the `fleett` service user already has. Run it again after the
`fleett` network is recreated: its subnet may change.

> The firewall commands are Linux-only (`iptables`, `DOCKER-USER`). On macOS the
> binary still serves the control plane with `FLEETT_DRIVER=mock`, but
> `install`, `status` and `remove` report a privilege/platform error.

Print the exact commands without running them (no privileges needed beyond the
Docker socket):

```bash
fleett egress print
```

## Verify the host firewall

```bash
sudo fleett egress status   # exit 0 only when all five rules are present
```

`status` compares the rules against the current subnet and prints the marker
state:

```
network: fleett (172.18.0.0/16)
marker:  /etc/fleett/egress.json (installed 2026-11-08T16:34:00Z by fleett 0.2.0)
  ok      iptables -A DOCKER-USER … -d 10.0.0.0/8 -j DROP
  …
egress firewall: enforced
```

From inside a guest instance (the acceptance test of issue #3), with
`allow_lan: false`:

```bash
# must fail: the firewall drops LAN destinations
docker compose -f /opt/fleett/instances/<id>/compose.yaml exec agent \
  nc -z 10.1.20.1 443; echo "exit=$?"      # non-zero

# must pass: Internet egress is not filtered
docker compose -f /opt/fleett/instances/<id>/compose.yaml exec agent \
  curl -fsS https://example.com >/dev/null; echo "exit=$?"   # 0
```

Agents on the same `fleett` subnet can still reach each other and the gate
(rule 2): that is how `api_key_env` / `HERMES_GATE_PROFILE_TOKEN` routing works.

## Deploy the egress proxy (complete recipe)

fleett does **not** ship or supervise the proxy; it renders `HTTP_PROXY`,
`HTTPS_PROXY` and `NO_PROXY` into the agent container and probes that the socket
answers. Deploy the proxy once per Docker host, before writing a policy that
needs it (otherwise `PUT …/egress` answers `422 proxy_unreachable`).

The repository ships everything in `docker/`:

| File | Purpose |
|---|---|
| `fleett-proxy.example.yaml` | Compose service `fleett-proxy`, container `fleett-egress-proxy`, port 3128, on the external `fleett` network |
| `fleett-proxy.squid.conf` | Squid configuration, an allowlist reading the two files below |
| `fleett-proxy.domains.example.txt` | FQDN allowlist template |
| `fleett-proxy.cidrs.example.txt` | CIDR allowlist template |

### 1. Prepare the files

From the repository root (the same checkout the fleett binary was built from, or
any copy of `docker/`):

```bash
cd docker
cp fleett-proxy.domains.example.txt fleett-proxy.domains.txt
cp fleett-proxy.cidrs.example.txt   fleett-proxy.cidrs.txt
```

`fleett-proxy.example.yaml` mounts exactly those two names. It also mounts
`fleett-proxy.squid.conf` read-only and keeps its cache in a named volume. The
proxy publishes **no host port** on purpose: only containers on the `fleett`
bridge reach `fleett-egress-proxy:3128`.

### 2. Generate the allowlists from the API

`GET /api/egress/summary` returns one row per instance: the coarse `policy`
state, the effective `fqdns`, `cidrs`, `ports`, the rendered `proxy`, and
`proxy_reachable`. The shared proxy reads a **fleet-wide union** (one Squid
cannot tell instances apart; per-instance isolation is discussed in
[EGRESS-POLICY.md](EGRESS-POLICY.md)).

```bash
export FLEETT_URL=https://fleett.example.test
export FLEETT_TOKEN=…                      # admin token

# FQDNs of every enforced policy. Squid spells a subdomain wildcard ".example.com".
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" "$FLEETT_URL/api/egress/summary" \
  | jq -r '[.instances[] | select(.policy == "enforced") | .fqdns[]] | unique[]' \
  | sed 's/^\*\././' > fleett-proxy.domains.txt

# CIDRs of every enforced policy.
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" "$FLEETT_URL/api/egress/summary" \
  | jq -r '[.instances[] | select(.policy == "enforced") | .cidrs[]] | unique[]' \
  > fleett-proxy.cidrs.txt
```

Inspect both files before starting the proxy: the union is what every agent will
be allowed to reach. If you need a per-instance exception, deploy one proxy per
instance and point that instance's policy `proxy` field at it.

```bash
# Per-instance view of what each enforced policy allows.
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" "$FLEETT_URL/api/egress/summary" \
  | jq -r '.instances[] | select(.policy == "enforced")
           | "\(.instance): " + ((.fqdns + .cidrs) | join(", "))'
```

`fleett-proxy.squid.conf` only accepts ports 80 and 443 (`acl fleett_ports`).
A policy that lists another port is accepted by the API but refused by the
shared proxy: add the port to `fleett_ports` in `fleett-proxy.squid.conf` (and
to `Safe_ports`) and restart the proxy.

### 3. Start the proxy

```bash
cd docker
docker compose -f fleett-proxy.example.yaml up -d
docker compose -f fleett-proxy.example.yaml ps       # fleett-egress-proxy Up
docker compose -f fleett-proxy.example.yaml logs --tail=50
```

`fleett` must already have created the external network (start the service and
create one instance, or `docker network create fleett`); Compose refuses a
missing external network instead of creating it.

### 4. Verify reachability

From the **host** (the control plane is a systemd unit outside the bridge, so
it resolves the container with `docker inspect`; do the same by hand):

```bash
PROXY_IP=$(docker inspect -f \
  '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' fleett-egress-proxy \
  | awk '{print $1}')
echo "$PROXY_IP"
nc -zv "$PROXY_IP" 3128          # "open" / "succeeded"
```

From an **agent container** on the `fleett` bridge, by service name:

```bash
docker compose -f /opt/fleett/instances/<id>/compose.yaml exec agent \
  sh -c 'nc -z fleett-egress-proxy 3128; echo "exit=$?"'      # exit=0
```

An HTTP check through the proxy (choose a host that is in the allowlist):

```bash
PROXY_IP=$(docker inspect -f \
  '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' fleett-egress-proxy \
  | awk '{print $1}')
curl -fsS -x "http://$PROXY_IP:3128" https://api.github.com/ -o /dev/null \
  && echo "proxy allows api.github.com"
```

Ask fleett the same question for every instance:

```bash
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/egress/summary" \
  | jq -r '.instances[] | "\(.instance)\t\(.policy)\t\(.proxy // "-")\t\(.proxy_reachable)"'
```

`proxy_reachable` is `null` when no proxy is rendered (legacy/`none` rows),
`true` when the socket answers, `false` with `proxy_error` when it does not.
`fleett doctor` / `GET /api/doctor` reports the same state as the
`egress-policy` check: `ok` when no policy needs a proxy, `error` when one
renders an unreachable proxy.

### 5. Host firewall for the proxy port

The five `fleett-egress` rules above drop LAN destinations but leave Internet
egress open; they do **not** force traffic through the proxy. For real
enforcement (an agent that unsets `HTTP_PROXY` must not escape), add, **before**
the LAN DROP rules, in `DOCKER-USER`:

```bash
PROXY_IP=$(docker inspect -f \
  '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' fleett-egress-proxy \
  | awk '{print $1}')
SUBNET=$(docker network inspect fleett -f \
  '{{range .IPAM.Config}}{{.Subnet}}{{end}}')

# The proxy itself keeps Internet access (so it can relay).
iptables -I DOCKER-USER -m comment --comment fleett-egress-proxy \
  -s "$PROXY_IP" -j ACCEPT
# Agents may reach only the proxy...
iptables -I DOCKER-USER -m comment --comment fleett-egress-proxy \
  -s "$SUBNET" -d "$PROXY_IP" -p tcp --dport 3128 -j ACCEPT
# ...and nothing else on the Internet.
iptables -I DOCKER-USER -m comment --comment fleett-egress-proxy \
  -s "$SUBNET" -j DROP
```

Order matters: the ACCEPT lines must be inserted before the final DROP (each
`-I` inserts at the top, so run them in reverse or check with
`iptables -L DOCKER-USER -n --line-numbers`). DNS resolution still needs a
resolver; allow it explicitly if the agents resolve names themselves (the proxy
resolves the destinations it relays). These rules are **not** installed by
fleett and are not tested by `fleett egress status`; validate them on a staging
host before relying on them for isolation.

Make the rules persistent with your distribution's mechanism (for example
`iptables-persistent`), or add them to `fleett-egress.service` after the
`fleett egress install` line.

## systemd: install the host firewall before the service

Rules do not survive a reboot. Install them at every boot, before `fleett`
starts, with a oneshot unit. It shares the service environment file so it sees
the same `FLEETT_NETWORK` / `FLEETT_EGRESS_MARKER`.

`/etc/systemd/system/fleett-egress.service`:

```ini
[Unit]
Description=fleett egress firewall (DOCKER-USER rules for the fleett network)
After=docker.service network-online.target
Wants=network-online.target
Requires=docker.service
Before=fleett.service

[Service]
Type=oneshot
RemainAfterExit=yes
EnvironmentFile=-/etc/fleett.env
ExecStart=/usr/local/bin/fleett egress install
# optional: ExecStop=/usr/local/bin/fleett egress remove
StandardOutput=journal

[Install]
WantedBy=multi-user.target
```

Then make the control plane depend on it — add to the `[Unit]` section of
`/etc/systemd/system/fleett.service`:

```ini
Wants=fleett-egress.service
After=fleett-egress.service
```

```bash
systemctl daemon-reload
systemctl enable --now fleett-egress.service
systemctl restart fleett.service          # or: systemctl enable --now fleett.service
sudo fleett egress status
```

`fleett-egress.service` starts before `fleett.service`; if it fails, the control
plane still starts, warns in the journal, and refuses to create restricted
instances (see below) until the firewall is installed.

## Enforcement policy

`FLEETT_EGRESS` (also `FLEET_EGRESS`):

| Value | Behaviour |
|---|---|
| `restrict` (default) | creating or patching an instance with `allow_lan: false` requires a marker at `FLEETT_EGRESS_MARKER` whose subnet matches the current `fleett` subnet. Missing, unreadable or stale marker → `409 egress_not_enforced` with `error.details.{egress,marker}`. |
| `off` | no check; a startup warning and a `warnings` entry on every instance state that the firewall is not enforced. |

`allow_lan: true` always passes, with a warning in the instance detail. Start,
stop, restart and delete are **never** blocked: an instance that already exists
keeps being controllable when the firewall is down.

With the `mock` driver the current subnet is unknown: the marker is then trusted
as-is instead of being compared.

`FLEETT_EGRESS_MARKER` overrides the marker path (default
`/etc/fleett/egress.json`); keep it in sync between the CLI and the service.

## Per-instance destination policy

The firewall above is a binary LAN switch. For "this agent may reach
`api.github.com` and nothing else", set a destination policy:

```bash
fleett egress set alice --mode deny --fqdn api.github.com --port 443
fleett egress policy alice
fleett egress blocks alice
fleett egress unset alice   # only DELETE removes a policy
```

A policy is stored in `egress_policies`, rendered as
`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` in the agent container and enforced by the
proxy you deployed above. Only a policy that decides something the host firewall
does not renders those variables; a blocklist of RFC 1918 ranges (or an empty
`allow` policy) is legacy-equivalent and gets none. `allow_lan` and
`FLEETT_EGRESS` remain shortcuts: an instance without a policy is unchanged. See
[EGRESS-POLICY.md](EGRESS-POLICY.md) for the model, the API, the block log and
the enforcement gaps (`HTTP_PROXY` is advisory, and one shared proxy cannot
apply a different allowlist per instance).

Saving a policy that renders a proxy is **refused with `422 proxy_unreachable`**
when that proxy does not answer on its TCP port, before the row, the compose
file or the container change. Deploy or restart the proxy, then retry. Every
other recreation path (instance patch, env change, clone, import, share change)
applies the same guard.

## Troubleshooting

### `422 proxy_unreachable` when saving a policy

The controller just probed the proxy URL the policy would render. It failed
within 750 ms. The exact deploy command is in `error.details.hint`:

```bash
docker compose -f docker/fleett-proxy.example.yaml up -d
```

Check, in order:

1. `docker compose -f docker/fleett-proxy.example.yaml ps` — is
   `fleett-egress-proxy` running?
2. `docker network inspect fleett` — does the proxy container list that network?
3. the network address is resolved with
   `docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' fleett-egress-proxy`
   then a TCP connect to `<ip>:3128`. From the host, `nc -zv <ip> 3128`.
4. the service user must reach the Docker socket; if `docker inspect` fails, the
   host cannot translate the service name and the socket probe fails too. Point
   `FLEETT_EGRESS_PROXY` (or the per-policy `proxy`) at the container's IP, or
   fix the service user's Docker access (`sudo usermod -aG docker fleett`).
5. a URL with a non-default port: `http://host:3128`; a scheme-less value is
   rejected (`400 invalid_egress_policy`).

The probe checks the socket only. A proxy that answers but has the wrong ACLs is
accepted; `POST /api/instances/{id}/egress/test` says so explicitly.

### `fleett doctor` reports `egress-policy: error`

The detail names the instance and proxy URL. It is an error only when a policy
that renders a proxy is stored and that proxy is unreachable: either deploy it
(above) or remove the policy (`DELETE /api/instances/{id}/egress`). No explicit
policy anywhere is `ok` with a detail saying only the LAN filter applies
(`legacy`). A policy that needs a proxy but has no address is a `warn`.

### The proxy is up but every agent connection fails

- `docker compose … logs fleett-proxy` and the Squid access log: refusals mean
  the destination is not in the allowlist files. Regenerate them from
  `GET /api/egress/summary` and restart the proxy.
- Squid reads `fleett-proxy.domains.txt`/`.cidrs.txt` at request time; a changed
  file needs `docker compose -f fleett-proxy.example.yaml restart fleett-proxy`
  only if Squid caches ACLs (`acl … -n` in the config), otherwise a reload is
  enough.
- Ports other than 80/443 are denied by `fleett-proxy.squid.conf`'s
  `fleett_ports` ACL.
- `NO_PROXY` keeps loopback, `.local` and the `fleett` bridge direct; a gate on
  another name must be added there by the operator.
- `allow_lan: true` joins `fleett-lan`, which bypasses the proxy entirely; set
  it to `false` (the API warns and the test endpoint reports `enforced: false`).

### The console/API says `legacy` for every instance

That is the normal state when no explicit policy is stored: only the host LAN
filter applies. Set a policy (`fleett egress set …` or the console) to get
`enforced`.

## Remove

```bash
sudo fleett egress remove
```

Deletes every rule of `DOCKER-USER` that carries the `fleett-egress` comment
(whatever subnet it was installed for), then the marker file. Foreign rules are
untouched. The proxy-port rules tagged `fleett-egress-proxy` are **not** removed
by that command; delete them with `iptables -D` under the same comment.

Stop and remove the proxy (its cache volume is kept; remove it explicitly to
erase it):

```bash
cd docker
docker compose -f fleett-proxy.example.yaml down
# docker compose -f fleett-proxy.example.yaml down -v   # also drops squid-cache
```

Then delete the generated allowlists if they are no longer needed:

```bash
rm fleett-proxy.domains.txt fleett-proxy.cidrs.txt
```

> If `iptables-persistent` (or `netfilter-persistent`) snapshots the ruleset at
> shutdown, exclude `DOCKER-USER`: Docker recreates its own chains at startup
> and the saved copy would then own stale `fleett-egress` rules. Let
> `fleett-egress.service` install them instead.

## IPv6 caveat

These rules are **IPv4 only**. If the Docker daemon has IPv6 enabled
(`/etc/docker/daemon.json` with `"ipv6": true`), or an agent container gets an
IPv6 address, the equivalent destinations (`fc00::/7` unique-local addresses,
link-local `fe80::/10`, and the host's global IPv6) remain reachable. Keep the
`fleett` network IPv4-only (the default: `docker network create fleett` creates
no IPv6 subnet), and check with:

```bash
docker network inspect fleett --format '{{json .EnableIPv6}}'   # false
docker compose -f /opt/fleett/instances/<id>/compose.yaml exec agent \
  sh -c 'ip -6 addr show scope global'                          # no output
```

If you do need IPv6 on that bridge, add the same five rules with `ip6tables`
using the network's IPv6 subnet, and extend `fleett egress install` before
relying on it.

## Gate exception

Rule 2 accepts traffic inside the `fleett` subnet on purpose: agents talk to
each other and to the gate (`hermes-gate` / the OpenAI-compatible proxy they
point at with `base_url`). It only covers destinations **inside that bridge
subnet** — it grants no access to any host network. An agent that must reach the
gate on the host instead of inside the bridge needs its own explicit
`-d <gate-ip> -j ACCEPT` rule placed *before* the DROP lines, with the
`fleett-egress` comment so `remove` still cleans it up. If you add the
`fleett-egress-proxy` default-drop rules above, that accept must come first too.
