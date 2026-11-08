# Stand-in for the Hermes agent image, used only by the Compose driver
# integration job (see .github/workflows/compose-integration.yml and
# docs/CI.md). The real image is hundreds of megabytes and needs a working
# gateway control socket, none of which the CI scenario exercises: the driver
# only requires a container that starts with `gateway run`, stays up and writes
# to its logs.
FROM alpine:3.22

# `render::render_compose` runs `command: ["gateway", "run"]`, so the program
# must exist in PATH and tolerate arguments. It traps TERM/INT so that
# `docker compose stop` is a clean exit instead of a 10 s SIGKILL, and it
# prints a stable marker that the CI scenario asserts through `GET /logs`.
RUN printf '%s\n' \
        '#!/bin/sh' \
        'set -eu' \
        'echo "fleett-test-agent: gateway starting (args: $*)"' \
        'trap "echo fleett-test-agent: gateway stopping; exit 0" TERM INT' \
        'while :; do sleep 1; done' \
        > /usr/local/bin/gateway \
    && chmod 0755 /usr/local/bin/gateway

# No python3 on purpose: the generated healthcheck runs `python3
# /opt/data/healthcheck.py`, which needs the gateway control socket anyway.
# Health therefore ends up `unhealthy`; the API status stays `running`.
CMD ["gateway", "run"]
