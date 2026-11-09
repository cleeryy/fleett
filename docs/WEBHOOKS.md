# Webhooks — signed outbound events

fleett can push every meaningful change to an HTTP endpoint: a Forgejo action, an n8n
workflow, a Discord relay, a deployment script. A webhook is distinct from the agent
gateway and from the message bus: it is one URL, a subscription and an HMAC secret, and it
never touches an agent container.

- **Transport**: `POST` with a JSON body, `Content-Type: application/json`.
- **Signature**: `X-Fleett-Signature: sha256=<hex HMAC-SHA256(secret, raw_body)>`.
- **Retries**: 3 retries after the first attempt (1 s, 10 s, 60 s), then the delivery is
  recorded `failed`.
- **Never blocking**: a slow or dead endpoint never delays the API mutation that triggered
  the event; delivery runs in a spawned task.
- **Secret at rest**: encrypted with the same AES-256-GCM key as providers and instance
  environment variables. No endpoint ever returns it; the API exposes `has_secret: true`.

## Events

| Event | Published by | Status |
|---|---|---|
| `instance.created` | `POST /api/instances` | wired |
| `instance.started` | `POST /api/instances/{id}/start` and `/restart` (`data.action` = `start` or `restart`) | wired |
| `instance.stopped` | `POST /api/instances/{id}/stop` | wired |
| `instance.deleted` | `DELETE /api/instances/{id}` | wired |
| `env.changed` | `PUT /api/instances/{id}/env`, `DELETE /api/instances/{id}/env/{key}`, `PATCH /api/instances/{id}` with `env` | wired |
| `upgrade.started` | `POST /api/upgrade` (accepted, not a dry run) | wired |
| `upgrade.ok` | rolling upgrade: one instance reached a healthy state | wired |
| `upgrade.failed` | rolling upgrade: pull failure, or rollback that could not restart | wired |
| `upgrade.rolled_back` | rolling upgrade: the previous image was restored | wired |
| `secret.rotated` | `PATCH /api/providers/{id}` with `secret` | wired |
| `incident.open`, `incident.closed` | watchdog / inspector merge | subscribable, no publisher yet |
| `pipeline.run.started`, `pipeline.run.ok`, `pipeline.run.failed` | pipeline scheduler merge | subscribable, no publisher yet |

The last five events are accepted by the API now. The `incident.*` and `pipeline.run.*`
publishers belong to the watchdog and scheduler work; the library function
`fleett::webhooks::emit(&state, Event::IncidentOpen, payload)` is the call site to add there.

### Subscription filters

`events` is a list (at most 32 entries). Each entry is:

- an exact event: `"instance.created"`,
- a prefix wildcard ending in `.*`: `"instance.*"`, `"upgrade.*"`, `"pipeline.run.*"`,
- `"*"`: everything.

Unknown names and malformed filters are refused with `400 invalid_webhook`.

## Payload

```json
{
  "id": 42,
  "event": "instance.started",
  "at": "2026-11-09T12:34:56Z",
  "data": {
    "id": "alice",
    "status": "running",
    "action": "restart"
  }
}
```

`id` is the delivery row id, also sent as `X-Fleett-Delivery`. `data` is event-specific and
always carries the instance, provider, job or webhook id it concerns. The body is the exact
byte sequence that is signed: do not re-serialize it before verifying.

Headers of every delivery:

| Header | Value |
|---|---|
| `Content-Type` | `application/json` |
| `X-Fleett-Event` | event name, e.g. `instance.started` |
| `X-Fleett-Delivery` | delivery id (same as `id` in the body) |
| `X-Fleett-Signature` | `sha256=<hex HMAC-SHA256(secret, raw_body)>` |

## Verifying a signature

Always compute the digest over the **raw request body**, before JSON parsing.

Python (standard library, works with Flask, FastAPI, Django, a lambda):

