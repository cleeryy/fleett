# Assistant — contextual drafting copilot

The console ships an assistant panel available on every page. It drafts the
files that are tedious to write by hand — `SKILL.md`, `SOUL.md`, environment
variables, `fleet.yaml`, pipeline YAML — and explains errors in plain language.
It is a **drafting tool**: it never writes anything. Every answer that contains
an applicable change returns a proposal, and the console applies it through the
regular routes (`PUT /files/content`, `PUT /instances/{id}/env`, …), so the
write keeps its ETag check, backup, git commit and audit row.

The whole assistant is server-side; the console only renders the panel, shows
the context summary and calls the routes below.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_ASSISTANT` | `on` | `off` removes the routes entirely (`404 route not found`) |
| `FLEETT_ASSISTANT_PROVIDER` | — | id of an existing provider; the assistant borrows its secret and base URL |
| `FLEETT_ASSISTANT_MODEL` | `deepseek-v4.1-flash` | model sent to the provider, unless the provider sets its own `model` |
| `FLEETT_ASSISTANT_BUDGET_TOKENS` | `50000` | token budget of one session; `0` disables the limit |
| `FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS` | `4096` | output ceiling of one reply (`max_tokens`); `0` omits the field and lets the provider decide |

The provider is a **normal provider** created with `POST /api/providers`
(see [PROVIDERS.md](PROVIDERS.md)): kind `openai-compatible`, a base URL such as
`https://opencode.ai/inference/go/openai/v1` and its encrypted secret. The
assistant decrypts that secret per request; it never stores one of its own. It
also sends the provider's `extra_headers` and uses its `model` when one is set,
so an `opencode.ai` gateway works without any extra configuration (issue #64).

`FLEETT_ASSISTANT=off` is a hard switch: the router does not register the
routes, so they answer the generic `404 not_found` and the console hides the
panel. Nothing else changes; the provider, if one is configured, keeps serving
the instances that reference it.

## Provider resolution

The assistant needs one provider. It resolves it from three sources, in this
order, and reports which one won in `provider_source`:

| `provider_source` | When |
|---|---|
| `env` | `FLEETT_ASSISTANT_PROVIDER` names an existing provider |
| `setting` | the console selected one with `PATCH /api/assistant/config` |
| `auto` | nothing is selected and the fleet holds **exactly one** provider |
| `none` | nothing applies (see `reason`) |

`auto` never guesses between several credentials: with two or more providers and
no selection the assistant stays unconfigured until an operator chooses.

When `provider` is `null`, `reason` says which situation the panel is in:

| `reason` | Meaning | What the console shows |
|---|---|---|
| `no_provider` | the fleet holds no provider at all | "create a provider" |
| `not_selected` | providers exist, none is selected | a selector pre-filled with the existing providers |

`reason` is `null` whenever a provider is resolved.

A persisted selection takes effect immediately, without a restart; the env
variable keeps priority, so a host pinned with `FLEETT_ASSISTANT_PROVIDER`
ignores the stored value (and says so with `provider_source: "env"`).

### Selecting a provider

```
PATCH /api/assistant/config        (admin)
{"provider": "<provider id>"}      selects and persists
{"provider": null}                 clears the selection
```

The response is the resolved configuration, identical to `GET
/api/assistant/config`, so the panel updates in one round trip. An unknown id
(or a missing `provider` field) is refused with `422`:

```json
{
  "error": {
    "code": "assistant_provider_unknown",
    "message": "the assistant references provider \"d2b3…\", which no longer exists",
    "details": {
      "provider": "d2b3…",
      "source": "setting",
      "valid": ["8f21…", "a4c9…"],
      "providers": "/api/providers",
      "hint": "select an existing provider with PATCH /api/assistant/config, or clear FLEETT_ASSISTANT_PROVIDER"
    }
  }
}
```

### Broken reference

`FLEETT_ASSISTANT_PROVIDER` or the stored setting may name a provider that was
deleted. `GET /api/assistant/config` and `POST /api/assistant/chat` then answer
`422 assistant_provider_unknown` with
`details.valid` (the ids that do exist), never a `500` and never a silent
`provider: null` — the console must be able to tell "no provider at all" from
"the configured one is gone". `GET /api/doctor` reports the same state as an
`error` on its `assistant` check.

### Without a provider

`GET /api/assistant/config` still answers `200` with `"provider": null` and
`provider_source: "none"`, which is what lets the panel render the right hint
instead of an error. The chat route answers `409 assistant_not_configured`
carrying the same `reason`:

```json
{
  "error": {
    "code": "assistant_not_configured",
    "message": "the assistant has no provider selected and the fleet holds several",
    "details": {
      "reason": "not_selected",
      "env": "FLEETT_ASSISTANT_PROVIDER",
      "hint": "choose one with PATCH /api/assistant/config {\"provider\":\"<id>\"}; the assistant never guesses between several credentials",
      "providers": "/api/providers",
      "configure": "/api/assistant/config"
    }
  }
}
```

## API

### `GET /api/assistant/config`

Any authenticated role. `?session_id=` reports that session's counter; without
it the response reports the sum of the live sessions (a restart resets them).

```json
{
  "enabled": true,
  "provider": {"id": "…", "name": "opencode-go"},
  "provider_source": "auto",
  "reason": null,
  "model": "deepseek-v4.1-flash",
  "budget_tokens": 50000,
  "used_tokens": 1200
}
```

`provider_source` and `reason` are documented under
[Provider resolution](#provider-resolution).

### `POST /api/assistant/chat`

```json
{
  "session_id": "panel-7f3c",
  "context": {
    "page": "files",
    "instance": "support-agent",
    "path": "/SKILL.md",
    "selection": "…",
    "file_content": "…",
    "schema": "…"
  },
  "messages": [
    {"role": "user", "content": "write me a SKILL.md that sorts invoices"}
  ],
  "task": "skill",
  "max_tokens": 4096
}
```

`context.page` is required; every other context field is optional. `messages`
is the conversation the console keeps (at most 40 messages, 16 KiB each, roles
`user` and `assistant` only). `task` defaults to `freeform`. `max_tokens` is
optional: it lowers the output ceiling for this turn only and is always bounded
by `FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS`.

```json
{
  "message": "Here is a draft that sorts invoices by supplier.",
  "proposal": {
    "kind": "file",
    "path": "/SKILL.md",
    "content": "---\nname: invoice-sorter\ndescription: Sort invoices by supplier\n---\n…"
  },
  "truncated": false,
  "finish_reason": "stop",
  "usage": {
    "prompt": 412,
    "completion": 188,
    "total": 600,
    "reasoning": 96,
    "max_tokens": 4096,
    "finish_reason": "stop",
    "truncated": false
  },
  "redaction": {
    "sent": ["page: files", "instance: support-agent", "path: /SKILL.md", "file_content: 812 bytes"],
    "omitted": ["env secrets: 2 value(s) withheld (DISCORD_BOT_TOKEN, API_KEY)"],
    "redacted": 1
  }
}
```

A proposal is one of:

| Kind | Shape | Applied through |
|---|---|---|
| `file` | `{kind, path, content}` | `PUT /api/instances/{id}/files/content` (with its ETag) |
| `yaml` | `{kind, path?, content}` | same route when `path` is set; copy-only otherwise |
| `env` | `{kind, env: [{key, value?, secret}]}` | `PUT /api/instances/{id}/env` |

`env` entries carry no value when `secret: true`: the operator fills the
credential in the console, and it never round-trips through the model.

A proposal is always complete (it replaces the whole target) and is validated:
`path` starts with `/`, contains no `..`, and never names `config.yaml` or
`healthcheck.py` (fleett renders both from the instance settings). An invalid
proposal is dropped and the explanation is still returned.

### Truncation and the output ceiling

`FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS` is sent as `max_tokens` (default `4096`);
`0` omits the field entirely so the provider applies its own default. A
per-request `max_tokens` can only lower the ceiling, never raise it.

The response always reports how generation ended:

| Field | Meaning |
|---|---|
| `finish_reason` | `stop`, `length` or `other`, normalized from the upstream value |
| `truncated` | `true` when `finish_reason` is `length`, or when the JSON output was structurally incomplete |

Both fields are repeated in `usage`, next to the token counts.

A truncated completion **never** yields a proposal. If the model had started
one, the route refuses with `422 assistant_proposal_truncated` instead of
handing the console a half file:

```json
{
  "error": {
    "code": "assistant_proposal_truncated",
    "message": "the assistant reply was cut at the output token ceiling before its proposal was complete: raise FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS or ask for a shorter file",
    "details": {
      "finish_reason": "length",
      "max_tokens": 4096,
      "env": "FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS",
      "hint": "increase FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS (0 lets the provider decide) or ask for a shorter file; nothing was applied"
    }
  }
}
```

When the cut fell in the explanation, the route answers `200` with
`truncated: true`, the recovered text and no proposal. The panel shows a
“Reply truncated” banner and a **Continue** button; continuing sends the usual
“continue exactly where you stopped” turn with the partial answer kept in the
history, so the model does not repeat the beginning.

A reasoning model such as `deepseek-v4.1-flash` returns `reasoning_content`,
and that reasoning **consumes the same `max_tokens` budget** as the visible
answer. The route keeps the reasoning out of `message` and reports its token
count in `usage.reasoning` when the provider sends
`completion_tokens_details.reasoning_tokens`. A long reasoning can therefore
leave the visible answer empty at `finish_reason: "length"`; the response is
then a readable notice asking to raise the ceiling rather than an empty bubble.

### Structured output repair

The system prompt asks for one JSON object `{message, proposal?}` (a single
fenced JSON block is tolerated). The answer is parsed defensively (issue #69):

1. strict JSON, then a fenced JSON block;
2. a tolerant repair for a cut object (closing the missing quote and brace),
   then a scan that extracts the `message` string and decodes its escapes;
3. a readable `422 assistant_reply_unusable` when nothing can be extracted.

Raw JSON is never returned in `message`, and literal `\n` or braces are never
displayed. Every parse failure is logged at WARN with the output size and a
redacted 200-character preview. A proposal is only offered when the JSON was
strictly valid **and** generation did not hit the ceiling.

## Tasks

The system prompt depends on `task`:

| Task | What the model is asked for |
|---|---|
| `skill` | `SKILL.md`: front matter (`name`, `description` ≤ 60 characters), steps, `## Pitfalls` |
| `soul` | `SOUL.md`: identity, tone, scope, refusals |
| `env` | environment variables with plausible allowlist values, credentials as `secret: true` |
| `yaml` | a complete `fleet.yaml` or pipeline document; without `path` it is a copy-only snippet |
| `explain` | cause, impact and fix of an error or a `last_error` |
| `config` | a copy-only Hermes `config.yaml` draft plus the instance settings that produce it |
| `freeform` | anything else |

The console's quick-task aliases (`write_skill`, `draft_soul`, `env_allowlist`,
`explain_error`, `config_yaml`) are accepted and mapped onto the table above.

The model is asked for exactly one JSON object `{"message": …, "proposal": …}`,
optionally inside a single JSON code fence. A plain-text answer is accepted
too: it becomes the message and no proposal is offered
(`{"choices":[{"message":{"content":"…"}}]}` still works with any
OpenAI-compatible server that ignores the instruction). A JSON-looking answer
that cannot be parsed is never shown raw: it is repaired or reported as
`422 assistant_reply_unusable`.

## Privacy and redaction

The context is built server-side and redacted **before** it leaves the process:

* the provider secret and every environment value flagged `secret` on the
  instance in scope are replaced by `[REDACTED]`;
* values matching `sk-…`, `flt_…` and `Bearer …` are replaced by `[REDACTED]`,
  in the context and in every history message (a pasted credential never
  reaches the model);
* binary content (`NUL` or a majority of control characters) is never sent;
* `file_content` is capped at 32 KiB, `selection` and `schema` at 8 KiB, and
  each truncation is reported.

The response carries the exact contract in `redaction.sent` (what was sent,
with its byte count) and `redaction.omitted` (what was withheld and why), plus
the number of replacements in `redaction.redacted`. The panel can display it
verbatim. Nothing secret is logged: the debug log prints the outgoing JSON
after redaction, and the HTTP client puts the provider secret in a header only.

## Budget

`FLEETT_ASSISTANT_BUDGET_TOKENS` caps the tokens of one **session**
(`session_id`): the prompt estimate plus the full reply allowance has to fit,
otherwise the route answers `429 assistant_budget_exceeded` with
`details.used_tokens` and `details.budget_tokens`, before calling the provider.
Counters live in memory and are reset by a restart; `0` disables the limit.

## Provider call

```
POST ${base_url}/chat/completions
Authorization: Bearer <provider secret>
Content-Type: application/json
<provider extra_headers, one line each>
x-opencode-session: <session id>        (opencode.ai gateways only)

{
  "model": "<provider model, else FLEETT_ASSISTANT_MODEL>",
  "messages": [{"role": "system", …}, …history…],
  "temperature": 0.2,
  "max_tokens": 4096
}
```

`max_tokens` is omitted entirely when
`FLEETT_ASSISTANT_MAX_OUTPUT_TOKENS=0`; it is clamped to that value when the
request supplies its own `max_tokens`.

The system message holds the instructions and the redacted console context.

Header precedence: the credential and `content-type` first, then every
`extra_headers` of the provider (a configured `authorization` overrides the
Bearer, which is how a proxy with its own scheme is wired), then
`x-opencode-session` when the `base_url` host contains `opencode.ai` **and** the
operator did not configure that header. The automatic value derives from the
request `session_id` — stable for one conversation, sanitised to
`[A-Za-z0-9._:-]`, at most 128 characters — so the gateway routes every turn of
the same panel the same way.

The client timeout is 60 s. A non-2xx answer becomes
`502 assistant_provider_error`; the upstream status **and** its body (up to
1 KiB, redacted with the provider secret, the header values, `sk-…`, `flt_…` and
`Bearer …`) are returned and logged at WARN:

```json
{
  "error": {
    "code": "assistant_provider_error",
    "message": "the assistant provider answered with HTTP 400",
    "details": {
      "provider_status": 400,
      "upstream": {
        "status": 400,
        "body": "{\"type\":\"error\",\"error\":{\"type\":\"MissingSessionID\",\"message\":\"Request is missing x-opencode-session…\"}}"
      }
    }
  }
}
```

The body is quoted because it names the cause (`MissingSessionID`, an unknown
model, a quota); the redactor runs over it first, so a credential echoed by the
gateway never reaches the console or the log.

## Apply path and audit

The assistant never writes. When the operator clicks **Apply**, the console
calls the regular route with the header:

```
X-Fleett-Assistant: 1
```

The write behaves exactly like a hand edit (ETag check, `.bak` copy, git
commit) and its audit row gains `details.via: "assistant"`, on
`instance.file.write`, `instance.file.delete`, `instance.env.update`,
`instance.env.delete` and `instance.patch`. Rejecting a proposal touches
nothing.

Every chat turn appends one `assistant.chat` audit row with the task, the model
and the token counts only — never the prompt, the answer or the context:

```json
{"task": "skill", "model": "deepseek-v4.1-flash", "prompt_tokens": 412, "completion_tokens": 188, "total_tokens": 600}
```

## Limitations

* The conversation history is sent by the console on every request; the server
  keeps only the token counters (no server-side transcript).
* `config.yaml` and `healthcheck.py` are regenerated from the instance
  settings, so the assistant explains which setting to change instead of
  proposing those files.
* No streaming: one request, one complete answer.
