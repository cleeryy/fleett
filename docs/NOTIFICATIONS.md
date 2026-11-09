# Outbound alerts (notifications)

The control plane used to be pull-only: an operator found out that an agent had
stopped by looking at the console. Notification channels push the same signal to
a webhook, Discord, Telegram or a plain SMTP relay, and keep a journal of every
delivery attempt.

```bash
# Webhook: every event.
fleett notification add --name ops --kind webhook \
  --target https://hooks.example.test/fleett --events '*'

# Discord: only stopped / unhealthy agents.
fleett notification add --name discord --kind discord \
  --target 'https://discord.com/api/webhooks/1/abc' \
  --events instance.down,instance.unhealthy

# Telegram: every incident.
fleett notification add --name oncall --kind telegram \
  --target 424242 --secret '123456:BOT-TOKEN' --events 'incident.*'

# SMTP: everything (relay on localhost).
FLEETT_SMTP_FROM=fleett@example.test fleett notification add \
  --name mail --kind smtp --target 127.0.0.1:25

fleett notifications
fleett notification test <id>
fleett deliveries --channel <id> --limit 50
```

Equivalent API: `POST /api/notifications`, see [API.md](../API.md#notifications).
The web console uses the same routes (mutations require the admin role).

## Channels

| Kind | `target` | `secret` | What is sent |
|---|---|---|---|
| `webhook` | `http(s)://…` URL | optional | JSON `POST` (see below) |
| `discord` | Discord webhook URL | optional | `{"content": "<message>"}` |
| `telegram` | chat id or `@channel` | **required** bot token | `sendMessage` to `api.telegram.org` |
| `smtp` | `host:port` | optional (unused in v1) | plain-text e-mail |

A channel also carries:

- `events`: comma-separated subscriptions, validated against the table below;
- `enabled`: disabled channels are skipped by the notifier but can still be
  tested explicitly;
- `has_secret`: returned by the API instead of the secret itself.

### Webhook payload

```json
{
  "event": "instance.down",
  "at": "2026-11-09T10:00:00Z",
  "instance": {"id": "alice", "name": "Alice", "state": "stopped"},
  "message": "instance: alice\nname: Alice\nstate: stopped\nlast error: …\nconsole: https://fleett.example/instances/alice",
  "details": null
}
```

`instance` is `null` for fleet-wide events (`quota.exceeded`,
`egress.drift`, `secret.expiring`, `notification.test`). `details` is the
optional free-form detail of the event.

## Events

| Event | Emitted when |
|---|---|
| `instance.down` | an instance is stopped (`POST /api/instances/{id}/stop`) or reported down |
| `instance.unhealthy` | the health probe reports `unhealthy` |
| `instance.up` | an instance is started and answers |
| `upgrade.failed` | a rolling upgrade leaves an instance rolled back or failed |
| `quota.exceeded` | the fleet cap (`FLEETT_MAX_TOTAL_*`) refuses a create |
| `egress.drift` | the host firewall marker no longer matches the Docker subnet |
| `secret.expiring` | a token is close to its expiry (operator-provided metadata) |
| `incident.open` / `incident.close` | an incident is opened or closed |
| `notification.test` | the "test channel" button/endpoint |

Subscriptions accept concrete events plus two wildcards:

- `*` — every event;
- `incident.*` — every `incident.` event. Other families (`instance.*`) are
  matched by listing their events, so a new event can never silently activate
  a channel.

Unknown events are refused at create/patch time (`400 invalid_events`).

## Message template

The subject is `fleett: <event> - <instance>` (or `fleett: <event>` without an
instance). The body is one short line per fact:

```
instance: alice
name: Alice
state: stopped
last error: healthcheck failed
console: https://fleett.example/instances/alice
```

The console link is absolute when `FLEETT_PUBLIC_URL` is set (or when OIDC is
configured, its public URL is reused); otherwise it is the relative path
`/instances/<id>`. Discord content is truncated to 1 900 characters, Telegram
text to 4 000.

## Coalescing and retries

- **Anti-spam:** at most one delivery per channel and per event in a rolling
  five-minute window. A second event in the window is recorded with status
  `suppressed` (and a pointer to the delivery it duplicates), not sent. Events
  of different kinds (`instance.down`, `instance.up`, …) have separate windows.
- **Retries:** the initial send plus three retries, after 1 s, 5 s and 30 s.
  The delivery row stays `pending` while retrying, then becomes `sent` or
  `failed`; `attempts` counts every try and `error` keeps the last error.
- **Journal:** every send, failure and suppression is a row in
  `notification_deliveries`. `GET /api/notifications/deliveries?channel=&limit=`
  returns them newest first. Deleting a channel keeps its journal.

A slow or unreachable target never blocks the API: `stop`/`start` handlers
spawn the delivery and return immediately.

## SMTP (v1 limitations)

SMTP is implemented as a minimal plain-TCP conversation, without STARTTLS and
without authentication:

- `target` is the relay `host:port`;
- `MAIL FROM` is `FLEETT_SMTP_FROM` (default `fleett@localhost`);
- `RCPT TO` is `FLEETT_SMTP_TO`, falling back to the `MAIL FROM` address;
- the body is `text/plain; charset=utf-8`, dot-stuffed, with `From`, `To`,
  `Subject`, `Date` and `MIME-Version` headers.

A server that requires TLS (port 587 with STARTTLS, or an explicit `530`)
fails the delivery with a clear message:

```
the SMTP server requires TLS; fleett speaks plain SMTP only in v1
(point the channel at a local relay): 530 5.7.0 Must issue a STARTTLS command first
```

Point the channel at a local relay (Postfix/msmtp on the host) that performs
TLS and authentication towards the real mail server. The `secret` field is
accepted but unused by the SMTP sender in v1.

## Security

- A channel secret is encrypted with the same AES-256-GCM key as the provider
  credentials (`FLEETT_SECRETS_KEY` / `/etc/fleett/secrets.key`). Without a key,
  creating or rotating a channel **with a secret** is refused with the existing
  `409 secrets_key_missing`. A plain webhook (no secret) does not need a key.
- No endpoint returns the secret or the ciphertext: the API returns
  `has_secret: true|false`.
- A Discord webhook URL and most webhook URLs are credentials. They are never
  logged; the audit trail records a masked target (`https://discord.com/…`), and
  Telegram/SMTP targets (a chat id, a `host:port`) are kept as-is.
- Secrets lookups happen per delivery: rotating the fleet key makes existing
  channel secrets unreadable, and the delivery fails with an explicit error
  instead of sending to the wrong place.
- The delivery journal contains no secret and no target.

## Wiring

Modules emit an event through:

```rust
fleett::notify::emit(&state, Event::InstanceDown, Payload::instance(&spec)).await;
// or, when the caller must not wait for the network:
fleett::notify::spawn_emit(state.clone(), event, payload);
```

Already wired: `POST /api/instances/{id}/stop` emits `instance.down`,
`POST /api/instances/{id}/start` emits `instance.up` (restart stays silent: it
is intentional), a create/update refused by the fleet caps emits
`quota.exceeded`, and a rolling upgrade that leaves an instance `rolled_back`
or `failed` emits `upgrade.failed` with its last error. The watchdog uses the
same call as it lands; `Event` already carries `egress.drift`,
`secret.expiring`, `instance.unhealthy` and `incident.*`.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `409 secrets_key_missing` | the channel has a secret but no key: `sudo fleett secrets init` |
| `400 invalid_target` | Telegram chat id empty, SMTP target without `:port`, or a non-http(s) webhook URL |
| `400 invalid_events` | an event name is not in the table and is not `*` / `incident.*` |
| a delivery is `failed` with HTTP 401/404 | rotated or deleted Discord/Telegram token, wrong chat id |
| no delivery after an event | the channel is disabled, its `events` do not match, or the event is inside the five-minute window (`suppressed` row) |
| `failed` with "the SMTP server requires TLS" | point the channel at a local relay; TLS is not supported in v1 |
