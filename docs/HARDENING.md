# Hardening the agent container

Every instance runs one container rendered from `backend/src/render.rs`. Its
security posture is chosen by `FLEETT_HARDENING` and applied to the next compose
file written (create, patch, upgrade, rollback):

| `FLEETT_HARDENING` | Root filesystem | Capabilities | `no-new-privileges` | `tmpfs` |
|---|---|---|---|---|
| `standard` (default) | writable | `cap_drop: [NET_RAW]` | yes | `/tmp` |
| `strict` | **`read_only: true`** | **`cap_drop: [ALL]`** | yes, exactly once (`security_opt` and the flag below) | `/tmp`, `/run`, `/var/tmp` |

Both modes always keep:

```yaml
security_opt:
  - no-new-privileges:true
```

`standard` is byte-identical to what every release before issue #18 wrote: the
`read_only` key is simply absent. Switching an instance from `strict` back to
`standard` rewrites the file without the key, so the diff is always explicit.

`strict` additionally turns the root filesystem read-only and drops **every**
capability. An agent needs none: it talks to its model endpoint over the
network, writes under `/opt/data` (the bind mount) and `/tmp`, and never mounts
the Docker socket. Dropping `ALL` also removes `NET_RAW`, so the standard-mode
`cap_drop: [NET_RAW]` entry disappears rather than being duplicated.

## What `strict` gives

- **`read_only: true`** — no write anywhere outside the declared mounts. A
  compromised agent cannot drop a binary in `/usr/local/bin`, rewrite its own
  entrypoint, add a cron job for the image's user or plant a file that a later
  container start would execute.
- **`cap_drop: [ALL]`** — no `CAP_CHOWN`, `CAP_SETUID`, `CAP_SETGID`,
  `CAP_DAC_OVERRIDE`, `CAP_NET_RAW`… A process that escapes the agent runs as an
  unprivileged UID with no way to change that.
- **`no-new-privileges`** — setuid binaries cannot raise the privilege of a
  process, even inside the writable mounts.
- **`tmpfs` on `/tmp`, `/run`, `/var/tmp`** — the scratch directories stay
  writable, are memory-backed (so they vanish with the container) and do not
  count against the image layer.

The container already has no Docker socket, a `pids_limit` of 256, a memory
limit, an `unless-stopped` restart policy and a read-only-by-default egress
firewall (see [EGRESS.md](EGRESS.md)). `strict` is the last layer: a read-only
rootfs and an empty capability set.

## The s6 / capability analysis

The stock Hermes image (`hermes-agent:0.22.2-local`, Hermes 0.22.2) is **not**
`USER 10000` from the start:

- it starts as **root**;
- its s6 entrypoint `chown`s `/opt/data` (and the mounted shares) to
  `HERMES_UID`/`HERMES_GID` — `10000:10000` in a fleett compose file — then
  drops privileges with `s6-setuidgid`;
- `setuid`/`setgid` needs `CAP_SETUID` and `CAP_SETGID`, and `chown` needs
  `CAP_CHOWN` — all three are **gone** under `cap_drop: [ALL]`;
- s6 also writes its supervision tree and service state under `/run`, which is
  why `strict` mounts a `tmpfs` there (without it, s6 cannot create
  `/run/s6/services` and the container exits during startup).

Consequences, in order of how likely they are to bite:

1. **Under `cap_drop: [ALL]`, `chown /opt/data` fails** ("Operation not
   permitted"). The entrypoint may tolerate it (the directory is usually already
   owned by `10000:10000` because fleett created it, and the compose file mounts
   a host directory), or it may abort.
2. **Under `cap_drop: [ALL]`, `s6-setuidgid` fails** if the drop happens in the
   container rather than through the image's own `USER`. The agent then keeps
   running as root inside the container — restricted by the read-only rootfs and
   the empty capability set, but not by UID.

So `strict` is an **opt-in** posture: test it against the exact image tag you
run before enabling it fleet-wide. If the image needs one of those capabilities,
either

- keep `standard` (the default) and rely on the read-only-by-default egress
  firewall plus the absence of the Docker socket, or
- pre-provision the data directory so the `chown` is a no-op *and* confirm the
  UID drop happens through the image's `USER`/`GROUP` rather than `CAP_SETUID`.

