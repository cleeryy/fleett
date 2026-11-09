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
  "ports": [443],
  "proxy": "http://fleett-egress-proxy:3128"
}
```

| Field | Meaning |
|---|---|
| `mode` | `deny` = default-deny allowlist (empty policy denies everything). `allow` = default-allow blocklist (empty policy allows everything). |
| `fqdns` | Exact hosts, lowercased. A leading `*.` matches subdomains only (`*.example.com` does **not** match `example.com`). |
| `cidrs` | IPv4 or IPv6 ranges, `ip/prefix`. |
| `ports` | Optional destination ports. Empty = every port. A populated list narrows the exception: in `deny` mode the listed destinations are allowed only on those ports; in `allow` mode they are blocked only on those ports. |
| `proxy` | Optional `http(s)://host:port` override for this instance only. Empty, `off`, `none` or `disabled` clear it: the instance falls back to `FLEETT_EGRESS_PROXY`. |

Validation, enforced by `PUT /api/instances/{id}/egress`:

- at most 64 entries per list;
- FQDN: lowercased, trailing dot removed, at most 253 characters, labels
  `[a-z0-9_-]` (no scheme, path, port or space); `*` only as a leading `*.`;
- CIDR: a valid IP and a prefix within `32` (v4) or `128` (v6);
- ports: `1..=65535`, deduplicated and sorted;
- proxy: an absolute `http://` or `https://` URL with a host.

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

Saving never deletes. A policy whose lists are all empty is stored like any
other (`source: explicit`); send `DELETE /api/instances/{id}/egress` — or
`fleett egress unset <id>` — to remove it. See *Render* for what an empty
policy actually does.

An instance archive (`GET /api/instances/{id}/export`) does not carry the
policy yet: re-apply it with `fleett egress set` after an import.

## Render

