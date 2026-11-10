# Onboarding and doctor

A fresh host has no Docker access, no agent image, no provider and no instance. Instead of
showing an empty fleet, fleett answers two read-only endpoints: the **doctor** checklist
(everything that can be wrong on the host) and the **onboarding** steps (the prerequisites of
the first instance, in order).

Both routes need an authenticated caller (any role, `viewer` included): they expose paths and
counts, never a secret. `GET /api/doctor` is what `fleett doctor` checks from the CLI and what
the console's diagnostics page renders; `GET /api/onboarding` drives the first-run assistant.

| Route | Auth | Purpose |
|---|---|---|
| `GET /api/doctor` | any authenticated role | Host and fleet checklist, one line per check |
| `GET /api/onboarding` | any authenticated role | Five first-run steps and their hint |

Neither route writes anything, calls Docker except through the configured driver, or makes a
network call: the DNS check only reads `/etc/resolv.conf`, so an isolated host still answers.

## `GET /api/doctor`

```json
{
  "ok": false,
  "checks": [
    {"id": "api", "label": "API", "status": "ok", "detail": "fleett 0.8.0, up 3m 4s"},
    {"id": "image", "label": "Agent image", "status": "error",
     "detail": "image \"hermes-agent:0.22.2-local\" (FLEETT_IMAGE) is not in the local Docker image store",
     "fix": "docker build -t hermes-agent:0.22.2-local /path/to/hermes-official"},
    {"id": "egress", "label": "Egress firewall", "status": "warn",
     "detail": "restrict, but the firewall is not installed (no marker at /etc/fleett/egress.json)",
     "fix": "sudo fleett egress install"}
  ]
}
```

- `checks` always contains the same twelve ids, in this order: `api`, `docker`, `image`,
  `secrets`, `egress`, `egress-policy`, `providers`, `assistant`, `instances`, `permissions`,
  `disk`, `dns`.
- `status` is one of:
  - `ok` — the check passed;
  - `warn` — fleett works, but a protection is off or something is missing (warnings never
    fail the report);
  - `error` — the operator must act.
- `ok` is `true` only when **no check is `error`**. A warning (egress off, no provider yet)
  does not fail it.
- `fix` is present when a precise command or setting repairs the check; it is absent on `ok`
  lines and on warnings where no command applies.
- `link` is an **internal console route** that repairs the check (`/providers?assistant=1`),
  for the cases where a click beats a command. `fix` stays the command form; a check may
  carry one, the other, or both.
- `reason` is the machine-readable cause of the line, for the cases the console branches on
  (`no_provider`, `not_selected`); it is absent when `detail` says it all.
- A check that cannot run (database error, unreachable Docker daemon, `df` missing) is
  reported as `error` with its detail. The route never answers `500`.

### The twelve checks

| `id` | Verifies | `warn` / `error` | `fix` |
|---|---|---|---|
| `api` | Version and control-plane uptime | never | — |
| `docker` | The driver answers `ping`. `FLEETT_DRIVER=mock` is reported as *simulated* and is `ok` | `error`: no answer | `sudo systemctl start docker && docker info` |
| `image` | `FLEETT_IMAGE` is present in the driver's local image store (`docker images`) | `error`: tag absent or the daemon unreachable | `docker build -t <image> /path/to/hermes-official` |
| `secrets` | An encryption key is configured; counts encrypted instances and leftover plaintext `secrets.env` files | `warn`: plaintext files remain; `error`: no key | `sudo fleett secrets init` (missing key) / `fleett secrets migrate --apply` (plaintext) |
| `egress` | `FLEETT_EGRESS` mode and the marker written by `fleett egress install` | `warn`: mode `off`, marker missing/unreadable, or marker written by another fleett version (stale) | `sudo fleett egress install` |
| `egress-policy` | Stored destination policies and the proxy they render. At most one TCP probe per distinct proxy (750 ms) | `error`: a policy renders a proxy that does not answer (every connection of that instance fails); `warn`: a policy needs a proxy but no address is configured | `docker compose -f docker/fleett-proxy.example.yaml up -d` / set `FLEETT_EGRESS_PROXY` or remove the policy |
| `providers` | Number of providers in the database | `warn`: zero | — |
| `assistant` | The provider the assistant borrows: `FLEETT_ASSISTANT_PROVIDER`, the selection stored by `PATCH /api/assistant/config`, else a single provider (`auto`) | `warn`: `reason: no_provider` (nothing to borrow) or `reason: not_selected` (several providers, none selected); `error`: the reference names a deleted provider | `link` for the selection/create page; `fix` only for a broken env reference (`set FLEETT_ASSISTANT_PROVIDER to an existing provider id, or unset it`) |
| `instances` | Number of instances | never | — |
| `permissions` | `FLEETT_ROOT` is writable (probe file, deleted immediately); with `FLEETT_DRIVER=compose`, the Docker socket is reachable (`DOCKER_HOST`, else `/var/run/docker.sock`) | `error`: root not writable or socket unreachable | `sudo chown -R fleett:fleett <root>` / `sudo usermod -aG docker fleett` |
| `disk` | Free space of the filesystem holding `FLEETT_ROOT`, from `df -B1` | `warn` below 10 % free, `error` below 5 % | free space or move `FLEETT_ROOT` |
| `dns` | `/etc/resolv.conf` declares at least one `nameserver`. No lookup is made | `error`: file unreadable or no nameserver | write a `nameserver` line |

