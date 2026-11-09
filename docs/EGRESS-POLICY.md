# Per-instance egress policy

The binary model (`allow_lan: true|false`) answers "may this agent see the LAN?".
It does not answer the question operators actually ask: *"this agent needs
`api.github.com` and nothing else"*. A destination policy adds that answer.

A policy is stored per instance, rendered into the agent container as
`HTTP_PROXY` / `HTTPS_PROXY`, and enforced by an **external** proxy the operator
runs. fleett owns the model, the validation, the API and the block log; it does
not deploy, supervise or verify the proxy ACLs. That split is deliberate and the
gaps are listed at the end of this page.

## Model

```json
{
  "mode": "deny",
  "fqdns": ["api.github.com", "*.example.com"],
  "cidrs": ["10.1.0.0/16"],
  "ports": [443]
}
```

| Field | Meaning |
|---|---|
| `mode` | `deny` = default-deny allowlist (empty policy denies everything). `allow` = default-allow blocklist (empty policy allows everything). |
| `fqdns` | Exact hosts, lowercased. A leading `*.` matches subdomains only (`*.example.com` does **not** match `example.com`). |
| `cidrs` | IPv4 or IPv6 ranges, `ip/prefix`. |
| `ports` | Optional destination ports. Empty = every port. A populated list narrows the exception: in `deny` mode the listed destinations are allowed only on those ports; in `allow` mode they are blocked only on those ports. |

Validation, enforced by `PUT /api/instances/{id}/egress`:

- at most 64 entries per list;
- FQDN: lowercased, trailing dot removed, at most 253 characters, labels
  `[a-z0-9_-]` (no scheme, path, port or space); `*` only as a leading `*.`;
- CIDR: a valid IP and a prefix within `32` (v4) or `128` (v6);
- ports: `1..=65535`, deduplicated and sorted.

Lists are sorted and deduplicated before they are stored, so the same policy
always renders the same compose file.

### Compatibility

An instance **without** a policy keeps exactly the historical behaviour:

| Legacy state | Reported `source` | Effective decision |
|---|---|---|
| no policy, `allow_lan: true` | `allow_lan` | allow everything (the instance joins the unrestricted `<network>-lan` bridge) |
| no policy, `allow_lan: false`, `FLEETT_EGRESS=restrict` | `legacy` | allow everything except `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` (the host `DOCKER-USER` rules) |
| no policy, `FLEETT_EGRESS=off` | `disabled` | allow everything, nothing is filtered |

Those projections exist so the console and the test endpoint have one model;
nothing is written to the database and no compose file changes until an explicit
policy is set. `allow_lan` and `FLEETT_EGRESS` stay accepted and documented as
shortcuts.

An explicit policy wins over both shortcuts, but it does not detach the
instance from `fleett-lan`: an `allow_lan: true` instance can still bypass the
proxy. The API returns a warning and the test endpoint reports
`enforced: false` in that case.

An instance archive (`GET /api/instances/{id}/export`) does not carry the
policy yet: re-apply it with `fleett egress set` after an import.

## Render

When a policy exists, the generated compose file gains three variables:

```yaml
environment:
  HTTP_PROXY: http://fleett-egress-proxy:3128
  HTTPS_PROXY: http://fleett-egress-proxy:3128
  NO_PROXY: localhost,127.0.0.1,::1,.local,fleett
```

The address comes from `FLEETT_EGRESS_PROXY` (default
`http://fleett-egress-proxy:3128`). An empty value, `off`, `none` or `disabled`
suppresses the variables: the policy is stored but nothing points at a proxy,
which the API reports as a warning instead of writing a dead address.

Instances without a policy never gain those variables: their compose bytes are
unchanged by this feature. A `PATCH`, an env change, an import or a rolling
upgrade re-renders the compose file and keeps the policy applied.

`NO_PROXY` keeps loopback, `.local` and the `fleett` bridge direct so
agent-to-agent traffic and the gate do not depend on the proxy. A gate with
another DNS name must be added to the proxy ACLs (and, when it is reached by
name, to `NO_PROXY` by the operator).

