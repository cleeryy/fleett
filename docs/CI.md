# CI — Compose driver integration

`.github/workflows/compose-integration.yml` runs the real Compose driver against the
Docker daemon of the runner, on every push to `master` and on every pull request. The
`backend` job of `ci.yml` covers the HTTP API through the mock driver; this job is the only
one that executes the generated `compose.yaml` with `docker compose`, so a regression in the
rendering (a field Compose rejects, a broken volume or network reference) fails CI instead of
the next deployment.

## API contract gates

`openapi.json` is the advertised contract (`GET /api/openapi.json`) and two independent gates
keep it honest:

1. **Route coverage (Rust).** `api::openapi::tests::every_router_route_is_documented_and_nothing_else_is`
   runs with `cargo test` in the `backend` job. It scans the `fn api_routes(…)` region of
   `backend/src/api/mod.rs`, extracts every `(METHOD, /api/path)` pair and compares it with
   the embedded `openapi.json` in both directions. A route added without documentation, or a
   documented operation the router no longer serves, fails the build with the exact
   `METHOD /path` pairs in the panic message. There is no allowlist.
   The sibling tests `the_served_version_is_the_binary_version` and
   `the_version_is_injected_regardless_of_the_stored_value` also prove that
   `info.version` is always `CARGO_PKG_VERSION`, never the literal stored in the file.
2. **Spec validation (Python).** The `spec` job installs `openapi-spec-validator` and runs it
   on `openapi.json`; the same job installs the SDK dependencies and runs
   `pnpm typecheck` in `sdk/typescript`, because the typed client mirrors this document.

Neither gate changes the existing `backend` or `web` jobs: the Rust test runs inside the
normal `cargo test --locked` step, and the `spec` job is additive.

## What the job covers

1. `docker build -t fleett-test-agent tests/compose` builds the stand-in image.
2. `cargo build --release --locked` builds the control plane (no web build: the `embed-web`
   feature is off by default).
3. `fleett serve` starts with a throwaway root and the Compose driver:

   ```text
   FLEETT_ROOT=$RUNNER_TEMP/fleett  FLEETT_BIND=127.0.0.1:8090
   FLEETT_DRIVER=compose            FLEETT_IMAGE=fleett-test-agent
   FLEETT_EGRESS=off                FLEETT_TOKEN=ci-token
   ```

4. `POST /api/instances` creates the instance `solo` (`guest` template). The create call runs
   `docker compose up -d`, and the job polls `GET /api/instances/solo` until the status is
   `running` (at most 60 s, 2 s apart).
5. `docker compose -f "$FLEETT_ROOT/instances/solo/compose.yaml" config` must exit 0: the
   rendered document is parsed and validated by Compose itself.
6. `GET /api/instances/solo/logs?tail=10` must return the marker printed by the stand-in
   (`fleett-test-agent: gateway starting (args: run)`), which proves the logs path of the
   driver actually reads the container.
7. `POST /stop` then `DELETE ?purge=true`; the job then asserts that
   `GET /api/instances/solo` answers `404`, that `docker ps -a` has no `fleett-solo`
   container, that `docker compose ls --all` no longer lists the `fleett-solo` project and
   that `instances/solo` is gone from the data root.

Any non-2xx answer, any missing expected state, fails the step (and the job). The job is
capped at three minutes for the scenario itself; the job allows ten minutes
(`timeout-minutes: 10`) because a cold release build alone can take over two. A diagnostics step dumps `docker ps -a`,
`docker compose ls --all`, the server log and the container logs when a step fails.
`Swatinem/rust-cache` caches `backend/target` plus the Cargo registry keyed by `Cargo.lock`,
which keeps the warm run well inside the cap; a cache miss (new branch, dependency bump) is
the slow path, where the release build alone can approach two minutes.

## Why a stand-in image

`tests/compose/Dockerfile` builds a ~10 MB Alpine image whose
`/usr/local/bin/gateway` is a five-line shell script: it prints one marker line, traps
`TERM`/`INT` and sleeps forever. The Compose service runs `command: ["gateway", "run"]`, so
the binary must exist in `PATH` and accept arguments — nothing more.

The real `hermes-agent` image (several hundred megabytes, built from the Hermes fork) is not
published, takes minutes to pull or build, and needs the gateway control socket to be useful.
CI only needs a process that starts, stays up, logs and stops; using the real image would
make the three-minute budget impossible and couple the workflow to a private artifact.

## Known limitation

Health stays `starting`, then `unhealthy`: the generated healthcheck runs
`python3 /opt/data/healthcheck.py`, which asks the gateway to `identify` itself over
`/opt/data/gateway.sock`. The stand-in implements neither the socket nor a Python
interpreter, so the probe can never succeed (`last_error` may show the probe message once the
container is unhealthy). The job therefore asserts on `status` (`running`, `stopped`) and on
the logs, never on `health`.

Also out of scope: what the real gateway does with `config.yaml`, provider calls, channel
bots, `FLEETT_EGRESS=restrict` (the job runs with `off`), encrypted secrets at rest (the
stand-in instance has no token) and OIDC. Those are covered by their own docs and manual
checks.

## Run it locally

Prerequisites: Docker with the Compose plugin, Rust stable, `curl` and `python3`. The
commands mirror the workflow; run them from the repository root.

```bash
# 1. stand-in image
docker build -t fleett-test-agent tests/compose

# 2. control plane
(cd backend && cargo build --release --locked)

# 3. throwaway data root and server (keep it in the background)
export FLEETT_ROOT="$(mktemp -d)"
export FLEETT_BIND=127.0.0.1:8090
export FLEETT_DRIVER=compose
export FLEETT_IMAGE=fleett-test-agent
export FLEETT_EGRESS=off
export FLEETT_TOKEN=ci-token
export FLEETT_URL=http://127.0.0.1:8090
export AUTH="Authorization: Bearer $FLEETT_TOKEN"

./backend/target/release/fleett serve &
SERVE_PID=$!

# 4. scenario
curl -fsS -X POST "$FLEETT_URL/api/instances" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  --data '{"id":"solo","name":"Local stand-in","template":"guest","model":"deepseek-v4.1-flash","base_url":"https://opencode.ai/inference/go/openai/v1"}'

curl -fsS "$FLEETT_URL/api/instances/solo" -H "$AUTH"
docker compose -f "$FLEETT_ROOT/instances/solo/compose.yaml" config
curl -fsS "$FLEETT_URL/api/instances/solo/logs?tail=10" -H "$AUTH"

curl -fsS -X POST "$FLEETT_URL/api/instances/solo/stop" -H "$AUTH"
curl -fsS -X DELETE "$FLEETT_URL/api/instances/solo?purge=true" -H "$AUTH"

# 5. assertions
docker ps -a --filter name=fleett-solo
docker compose ls --all
test ! -e "$FLEETT_ROOT/instances/solo"

# 6. cleanup
kill "$SERVE_PID"
rm -rf "$FLEETT_ROOT"
```

Notes:

- The test creates the shared `fleett` Docker network on first start (the control plane does
  it when the Compose driver is selected). It is left in place between runs on purpose;
  remove it with `docker network rm fleett` when nothing uses it.
- If a run is interrupted before the delete step, remove the leftover project with
  `docker compose -f "$FLEETT_ROOT/instances/solo/compose.yaml" down --remove-orphans`.
- Do not point the scenario at a production root: it creates and purges an instance named
  `solo` and expects port `8090` to be free.