```python
import hashlib
import hmac


def verify(secret: str, raw_body: bytes, signature_header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), raw_body, hashlib.sha256).hexdigest()
    # Constant-time comparison: never use ==.
    return hmac.compare_digest(expected, signature_header)


# Flask example
# @app.post("/fleett")
# def hook():
#     body = request.get_data(cache=True)  # raw bytes, not request.json
#     if not verify(SECRET, body, request.headers["X-Fleett-Signature"]):
#         return {"error": "bad signature"}, 401
#     event = request.headers["X-Fleett-Event"]
#     payload = request.get_json()
#     ...
```

JavaScript / Node (no dependency):

```js
import { createHmac, timingSafeEqual } from "node:crypto";

export function verify(secret, rawBody, signatureHeader) {
  const expected = "sha256=" + createHmac("sha256", secret).update(rawBody).digest("hex");
  const a = Buffer.from(expected);
  const b = Buffer.from(signatureHeader ?? "");
  return a.length === b.length && timingSafeEqual(a, b);
}

// Express: express.raw({ type: "application/json" }) keeps the exact bytes.
// app.post("/fleett", express.raw({ type: "application/json" }), (req, res) => {
//   if (!verify(process.env.FLEETT_WEBHOOK_SECRET, req.body, req.get("X-Fleett-Signature"))) {
//     return res.status(401).end();
//   }
//   const event = req.get("X-Fleett-Event");
//   const payload = JSON.parse(req.body.toString("utf8"));
// });
```

An invalid signature is a `401` on the receiver side; fleett will retry the delivery (the
endpoint answered a non-2xx status). A receiver should therefore treat a bad signature as a
configuration problem, not as a transient error.

## Retries and the delivery log

One delivery makes up to **4 attempts**: the first one, then retries after 1 s, 10 s and 60 s.
Every attempt has a 10 s timeout. Any `2xx` answer marks the delivery `sent`; a non-2xx or a
transport error schedules the next attempt. The row keeps the last HTTP status and the last
error.

`GET /api/webhooks/{id}/deliveries?limit=50` (newest first, max 200):

```json
{
  "id": 42,
  "webhook_id": "5a2c…",
  "event": "instance.started",
  "payload": { "id": 42, "event": "instance.started", "at": "…", "data": { "id": "alice" } },
  "status": "sent",
  "attempts": 1,
  "response_status": 204,
  "error": null,
  "created_at": "2026-11-09T12:34:56Z",
  "delivered_at": "2026-11-09T12:34:56Z"
}
```

`status` is `pending`, `sent` or `failed`. Deleting a webhook deletes its log.

## API

| Method | Route | Body | Result |
|---|---|---|---|
| GET | `/api/webhooks` | — | `Webhook[]`, never the secret |
| POST | `/api/webhooks` | `{name,url,events,secret,enabled?}` | `201 Webhook`, admin only |
| GET | `/api/webhooks/{id}` | — | `Webhook` |
| PATCH | `/api/webhooks/{id}` | `{name?,url?,events?,secret?,enabled?}` | `Webhook`, admin only; `secret` rotates the signing key |
| DELETE | `/api/webhooks/{id}` | — | `204`, admin only |
| POST | `/api/webhooks/{id}/test` | — | `{ok,status,latency_ms,error?}`, admin only |
| GET | `/api/webhooks/{id}/deliveries?limit=50` | — | `WebhookDelivery[]` |

`POST /api/webhooks` and the test endpoint require a secrets key: the HMAC secret is stored
encrypted, so a control plane without `FLEETT_SECRETS_KEY` (or the key file) answers
`409 secrets_key_missing`. Run `sudo fleett secrets init` first.

Every mutation is audited (`webhook.create`, `webhook.update`, `webhook.delete`,
`webhook.test`).

### Test one endpoint

```bash
fleett webhook test 5a2c…
# or
curl -sS -X POST -H "Authorization: Bearer $FLEETT_TOKEN" \
  http://127.0.0.1:8090/api/webhooks/5a2c…/test
```

The test sends one signed `webhook.test` payload, **not retried** (the operator waits for the
answer), and records it in the delivery log.

## CLI

```bash
fleett webhooks                                  # table: id, name, enabled, events, url
fleett --json webhooks                           # same as JSON

fleett webhook add --name deploy \
  --url https://ci.example.test/fleett \
  --events instance.created,instance.started,upgrade.* \
  --secret "$(openssl rand -hex 32)"

fleett webhook test <id>
fleett webhook rm <id>
fleett deliveries <id> --limit 20                # attempts, HTTP status, last error
```

