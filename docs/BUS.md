# Agent-to-agent message bus

`fleett` keeps a small, persistent message bus between the instances of a fleet (issue
#28). It is what turns *N isolated agents* into a fleet: `probe` can ask `tchouniz` a
question, `tchouniz` can answer, and the whole exchange is visible in the console and in
the audit log.

The bus is deliberately not a chat system. It has no delivery daemon, no websocket and no
agent-side code: a message is one row in SQLite and one JSON file on disk. Agents poll
their inbox through the API (or read the spool directory), which keeps an agent that is
mid-task free to finish before it answers.

## Data model

| Field | Meaning |
|---|---|
| `id` | UUID of the message |
| `from` / `to` | instance ids; the bus refuses self-messages and unknown instances |
| `kind` | `ask` (expects a `result`), `tell` (one-way) or `result` |
| `body` | free text, trimmed, 16 KiB max |
| `correlation_id` | ties a question to its answer and powers loop detection |
| `hops` | number of bus messages in the conversation, starting at `0` |
| `status` | `queued`, `delivered` (fetched by the recipient) or `read` (acked) |
| `expires_at` | queue TTL, 24 h by default (`ttl_s`, 30 s … 30 days) |

Messages survive a controller restart and an instance stop: a message addressed to a
stopped instance stays `queued` and is delivered when the agent polls its inbox after the
next start. Expired messages are filtered out of every read and removed by
`DELETE /api/messages/expired` (or by the housekeeping loop).

## Loop protection

Every message carrying a `correlation_id` gets `hops = max(hops of the conversation) + 1`.
Once `hops` reaches the server ceiling (8, `DEFAULT_MAX_HOPS`), the bus answers `409
message_loop` with `details {hops, max_hops}`. A normal request/reply pair uses hop 0 and
hop 1; a bot that forwards the answer again is stopped at the eighth round trip instead of
saturating the fleet.

## API

Send as instance `probe`:

```bash
curl -sS -X POST "$FLEETT_URL/api/instances/probe/messages" \
  -H "Authorization: Bearer $FLEETT_TOKEN" -H 'Content-Type: application/json' \
  -d '{"to":"tchouniz","kind":"ask","body":"what changed in the last deploy?","correlation_id":"deploy-42"}'
```

Read the inbox (this stamps `delivered_at`):

```bash
curl -sS "$FLEETT_URL/api/instances/tchouniz/messages?view=inbox&limit=50" \
  -H "Authorization: Bearer $FLEETT_TOKEN"
```

Acknowledge it once the agent has handled it:

```bash
curl -sS -X POST "$FLEETT_URL/api/instances/tchouniz/messages/<message-id>/ack" \
  -H "Authorization: Bearer $FLEETT_TOKEN"
```

`GET /api/messages` is the admin view (`?instance=`, `?from=`, `?to=`, `?kind=`, `?limit=`).
It shows both directions, expired rows excluded, newest first.

An admin may set `"from"` to another instance when acting as an orchestrator; a non-admin
caller cannot send on behalf of an instance other than the one in the path.

## Agent side

Every rendered compose file mounts `$FLEETT_ROOT/bus` at `/opt/bus`. When a
message is queued, a JSON mirror is written to `bus/<recipient>/<id>.json`, so an agent
without HTTP access can still read its inbox:

```bash
ls /opt/bus/tchouniz/
cat /opt/bus/tchouniz/<id>.json
```

The spool is a convenience mirror, not the source of truth: acknowledgements and the
global view only exist in SQLite. Instances created before this release get the mount the
next time their compose file is rendered (any `PATCH`, upgrade or re-create).

## CLI

```bash
fleett say tchouniz "what changed in the last deploy?" --kind ask --correlation deploy-42
fleett messages tchouniz                 # inbox
fleett messages tchouniz --view outbox   # sent by tchouniz
fleett ack tchouniz <message-id>
```

`fleett say` takes the sender from `--from`, or from `FLEETT_INSTANCE` when the CLI runs
inside an agent container.

## Audit

Every send is audited as `message.send` (from, to, kind, hops, correlation) and every ack
as `message.ack`. The console shows the same data under *Messages*.

## Limits

- 16 KiB per body, 500 rows per page, 30 days maximum TTL.
- Eight hops per conversation.
- No binary payloads and no attachments: use a share for files.
- Delivery is at-least-once polling: an agent that fetches an inbox twice sees the same
  message until it acks it. Consumers must be idempotent on `id`.
