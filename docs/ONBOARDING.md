# Onboarding and doctor

A fresh host has no Docker access, no agent image, no provider and no instance. Instead of
showing an empty fleet, fleett answers two read-only endpoints: the **doctor** checklist
(everything that can be wrong on the host) and the **onboarding** steps (the five
prerequisites of the first instance, in order).

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
- `link` is the console page that repairs the check in one click, when the fix is a UI action
  (today: `/providers?new=1` and `/providers?assistant=1` for the `assistant` check).
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
| `assistant` | Resolved assistant provider: `FLEETT_ASSISTANT_PROVIDER`, else the console selection, else the only provider of the fleet. `FLEETT_ASSISTANT=off` is `ok` | `warn`: `no_provider` (empty fleet) or `not_selected` (several providers, none chosen); `error`: the env variable or the stored selection names a deleted provider | select one: `PATCH /api/assistant/config` (link `/providers?assistant=1`); create one: `POST /api/providers` (link `/providers?new=1`) |
| `instances` | Number of instances | never | — |
| `permissions` | `FLEETT_ROOT` is writable (probe file, deleted immediately); with `FLEETT_DRIVER=compose`, the Docker socket is reachable (`DOCKER_HOST`, else `/var/run/docker.sock`) | `error`: root not writable or socket unreachable | `sudo chown -R fleett:fleett <root>` / `sudo usermod -aG docker fleett` |
| `disk` | Free space of the filesystem holding `FLEETT_ROOT`, from `df -B1` | `warn` below 10 % free, `error` below 5 % | free space or move `FLEETT_ROOT` |
| `dns` | `/etc/resolv.conf` declares at least one `nameserver`. No lookup is made | `error`: file unreadable or no nameserver | write a `nameserver` line |

The egress marker is *stale* when it was installed by a different fleett version than the one
running: the rules may not match the current release, so the checklist asks for a reinstall.
`egress-policy` is `ok` when no explicit policy exists: that is the normal legacy state, not a
degradation. See [EGRESS.md](EGRESS.md) for the proxy deployment recipe.

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
    {"id": "assistant", "title": "Select the assistant provider", "done": true,
     "description": "The console assistant drafts with provider \"openai\" (the only provider of the fleet)."},
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
  - `assistant` — a provider is resolved for the assistant (env, persisted selection, or the
    only provider of the fleet); `done` when `FLEETT_ASSISTANT=off`. The description names the
    resolved provider, or says why none is selected, and the action opens the selector
    (`/providers?assistant=1`) or the create form (`/providers?new=1`);
  - `instance` — at least one instance exists.
- `action` tells the console what to offer on an unfinished step, and is **absent once the
  step is done**:
  - `{"kind": "link", "to": "/providers?new=1"}` — internal navigation (the page opens its
    create form);
  - `{"kind": "command", "command": "docker build -t … /path/to/hermes-official"}` — a shell
    command for the host, shown with a copy button.
- `complete` is `true` as soon as **one instance exists**, whatever the other steps say: the
  tour stops appearing for an established fleet. Creating an instance without a provider
  completes the tour with `provider.done = false` and `assistant.done = false`, which is
  allowed (per-instance tokens still work, the assistant then shows its own hint). Deleting
  the last instance brings the tour back.
- The `assistant` step is a hint, not a gate: `complete` never depends on it (issue #63), but
  it makes the otherwise silent "the assistant has no provider" state visible before the
  operator leaves the tour.

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
  parsing, uptime and byte formatting, "warnings never fail the report".
- `backend/src/onboarding.rs` — `action` serialisation and the omitted `action` on done steps.
- `backend/tests/onboarding.rs` — full doctor report shape and statuses with the mock driver,
  missing/present image, missing encryption key, plaintext secrets, egress `off`/missing/stale
  marker, disk on a temp root, a failing check reported inline (no `500`), assistant resolution
  (`auto`, `not_selected`, `setting`, broken reference), onboarding from an empty host to a
  complete fleet (`0/5 → 2/5 → 4/5 → complete`), viewer access, anonymous rejection.