The secret is only accepted on input; no command prints it back.

## OpenAPI and the TypeScript SDK

`GET /api/openapi.json` serves the static OpenAPI 3.0 document (also committed at the
repository root as `openapi.json`, embedded in the binary, and regenerated by hand from
`backend/src/api/mod.rs`). It is public: it contains no secret and lets SDK generators read
the contract without a token. A unit test parses it and checks that every path starts with
`/api` and every operation has responses.

`GET /api/openapi.json` is the canonical contract for every route, not only webhooks.

The minimal TypeScript client lives in `sdk/typescript/`. It has no runtime dependency and
is typechecked in CI with `tsc --noEmit`. It covers instances (lifecycle, env, files, git),
providers, usage and webhooks. Pipelines are omitted until the scheduler exposes an HTTP
API; the document currently has no `/api/pipelines` route.

```ts
import { FleettClient } from "./sdk/typescript/src/index.ts";

const fleett = new FleettClient({
  url: "http://127.0.0.1:8090",
  token: process.env.FLEETT_TOKEN,
});

const hook = await fleett.webhooks.create({
  name: "discord-relay",
  url: "https://relay.example.test/fleett",
  events: ["secret.rotated", "instance.*"],
  secret: process.env.WEBHOOK_SECRET!,
});

console.log(hook.id, hook.has_secret); // the secret itself is never returned
console.log(await fleett.webhooks.deliveries(hook.id, 20));
```

## Examples

### Notify Discord when a credential rotates

The relay is a small endpoint that verifies the signature, then posts to a Discord webhook.
It keeps the fleett secret out of Discord.

```js
import http from "node:http";
import { createHmac, timingSafeEqual } from "node:crypto";

const SECRET = process.env.FLEETT_WEBHOOK_SECRET;
const DISCORD = process.env.DISCORD_WEBHOOK_URL;

http
  .createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", async () => {
      const body = Buffer.concat(chunks);
      const expected = "sha256=" + createHmac("sha256", SECRET).update(body).digest("hex");
      const given = Buffer.from(req.headers["x-fleett-signature"] ?? "");
      const ok =
        Buffer.byteLength(expected) === given.length &&
        timingSafeEqual(Buffer.from(expected), given);
      if (!ok) {
        res.writeHead(401).end();
        return;
      }
      const { event, data } = JSON.parse(body.toString("utf8"));
      if (event === "secret.rotated") {
        await fetch(DISCORD, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            content: `fleett: provider **${data.name}** rotated (${data.restarted} instance(s) restarted)`,
          }),
        });
      }
      res.writeHead(204).end();
    });
  })
  .listen(9000);
```

Subscribe with `--events secret.rotated`.

### Trigger a deployment after an upgrade

```bash
fleett webhook add --name forgejo-deploy \
  --url https://forgejo.example.test/api/v1/repos/ops/fleet/actions/workflows/deploy.yml/dispatches \
  --events upgrade.ok \
  --secret "$(openssl rand -hex 32)"
```

The receiver verifies `X-Fleett-Signature`, checks `X-Fleett-Event == "upgrade.ok"`, then
reads `data.id` and `data.to` (the new image) before dispatching the deployment.

## Security notes

- Use a random secret of at least 32 bytes (`openssl rand -hex 32`).
- Event payloads never contain credential values: an `env.changed` event lists the variable
  names and secret flags, and `secret.rotated` names the provider and its variable.
- Compare signatures in constant time and only after reading the raw body.
- `http` is accepted so a receiver on loopback or the private network (n8n, a local relay)
  works without TLS; prefer `https` for anything off-host.
- Rotate with `PATCH /api/webhooks/{id}` and `{"secret": "…"}`; deliveries in flight keep
  being signed with the old key only until they succeed (the secret is read at delivery
  time, so a rotation applies to the next attempt).
- A webhook is a credential-adjacent object: admin role required to create, edit or test it.