## Deploy the proxy

`docker/fleett-proxy.example.yaml` runs Squid on the `fleett` network under the
container name `fleett-egress-proxy`:

```bash
cd docker
cp fleett-proxy.domains.example.txt fleett-proxy.domains.txt
cp fleett-proxy.cidrs.example.txt fleett-proxy.cidrs.txt
# edit both allowlists, then:
docker compose -f fleett-proxy.example.yaml up -d
```

The shipped Squid configuration (`fleett-proxy.squid.conf`) is an allowlist:

```squid
http_port 3128
acl fleett_domains dstdomain "/etc/squid/fleett-domains.txt"
acl fleett_nets dst "/etc/squid/fleett-cidrs.txt"
acl fleett_ports port 80 443
http_access allow fleett_domains fleett_ports
http_access allow fleett_nets fleett_ports
http_access deny all
```

Generate the allowlist files from the API. `GET /api/instances/{id}/egress`
returns the effective lists and, for an explicit policy, a ready-to-copy
`proxy_acl` snippet naming `/etc/squid/fleett-<id>.domains` and
`/etc/squid/fleett-<id>.cidrs`:

```bash
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/instances/alice/egress" | jq -r '.fqdns[]' \
  > fleett-proxy.domains.txt
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/instances/alice/egress" | jq -r '.cidrs[]' \
  > fleett-proxy.cidrs.txt
docker compose -f docker/fleett-proxy.example.yaml restart fleett-proxy
```

The proxy must be on the `fleett` network (external network `fleett`, created
by fleett) for the container name to resolve. It must **not** publish `3128` on
the host.

## API

| Method | Route | Body | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/egress` | — | effective policy: `source`, `mode`, `fqdns`, `cidrs`, `ports`, `proxy`, `proxy_acl` (explicit only), `warnings` |
| PUT | `/api/instances/{id}/egress` | `{"mode","fqdns","cidrs","ports"}` | validate, store, rewrite the compose file, restart; `400 invalid_egress_policy`; audited as `egress.policy.update` |
| DELETE | `/api/instances/{id}/egress` | — | drop the explicit policy, return to the legacy shape; `404` when none; `egress.policy.delete` |
| POST | `/api/instances/{id}/egress/test` | `{"host":"api.github.com","port":443}` | policy decision plus enforcement state (below) |
| GET | `/api/instances/{id}/egress/blocks?limit=200` | — | refused destinations, newest first; `limit` 1..=1000 |
| DELETE | `/api/instances/{id}/egress/blocks` | — | `{"removed":N}`; audited as `egress.blocks.clear` |
| GET | `/api/egress/blocks?instance=&limit=` | — | admin view across the fleet |
| POST | `/api/egress/blocks` | `{"instance","host","port","reason?"}` | proxy ingestion; `201`, rate-limited, token required |

`POST …/egress/test` never fakes enforcement:

```json
{
  "instance": "alice",
  "host": "api.github.com",
  "port": 443,
  "allowed": true,
  "reason": "fqdn api.github.com matches the deny-mode allowlist on port 443",
  "source": "explicit",
  "mode": "deny",
  "enforced": false,
  "enforcement": "proxy 127.0.0.1:9 is unreachable: Connection refused (os error 111)",
  "proxy": "http://127.0.0.1:9"
}
```

`allowed` is the **policy decision**. `enforced` is true only when:

- an explicit policy exists, the instance is not on `fleett-lan`, and a TCP
  connection to the proxy succeeds — the message says the probe checks the
  socket only, not the ACLs;
- or no explicit policy exists, `FLEETT_EGRESS=restrict`, and the host firewall
  marker matches the current `fleett` subnet.

`FLEETT_EGRESS=off`, `allow_lan: true`, a missing marker, an empty
`FLEETT_EGRESS_PROXY` or an unreachable proxy all answer `enforced: false` with
the reason.

## Block log

The proxy (or its log shipper) posts one record per refused destination:

```bash
curl -fsS -X POST "$FLEETT_URL/api/egress/blocks" \
  -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"instance":"alice","host":"10.1.20.1","port":443,"reason":"denied by ACL"}'