Never widen the capability set without recording why: `cap_add` in a rendered
compose file is the one line that voids most of this document.

## Verify before enabling

Run these on a copy of **your** image tag, not on a generic `debian` image:

```bash
# 1. Capabilities the image starts with (as root, before any drop).
docker run --rm --entrypoint sh hermes-agent:0.22.2-local -c 'command -v capsh >/dev/null && capsh --print || echo "no capsh in the image"'

# 2. Does the entrypoint still come up with every capability dropped and a
#    read-only rootfs? Use the same command a fleett instance runs.
docker run --rm \
  --read-only \
  --tmpfs /tmp --tmpfs /run --tmpfs /var/tmp \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -v /tmp/fleett-probe/data:/opt/data \
  -e HERMES_UID=10000 -e HERMES_GID=10000 \
  --entrypoint gateway hermes-agent:0.22.2-local run

# 3. A rendered compose file can be probed the same way.
docker compose -f /opt/fleett/instances/<id>/compose.yaml up
```

Step 2 is the real gate: it either prints the gateway startup logs and stays up,
or exits with `Operation not permitted` / a permission error on `/run`.

## Verify after enabling

```bash
# No capability at all. `capsh --print` prints "Current: = ..." (empty set) and
# "Bounding set =" with nothing after it.
docker exec -it fleett-<id> sh -c 'capsh --print | sed -n "1,6p"'
# Expected (the important lines):
#   Current: =
#   Bounding set =
#   NoNewPrivs:     1

# The root filesystem is read-only: this must fail with
# "Read-only file system".
docker exec fleett-<id> sh -c 'touch /usr/local/bin/probe' || echo "read-only rootfs: OK"

# /tmp is writable and memory-backed.
docker exec fleett-<id> sh -c 'touch /tmp/probe && echo "tmpfs: OK"'

# The compose file itself: read_only, cap_drop ALL, the three tmpfs mounts and a
# single no-new-privileges flag.
docker inspect fleett-<id> --format '{{.HostConfig.ReadonlyRootfs}}'          # true
docker inspect fleett-<id> --format '{{.HostConfig.CapDrop}}'                 # [ALL]
docker inspect fleett-<id> --format '{{.HostConfig.SecurityOpt}}'             # [no-new-privileges:true]
docker inspect fleett-<id> --format '{{json .HostConfig.Tmpfs}}'              # /run,/tmp,/var/tmp
```

`docker inspect` on a running instance is the authority: the compose file is
what fleett wrote, the inspect payload is what Docker enforced.

## Rolling it out

`FLEETT_HARDENING` is read at startup and applied when a compose file is
rewritten:

```bash
# 1. Probe the image (see above) — do not skip this step.
# 2. Flip the setting and restart the control plane.
FLEETT_HARDENING=strict
# 3. Re-render one instance and watch it come up.
fleett restart <id>          # rewrites nothing: start/stop reuse the file
#    A PATCH, an upgrade or a `fleett upgrade --image` rewrites it. To re-render
#    the same settings through the API:
curl -fsS -X PATCH -H "Authorization: Bearer $FLEETT_TOKEN" \
  -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:8090/api/instances/<id>
docker inspect fleett-<id> --format '{{.HostConfig.ReadonlyRootfs}}'   # true
# 4. Only then touch the rest of the fleet.
```

An instance that fails to start after the flip keeps its compose file: set
`FLEETT_HARDENING=standard`, restart the control plane and PATCH the instance
again to go back to the previous bytes.

## API and control-plane hardening

The container posture above is one layer; the control plane itself is another.
A bearer token is a 64-hex secret that grants the fleet, so the API must not
allow an unlimited number of guesses, must not answer differently for a
non-existent instance, and must not let a stolen session drive the browser
anywhere it wants. Three protections, all enabled by default, all tunable from
the environment:

| Protection | What it does | Default |
|---|---|---|
| Rate limiting | sliding window per `(client IP, credential, route class)` | 120/min on `/api/*`, 10/min on the auth pages, 30/min on the heavy routes |
| Progressive lockout | stops credential guessing after N failures | 10 failures in 600 s → refused for 900 s |
| Security headers | CSP, framing, referrer, permissions, HSTS, MIME sniffing | all responses, UI and API |

### Rate limits