A policy renders proxy variables **only when it decides something the host
firewall does not** (issue #58). A policy is *legacy-equivalent* when:

- `mode` is `allow` (a blocklist), **and**
- `fqdns` and `ports` are empty, **and**
- every `cidrs` entry is inside `10.0.0.0/8`, `172.16.0.0/12` or
  `192.168.0.0/16` — or the list is empty.

The host `DOCKER-USER` rules already drop those ranges for a restricted
instance, so a proxy would add a failure mode without adding a decision. Such a
policy is stored, answered with `legacy_equivalent: true`, and renders **no**
`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`: the instance keeps its historical bytes.
This is what made an "identity" policy silently cut all egress in 0.6.0.

Everything else renders the three variables:

```yaml
environment:
  HTTP_PROXY: http://fleett-egress-proxy:3128
  HTTPS_PROXY: http://fleett-egress-proxy:3128
  NO_PROXY: localhost,127.0.0.1,::1,.local,fleett
```

`deny` mode, a single FQDN, a port restriction, a destination outside RFC 1918
(including any IPv6 range, since the host rules are v4-only) or an explicit
proxy all need it. "Explicit proxy" means the policy's own `proxy` field, or a
`FLEETT_EGRESS_PROXY` the operator changed from its built-in default: a proxy
you configured yourself is where your own rules live, so it is kept even for a
legacy-equivalent policy.

The address comes from the policy `proxy`, else `FLEETT_EGRESS_PROXY` (default
`http://fleett-egress-proxy:3128`). An empty value, `off`, `none` or `disabled`
suppresses the variables; the policy is still stored, and the API reports a
warning instead of writing a dead address — but only when the policy would have
needed the proxy (a legacy-equivalent policy never reports one).

Instances without a policy never gain those variables: their compose bytes are
unchanged by this feature. A `PATCH`, an env change, an import or a rolling
upgrade re-renders the compose file and keeps the policy applied.

`NO_PROXY` keeps loopback, `.local` and the `fleett` bridge direct so
agent-to-agent traffic and the gate do not depend on the proxy. A gate with
another DNS name must be added to the proxy ACLs (and, when it is reached by
name, to `NO_PROXY` by the operator).

### Reachability: `422 proxy_unreachable`

`PUT /api/instances/{id}/egress` probes the proxy the resulting policy would
render (fresh TCP connect, 750 ms, no cache) **before** writing the row or
recreating the container. If it does not answer, the write is refused:

```json
{
  "error": {
    "code": "proxy_unreachable",
    "message": "proxy http://127.0.0.1:9 is unreachable (…); deploying a policy …",
    "details": {
      "instance": "alice",
      "proxy": "http://127.0.0.1:9",
      "hint": "docker compose -f docker/fleett-proxy.example.yaml up -d"
    }
  }
}
```

No row is written, the compose file is unchanged and the container is not
recreated. The same guard runs on every other recreation path (instance patch,
env change, clone, import, share change). A reachable proxy is accepted even if
its ACLs do not match the policy: the probe checks the socket only.

`GET /api/instances/{id}/egress` still reports a proxy that died *after* the
write, without failing the read:

- `proxy_reachable: true|false`, absent when no proxy is rendered;
- `proxy_error`: the socket error text when `proxy_reachable` is `false`;
- a human-readable entry in `warnings` (kept for compatibility).

The probe results are cached for ten seconds per proxy URL, successes and
failures alike, so a dead proxy does not slow every read. `fleett doctor` /
`GET /api/doctor` reports the same state as the `egress-policy` check
(`ok`/`error`/`warn`), and `GET /api/egress/summary` gives every instance at
once.

## Deploy the proxy

`docker/fleett-proxy.example.yaml` runs Squid on the `fleett` network under the
container name `fleett-egress-proxy`. The complete, executable recipe lives in
[EGRESS.md](EGRESS.md#deploy-the-egress-proxy-complete-recipe); the short form:

```bash
cd docker
cp fleett-proxy.domains.example.txt fleett-proxy.domains.txt
cp fleett-proxy.cidrs.example.txt fleett-proxy.cidrs.txt
# generate both files from GET /api/egress/summary (see EGRESS.md), then:
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

The `proxy_acl` snippet returned by `GET /api/instances/{id}/egress` reads those
same **shared** files (`/etc/squid/fleett-domains.txt`,
`/etc/squid/fleett-cidrs.txt`), with per-instance ACL names so several snippets
can be concatenated without redeclaring an ACL. Generate the files from the
fleet-wide union:

```bash
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/egress/summary" \
  | jq -r '[.instances[] | select(.policy == "enforced") | .fqdns[]] | unique[]' \
  | sed 's/^\*\././' > docker/fleett-proxy.domains.txt
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" \
  "$FLEETT_URL/api/egress/summary" \
  | jq -r '[.instances[] | select(.policy == "enforced") | .cidrs[]] | unique[]' \
  > docker/fleett-proxy.cidrs.txt
docker compose -f docker/fleett-proxy.example.yaml restart fleett-proxy
```

The proxy must be on the `fleett` network (external network `fleett`, created
by fleett) for the container name to resolve. It must **not** publish `3128` on
the host.

## API

| Method | Route | Body | Effect |
|---|---|---|---|
| GET | `/api/instances/{id}/egress` | — | effective policy: `source`, `mode`, `fqdns`, `cidrs`, `ports`, `proxy` (only when rendered), `proxy_reachable`, `proxy_error`, `proxy_acl`, `legacy_equivalent`, `warnings` |
| PUT | `/api/instances/{id}/egress` | `{"mode","fqdns","cidrs","ports","proxy"}` | validate, probe the rendered proxy, store, rewrite the compose file, restart; `400 invalid_egress_policy`; `422 proxy_unreachable` (details `{instance,proxy,hint}`) when the proxy does not answer, before any write; audited as `egress.policy.update` |
| DELETE | `/api/instances/{id}/egress` | — | drop the explicit policy, return to the legacy shape; `404` when none; `egress.policy.delete` |
| POST | `/api/instances/{id}/egress/test` | `{"host":"api.github.com","port":443}` | policy decision plus enforcement state (below) |
| GET | `/api/instances/{id}/egress/blocks?limit=200` | — | refused destinations, newest first; `limit` 1..=1000 |
| DELETE | `/api/instances/{id}/egress/blocks` | — | `{"removed":N}`; audited as `egress.blocks.clear` |
| GET | `/api/egress/summary` | — | `{"instances":[{instance,policy:"none\|legacy\|enforced",mode,fqdns,cidrs,ports,proxy?,proxy_reachable?,proxy_error?,source,warnings}]}`; filtered to readable instances, one probe per distinct proxy |
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
   operator artifact. The API refuses to *create* a policy that renders a dead
   proxy (`422 proxy_unreachable`), but it cannot keep it alive: if the proxy
   stops later, `HTTP_PROXY` points at a dead name until the operator restarts
   it. A legacy-equivalent policy renders no variables and the host firewall
   covers it. `GET /api/egress/summary` and the `egress-policy` doctor check
   report the breakage.
2. **One shared proxy cannot distinguish instances.** The rendered variables
   are identical for every instance, so the proxy only sees the source IP; the
   ACL files are therefore a union unless the operator runs one proxy per
   instance or a proxy that authenticates each agent. Docker container IPs are
   not stable enough for `src` ACLs. `proxy_acl` keeps per-instance ACL names
   but reads the shared files, so it can be concatenated into the shared proxy
   configuration; per-instance files are only meaningful with one proxy per
   instance.
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
fleett egress policy alice                       # GET: source, mode, lists, proxy, warnings
fleett egress set alice --mode deny --fqdn api.github.com --port 443
fleett egress set alice --mode allow --cidr 10.0.0.0/8 --port 22
fleett egress set alice --mode deny --fqdn api.github.com \
                    --proxy http://127.0.0.1:3128   # per-instance proxy override
fleett egress unset alice                        # DELETE: back to the legacy shape
fleett egress blocks alice                       # newest first
fleett egress blocks alice --clear               # DELETE the block log
fleett --json egress policy alice                # raw payload, proxy_acl included
```

`set` replaces the whole policy, exactly like the `PUT` it calls: omitted lists
become empty and an omitted `--proxy` clears the override. That is why clearing
every field is **not** how you remove a policy — it stores an empty one (deny
everything in `deny` mode, legacy-equivalent in `allow` mode). Use `unset`.

`--fqdn`, `--cidr` and `--port` are repeatable; `--mode` accepts `allow` or
`deny` (the server defaults to `deny`); `--proxy off` clears the override.
`unset` exits 1 with the API's `not_found` code when the instance has no
explicit policy, so a script can tell "removed" from "there was nothing".
`policy`, `set` and `unset` print a short report, or the raw JSON with
`--json`. They use the same `~/.config/fleett/config.toml` (or `FLEETT_URL` /
`FLEETT_TOKEN`) as the other client commands.

The host-firewall subcommands are unchanged and stay local (no bearer token,
root on the Docker host): `fleett egress install|remove|status|print`.

See [CLI.md](CLI.md).
