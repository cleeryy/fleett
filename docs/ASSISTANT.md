# Assistant — contextual drafting copilot

The console ships an assistant panel available on every page. It drafts the
files that are tedious to write by hand — `SKILL.md`, `SOUL.md`, environment
variables, `fleet.yaml`, pipeline YAML — and explains errors in plain language.
It is a **drafting tool**: it never writes anything. Every answer that contains
an applicable change returns a proposal, and the console applies it through the
regular routes (`PUT /files/content`, `PUT /instances/{id}/env`, …), so the
write keeps its ETag check, backup, git commit and audit row.

The whole assistant is server-side; the console only renders the panel, shows
the context summary and calls the two routes below.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FLEETT_ASSISTANT` | `on` | `off` removes every route entirely (`404 route not found`) |
| `FLEETT_ASSISTANT_PROVIDER` | — | id of an existing provider; wins over the selection stored in the console |
| `FLEETT_ASSISTANT_MODEL` | `deepseek-v4.1-flash` | model sent to the provider |
| `FLEETT_ASSISTANT_BUDGET_TOKENS` | `50000` | token budget of one session; `0` disables the limit |

The provider is a **normal provider** created with `POST /api/providers`
(see [PROVIDERS.md](PROVIDERS.md)): kind `openai-compatible`, a base URL such as
`https://opencode.ai/inference/go/openai/v1` and its encrypted secret. The
assistant decrypts that secret per request; it never stores one of its own.

`FLEETT_ASSISTANT=off` is a hard switch: the router does not register the
routes, so they answer the generic `404 not_found` and the console hides the
panel. Nothing else changes; the provider, if one is configured, keeps serving
the instances that reference it.

### Which provider answers (issue #63)

The provider is resolved at request time, in this exact order, and the answer
says where it came from (`provider_source`):

| Order | Source | `provider_source` |
|---|---|---|
| 1 | `FLEETT_ASSISTANT_PROVIDER` names an existing provider | `env` |
| 2 | the selection stored by `PATCH /api/assistant/config` | `setting` |
| 3 | exactly one provider exists and nothing selected it | `auto` |
| 4 | otherwise: nothing is resolved, `reason` says why | `none` |

A credential is never guessed: with two providers and no selection the
assistant stays off until one is chosen. The two empty states are distinct, so
the console can show the right message:

| `provider` | `reason` | Meaning | Console |
|---|---|---|---|
| `null` | `no_provider` | the fleet has no provider at all | create form (`/providers?new=1`) |
| `null` | `not_selected` | providers exist, none is selected | picker pre-filled with `GET /api/providers` |

`FLEETT_ASSISTANT_PROVIDER` always wins. A selection made in the console is
still stored (so it takes over the day the variable is removed), but the
response reports `provider_source: "env"`, and the panel says so instead of
pretending the selection applied.

### A reference to a deleted provider

`FLEETT_ASSISTANT_PROVIDER` or the stored selection can name an id that no
longer exists (the provider was deleted). That is a `422`, never a `500` and
never a silent `null`:

```json
{
  "error": {
    "code": "assistant_provider_unknown",
    "message": "the assistant provider \"6f4c…\" (env) does not exist",
    "details": {
      "provider": "6f4c…",
      "source": "env",
      "valid": ["d2b3e0c1-f699-42e8-8de8-94361fb1468b"],
      "env": "FLEETT_ASSISTANT_PROVIDER",
      "providers": "/api/providers",
      "config": "/api/assistant/config",
      "hint": "set FLEETT_ASSISTANT_PROVIDER to an existing provider id, or unset it and select one with PATCH /api/assistant/config"
    }
  }
}
```

`details.valid` is every existing provider id, sorted by name: the console
offers them directly. `GET /api/assistant/config`, `PATCH /api/assistant/config`
and `POST /api/assistant/chat` answer the same error. `details.source` is `env`
or `setting` — the reference that dangles — and `deleted` when the row vanished
between the resolution and the provider call (a race; `valid` is then empty).

### Without a provider

`GET /api/assistant/config` still answers `200` with `"provider": null` and a
`reason`, which is what lets the panel render the right hint instead of an
error. The chat route answers `409 assistant_not_configured`:

```json
{
  "error": {
    "code": "assistant_not_configured",
    "message": "the assistant has no provider selected: several providers exist",
    "details": {
      "reason": "not_selected",
      "providers": "/api/providers",
      "config": "/api/assistant/config",
      "hint": "pick one with PATCH /api/assistant/config {\"provider\": \"<id>\"}"
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
  "provider": {"id": "d2b3e0c1-…", "name": "opencode-go"},
  "provider_source": "auto",
  "reason": null,
  "model": "deepseek-v4.1-flash",
  "budget_tokens": 50000,
  "used_tokens": 1200
}
```

`provider_source` is `env`, `setting`, `auto` or `none`; `reason` is `null` when
a provider is resolved, `no_provider` or `not_selected` otherwise.

### `PATCH /api/assistant/config`

Admin role. Selects the provider the assistant borrows, or clears the selection:

```json
{"provider": "d2b3e0c1-f699-42e8-8de8-94361fb1468b"}
{"provider": null}
```

`provider` is required (`400 invalid_body` without it): a typo can never clear a
working selection. An unknown id answers `422 assistant_provider_unknown` with
`details.valid`; `null` (or an empty string) clears the stored selection and
`auto` takes over again. The answer is the freshly resolved configuration, the
same shape as `GET`, so the console needs no second call. When
`FLEETT_ASSISTANT_PROVIDER` is set, the body is stored but ignored and
`provider_source` stays `env`.

Every call appends one `assistant.config` audit row:
`{"provider": "…", "source": "setting", "env_override": false}`.

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
  "task": "skill"
}
```

`context.page` is required; every other context field is optional. `messages`
is the conversation the console keeps (at most 40 messages, 16 KiB each, roles
`user` and `assistant` only). `task` defaults to `freeform`.

```json
{
  "message": "Here is a draft that sorts invoices by supplier.",
  "proposal": {
    "kind": "file",
    "path": "/SKILL.md",
    "content": "---\nname: invoice-sorter\ndescription: Sort invoices by supplier\n---\n…"
  },
  "usage": {"prompt": 412, "completion": 188, "total": 600},
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

The model is asked for exactly one JSON object `{"message": …, "proposal": …}`.
A plain-text answer is accepted too: it becomes the message and no proposal is
offered (`{"choices":[{"message":{"content":"…"}}]}` still works with any
OpenAI-compatible server that ignores the instruction).

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

{
  "model": "<FLEETT_ASSISTANT_MODEL>",
  "messages": [{"role": "system", …}, …history…],
  "temperature": 0.2,
  "max_tokens": 1200
}
```

The system message holds the instructions and the redacted console context.
The client timeout is 60 s. A non-2xx answer becomes
`502 assistant_provider_error` (`details.provider_status`); the upstream body is
never echoed, because it can quote the prompt or a credential fragment.

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