Every `/api/*` request is counted in a one-minute sliding window. The bucket key
is `(client IP, credential digest, class)`, where the credential is a SHA-256
digest of the bearer token or session cookie — the raw secret is never kept in
the limiter. Consequences, all covered by tests:

- **Per token.** Exhausting token A never affects token B, even from the same
  address.
- **Per address.** The same token from another `X-Forwarded-For` hop (or another
  peer address) has its own bucket.
- **Per class.** A flood of `/api/instances` calls does not consume the 30/min of
  `/instances/{id}/logs`, and the authentication pages have their own 10/min.

| Class | Routes | Variable | Default |
|---|---|---|---|
| `api` | every `/api/*` route not listed below | `FLEETT_RATE_LIMIT_PER_MIN` | `120` |
| `auth` | `/auth/login`, `/auth/callback` (and `/auth/logout`) | `FLEETT_RATE_LIMIT_AUTH_PER_MIN` | `10` |
| `heavy` | any route with an `exec`, `logs`, `files` or `export` segment under `/api/` — `GET /api/instances/{id}/logs`, `/files`, `/files/content`, `/export`, … | `FLEETT_RATE_LIMIT_HEAVY_PER_MIN` | `30` |
| `exempt` | `/api/health`, `/api/auth/config`, the static console, `/` | — | never limited |

A value of `0` disables that bucket (`FLEETT_RATE_LIMIT_PER_MIN=0` lifts the
blanket limit while keeping the auth and heavy buckets, for example). Setting
all three to `0` disables rate limiting entirely.

When a window is exhausted the API answers **`429`** with a `Retry-After`
header (whole seconds, at least 1) and the usual envelope:

```json
{"error":{"code":"rate_limited","message":"too many requests: retry in 42 second(s)",
  "details":{"retry_after_s":42,"class":"api"}}}
```

Stale buckets are collected once a minute, and the in-memory map is dropped
wholesale past 50 000 live buckets: a flood of unique keys can forget counters,
never exhaust memory. The state lives in the process — a restart is a legitimate
reset, exactly like the Prometheus counters.

### Progressive lockout

Failed authentications are counted separately from ordinary traffic, per
`(client IP, credential shape)`. The **shape** is `bearer`, `session` or
`anonymous`, not the token digest: an attacker who rotates random tokens is
still stopped, while a client that keeps presenting the same bad token is
already stopped by the bearer bucket.

- `FLEETT_AUTH_LOCKOUT_FAILURES` (default `10`): failures tolerated inside the
  window before the lock engages. `0` disables the lockout.
- `FLEETT_AUTH_LOCKOUT_WINDOW_S` (default `600`): sliding window of the failure
  counter.
- `FLEETT_AUTH_LOCKOUT_S` (default `900`): how long the lock refuses new
  attempts. Must be positive; the window too.

A `401` response on a non-exempt route records one failure. The failure that
reaches the threshold engages the lock; the following attempts answer `429`
with `Retry-After` and:

```json
{"error":{"code":"rate_limited","message":"too many requests: retry in 900 second(s)",
  "details":{"retry_after_s":900,"reason":"lockout"}}}
```

Each engagement writes one `auth.lockout` row to the audit trail (`actor`
`anonymous`, `target` the client address, `details.failures/window_s/lockout_s/
credential`) and one `WARN` log line. Refused attempts do not grow the audit
table. **A successful authentication clears the counter** for that IP and shape,
so a user who mistypes a password twice is not punished after a successful
login. The lock expires on its own after `FLEETT_AUTH_LOCKOUT_S`; there is no
unlock command, a restart also clears it.

The client address is the first `X-Forwarded-For` hop, then `X-Real-IP`, then the
peer address — the same rule the audit log uses. Put the control plane behind a
proxy that overwrites these headers, otherwise a direct client can choose its
own bucket name.

### Security headers

Every response — the console, `/api/*`, error pages, health — carries:

| Header | Value |
|---|---|
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'` |
| `X-Frame-Options` | `DENY` |
| `Referrer-Policy` | `no-referrer` |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` |
| `X-Content-Type-Options` | `nosniff` |
| `Strict-Transport-Security` | `max-age=31536000; includeSubDomains` — **only when `FLEETT_PUBLIC_URL` is `https://…`** |

