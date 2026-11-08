# Egress firewall — keeping agents off the LAN

An agent container joins the `fleett` bridge network. That network has no LAN
scoping by itself: from `172.18.0.0/16` (or whatever Docker picked) an agent can
reach every host the Docker daemon can reach — Proxmox `:8006`, OPNsense, the
bastion, Infisical, anything on `10.0.0.0/8`, `172.16.0.0/12` or
`192.168.0.0/16`. For a guest agent, that is the real risk — well before the
Docker socket, which is never mounted.

The control plane therefore relies on a **host firewall**, installed once by the
operator, plus a per-instance `allow_lan` flag:

| Network | Created | Joined by |
|---|---|---|
| `fleett` (restricted) | by the app | every instance |
| `fleett-lan` (unrestricted) | on demand, when an instance sets `allow_lan: true` | that instance only |

`DOCKER-USER` rules drop LAN destinations for the `fleett` subnet, while leaving
Internet egress (and the `fleett` subnet itself) reachable. `fleett-lan` is not
matched by those rules, so an `allow_lan` instance keeps full LAN access; the
API returns a warning in `warnings` for every such instance, and the console
shows it.

## Install

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

## Verify

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

## systemd: install before the service

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

## Remove

```bash
sudo fleett egress remove
```

Deletes every rule of `DOCKER-USER` that carries the `fleett-egress` comment
(whatever subnet it was installed for), then the marker file. Foreign rules are
untouched.

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
`fleett-egress` comment so `remove` still cleans it up.