The egress marker is *stale* when it was installed by a different fleett version than the one
running: the rules may not match the current release, so the checklist asks for a reinstall.
`egress-policy` is `ok` when no explicit policy exists: that is the normal legacy state, not a
degradation. See [EGRESS.md](EGRESS.md) for the proxy deployment recipe.

`assistant` is `ok` as soon as a provider is resolved (`env`, `setting` or `auto`), and also
when `FLEETT_ASSISTANT=off`: a disabled panel is a deliberate choice, not a degradation. A
deleted provider is an `error`, never a silent `null` — see
[ASSISTANT.md](ASSISTANT.md#which-provider-answers-issue-63).

## `GET /api/onboarding`

```json
{
  "complete": false,
  "steps": [
    {"id": "docker", "title": "Check Docker", "done": true,
     "description": "fleett drives Docker Compose; the engine must answer on this host."},
    {"id": "image", "title": "Build the agent image", "done": false,
     "description": "The agent image \"hermes-agent:0.22.2-local\" (FLEETT_IMAGE) must exist in the local image store.",
     "action": {"kind": "command", "command": "docker build -t hermes-agent:0.22.2-local /path/to/hermes-official"}},
    {"id": "provider", "title": "Create a provider", "done": false,
     "description": "A provider stores one API credential the instances reuse; its test button checks the key before the first instance starts.",
     "action": {"kind": "link", "to": "/providers?new=1"}},
    {"id": "assistant", "title": "Select the assistant provider", "done": false,
     "description": "The assistant borrows the secret of a provider; create one, then it is used automatically.",
     "action": {"kind": "link", "to": "/providers?new=1"}},
    {"id": "instance", "title": "Create your first instance", "done": false,
     "description": "Pick a template, attach the provider and start the agent; the instance page shows its health, logs and allowlist variables.",
     "action": {"kind": "link", "to": "/instances?new=1"}}
  ]
}
```

- The steps are always the same five, in order: `docker`, `image`, `provider`, `assistant`,
  `instance`.
- `done` is computed live:
  - `docker` — the driver answers `ping` (the mock driver answers `true`);
  - `image` — `FLEETT_IMAGE` is in the local image store (the same probe as the `image`
    check);
  - `provider` — at least one provider exists;
  - `assistant` — a provider is resolved for the assistant (`env`, the stored selection or
    `auto`, i.e. the single provider just created). `done` is `false` with
    `action.to = "/providers?assistant=1"` when several providers exist and none is selected,
    and with `/providers?new=1` when none exists at all;
  - `instance` — at least one instance exists.
- `action` tells the console what to offer on an unfinished step, and is **absent once the
  step is done**:
  - `{"kind": "link", "to": "/providers?new=1"}` — internal navigation (the page opens its
    create form);
  - `{"kind": "command", "command": "docker build -t … /path/to/hermes-official"}` — a shell
    command for the host, shown with a copy button.
- `complete` is `true` as soon as **one instance exists**, whatever the other steps say: the
  assistant stops appearing for an established fleet. Creating an instance without a provider
  completes the tour with `provider.done = false` (and `assistant.done = false`), which is
  allowed (per-instance tokens still work). Deleting the last instance brings the tour back.

Nothing is stored: the two routes derive everything from Docker, the database and the
filesystem, so an instance created over the API (or by `fleett create`) also completes the
tour.

## Scripting

```bash
# Fail a cron job when the host is not ready (`ok` is false when a check is an error).
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" https://fleett.example.test/api/doctor \
  | jq -e '.ok' >/dev/null || echo "fleett needs attention"

# One line per failing check, with its fix.
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" https://fleett.example.test/api/doctor \
  | jq -r '.checks[] | select(.status == "error") | "\(.id): \(.detail) → \(.fix // "no automatic fix")"'

# Is the first-run assistant still needed?
curl -fsS -H "Authorization: Bearer $FLEETT_TOKEN" https://fleett.example.test/api/onboarding \
  | jq -e '.complete'
```

The CLI keeps its own checklist in `fleett doctor` (it also reads `/api/health`, the identity
and the per-instance warnings) and exits `1` when a check fails:

```bash
fleett doctor           # human-readable table
fleett --json doctor    # machine-readable, same exit code
```

The console renders `GET /api/doctor` on its diagnostics page and `GET /api/onboarding` as the
first-run assistant; the `action` objects above are the only UI contract, so an external
client can drive the same flow without knowing the console routes.

## Tests

- `backend/src/doctor.rs` — disk thresholds (10 %/5 %), `df` parsing on a temp root, DNS
  parsing, uptime and byte formatting, optional `link`/`reason` fields, "warnings never fail
  the report".
- `backend/src/assistant.rs` — provider resolution (`env` > `setting` > `auto`, `no_provider`,
  `not_selected`, broken reference with the valid ids), `provider_source`/`reason`
  serialisation.
- `backend/src/onboarding.rs` — `action` serialisation and the omitted `action` on done steps.
- `backend/tests/onboarding.rs` — full doctor report shape and statuses with the mock driver,
  missing/present image, missing encryption key, plaintext secrets, egress `off`/missing/stale
  marker, disk on a temp root, a failing check reported inline (no `500`), the `assistant`
  check (`no_provider`, `auto`, `not_selected`, broken `env`/`setting`), onboarding from an
  empty host to a complete fleet (`0/5 → 2/5 → 4/5 → complete`), viewer access, anonymous
  rejection.
- `backend/tests/assistant.rs` — auto resolution, `not_selected`, `PATCH` selection and its
  persistence, env precedence, `422 assistant_provider_unknown` with the valid ids, admin
  requirement.