HSTS is conditional on purpose: announcing it on a plain-HTTP deployment would
pin browsers to a scheme the server does not serve. `FLEETT_PUBLIC_URL` is read
in every authentication mode (it already drives the OIDC redirect URI and the
`auto` cookie security); set it to the external HTTPS URL when a TLS-terminating
proxy fronts fleett. API bodies are unchanged — the headers are additive.

The CSP allows inline **styles** (the self-contained error pages) but no inline
script and no external origin, so the console must keep shipping its JS and CSS
as same-origin assets.

### `GET /api/security/status`

Admin-only. Reports the running posture, never a secret (no token, no client
address):

```json
{"rate_limit":{"enabled":true,
  "limits":{"per_min":120,"auth_per_min":10,"heavy_per_min":30}},
 "lockout":{"failures":10,"window_s":600,"lockout_s":900,"locked_ips":0},
 "headers":{"csp":"default-src 'self'; …","x_frame_options":"DENY",
  "referrer_policy":"no-referrer",
  "permissions_policy":"camera=(), microphone=(), geolocation=()",
  "x_content_type_options":"nosniff","hsts":true},
 "tokens":{"expired_or_revoked":1,"active":3},
 "auth_mode":"bearer"}
```

`locked_ips` counts the distinct client addresses currently locked out. `tokens.active`
counts named tokens still accepted; `tokens.expired_or_revoked` counts revoked
rows (named tokens do not expire on their own). The bootstrap `FLEETT_TOKEN` is
not stored in the database and is not counted. A `viewer` receives
`403 forbidden`.

### `fleett security`

```bash
fleett security
# fleett security https://fleett.example.com
#
#   rate limit    enabled: 120/min, auth 10/min, heavy 30/min
#   lockout       after 10 failures in 600 s for 900 s; 0 locked
#   headers       x-frame-options DENY, referrer-policy no-referrer, hsts on
#   tokens        3 active, 1 revoked
#   auth mode     bearer

fleett --json security   # the raw /api/security/status payload
```

### Status codes and instance existence

Credentials are resolved **before** the route handler runs, so the answer never
reveals whether an instance exists to a caller who is not authenticated:

| Situation | Answer |
|---|---|
| missing or invalid bearer/session | `401 unauthorized` — always the same body, whether or not the instance in the URL exists |
| valid credential, insufficient role | `403 forbidden` |
| valid session, mutation without the CSRF header | `403 csrf_required` |
| rate limit or lockout | `429 rate_limited` + `Retry-After` |
| unknown instance/file/token id, for an authenticated caller allowed to see it | `404 not_found` |
| unknown route | `404 not_found` |

`404` is therefore only used for resources the caller is allowed to enumerate
(an authenticated admin asking for a missing instance, an admin revoking an
unknown token id); it is never an oracle for a bad credential. Do not add
"instance not found" checks before the authentication layer: the middleware
guarantees the ordering today and a new public route would silently break it.

### Metrics

`fleett_rate_limited_total{class="api|auth|heavy|lockout"}` counts refused
requests (see [METRICS.md](METRICS.md)). All four series are always exposed,
including at zero, so a dashboard does not have to guess whether a class was
never refused. `fleett doctor` does not fail on rate limiting: a burst is
normal, the counter is the signal.

### Verify

```bash
# 1. The headers on the console and on the API.
curl -sI https://fleett.example.com/ | grep -i -E 'content-security|x-frame|referrer|permissions|strict-transport'
curl -sI https://fleett.example.com/api/health | grep -i -E 'content-security|x-frame'

# 2. The limiter (adjust the token) — the third call of a 2/min bucket is a 429.
for i in 1 2 3; do
  curl -s -o /dev/null -w '%{http_code} ' -H "Authorization: Bearer $FLEETT_TOKEN" \
    http://127.0.0.1:8090/api/instances
done; echo

# 3. The lockout: ten bad tokens, then a Retry-After.
for i in $(seq 1 12); do
  curl -s -o /dev/null -D - -H 'Authorization: Bearer nope' \
    http://127.0.0.1:8090/api/instances | grep -i -E 'HTTP/|retry-after'
done

# 4. The status endpoint and the CLI.
curl -s -H "Authorization: Bearer $FLEETT_TOKEN" \
  http://127.0.0.1:8090/api/security/status | jq .
fleett security
```