```

- authentication: bearer token or session; a `viewer` token is read-only and
  cannot ingest; unauthenticated calls answer `401`;
- the instance must exist (`404` otherwise);
- `host` is an FQDN or IP literal (no scheme), at most 253 bytes; `port` is
  `1..=65535`; `reason` is optional, at most 512 bytes;
- **rate limit**: 120 records per instance per minute, sliding window, answered
  `429 rate_limited` beyond that. The budget of one instance never affects
  another.
- the log is deleted with its instance.

Nothing in this repository tails a proxy log: with the shipped Squid example,
the operator needs a small shipper, or a proxy wrapper that calls the route on
`ERR_ACCESS_DENIED`. Until one is deployed the block list stays empty — that is
an honest gap, not a hidden failure.

## Enforcement limits (read this before trusting a policy)

1. **fleett does not run the proxy.** `docker/fleett-proxy.example.yaml` is an
   operator artifact. If it is not running, `HTTP_PROXY` points at a dead name
   and nothing is enforced.
2. **One shared proxy cannot distinguish instances.** The rendered variables
   are identical for every instance, so the proxy only sees the source IP; the
   ACL files are therefore a union unless the operator runs one proxy per
   instance or a proxy that authenticates each agent. Docker container IPs are
   not stable enough for `src` ACLs. Per-instance ACL files and the `proxy_acl`
   snippet are provided for the per-instance deployment, not for a magic
   shared one.
3. **`HTTP_PROXY` is advisory.** An agent controls its process environment and
   can unset it or open a raw socket. Actual enforcement requires host rules
   that allow the `fleett` subnet to reach only the proxy and drop other
   Internet egress. fleett does **not** install those rules: its
   `fleett egress install` only drops the RFC 1918 ranges (see
   [EGRESS.md](EGRESS.md)). The operator must add and persist, in `DOCKER-USER`,
   before the LAN drop rules:

   ```bash
   iptables -I DOCKER-USER -m comment --comment fleett-egress-proxy -s <fleett-subnet> -d <proxy-ip> -j ACCEPT
   iptables -I DOCKER-USER -m comment --comment fleett-egress-proxy -s <fleett-subnet> -j DROP
   ```

   Order matters and these rules are not written by fleett; test them on a
   staging host before relying on the policy for isolation.
4. **`allow_lan: true` defeats the proxy.** The instance joins `fleett-lan` and
   reaches the LAN directly. The policy is still stored, but the API warns and
   the test endpoint reports `enforced: false`.
5. **DNS is not filtered by the policy.** Name resolution can still reach a
   DNS server; only proxied connections are subject to the ACLs. A strict host
   setup should append the DNS rule above or run a resolver behind the proxy.
6. **IPv6 is out of scope**, like the rest of `docs/EGRESS.md`: the rendered
   proxy variables apply to both families, but the host firewall rules are v4
   only.
7. **The probe checks reachability, not correctness.** A reachable proxy with
   the wrong ACLs still answers `enforced: true`; the message says so.

The feature is useful before those gaps are closed: it records intent, renders
the variables, gives the console and the CLI a concrete decision, and logs the
refusals once a shipper exists. It must not be sold as network isolation on its
own.

## CLI

```bash
fleett egress policy alice
fleett egress set alice --mode deny --fqdn api.github.com --port 443
fleett egress set alice --mode allow --cidr 10.0.0.0/8 --port 22
fleett egress blocks alice
fleett egress blocks alice --clear
fleett --json egress policy alice
```

`set` replaces the whole policy: omitted lists become empty. `--fqdn`, `--cidr`
and `--port` are repeatable. See [CLI.md](CLI.md).
