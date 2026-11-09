# Packaging — container image and Debian package

Two artifacts ship the control plane: an OCI image (`Dockerfile`) and a `.deb`
(`packaging/`). Both embed the web console in the binary, so neither needs
`web/dist` at runtime.

| Artifact | Built by | Published as |
|---|---|---|
| Container image | `Dockerfile` (tag `v*` → `container` job) | `ghcr.io/cleeryy/fleett:<version>`, `:latest` |
| `.deb` | `packaging/build-deb.sh <version> [arch]` (tag `v*` → `deb` job) | release asset `fleett_<version>_amd64.deb` (+ `.sha256`) |
| Tarball | `release` workflow (unchanged) | `fleett_<version>_<target>.tar.gz` |

The package and the image install the same binary and the same defaults. They
differ in how the process gets its data root and its Docker access:

| | `.deb` | container |
|---|---|---|
| User | `fleett` (UID/GID 10000, created by `postinst`) | `10000:10000` (image default) |
| Data root | `/var/lib/fleett` (0700 `fleett:fleett`) | `/data` (`VOLUME /data`) |
| Docker socket | `SupplementaryGroups=docker` | `-v /var/run/docker.sock` + `--group-add <gid>` |
| Secrets key | `/etc/fleett/secrets.key` (created 0640 `root:fleett`) | mount it, or set `FLEETT_SECRETS_KEY` |
| Bind | `127.0.0.1:8090` | `0.0.0.0:8090` + publish on loopback |

## Container image

### Build

```bash
docker build -t fleett:test .
```

Three stages: `node:22-alpine` builds `web/` (`pnpm install --frozen-lockfile`,
`pnpm build`), `rust:1-slim-trixie` + `cmake` (libgit2 is vendored and compiled
from source by the `git2` crate used for instance history) builds the backend
with `--release --locked --features embed-web` after copying `web/dist` to
`../web/dist` (where `rust-embed` looks), and `debian:trixie-slim` is the
runtime: `ca-certificates`, `curl` (healthcheck), `docker-cli` and
`docker-compose` — the compose driver shells out to `docker compose`, so the
image is useless without them.

### Smoke test (mock driver, no Docker socket)

```bash
docker run -d --name fleett-test \
  -e FLEETT_DRIVER=mock \
  -e FLEETT_TOKEN=test \
  -e FLEETT_BIND=0.0.0.0:8090 \
  -p 127.0.0.1:18090:8090 \
  -v fleett-test-data:/data \
  fleett:test

curl -fsS http://127.0.0.1:18090/api/health
curl -fsS http://127.0.0.1:18090/ | head -5     # embedded console
docker rm -f fleett-test && docker volume rm fleett-test-data
```

`FLEETT_BIND=0.0.0.0:8090` is required: a published port reaches the container
interface, never the container's own loopback, and the default bind is
`127.0.0.1:8090`. The healthcheck runs inside the container and works either
way.

### Real deployment (compose driver, Docker socket)

```bash
SOCKET_GID="$(stat -c '%g' /var/run/docker.sock)"

docker run -d --name fleett \
  --restart unless-stopped \
  -v /var/run/docker.sock:/var/run/docker.sock \
  --group-add "$SOCKET_GID" \
  -v /var/lib/fleett:/var/lib/fleett \
  -v /etc/fleett/secrets.key:/etc/fleett/secrets.key:ro \
  -e FLEETT_ROOT=/var/lib/fleett \
  -e FLEETT_BIND=0.0.0.0:8090 \
  -e FLEETT_DRIVER=compose \
  -e FLEETT_EGRESS=off \
  -e FLEETT_TOKEN="$(cat /etc/fleett/token)" \
  -p 127.0.0.1:8090:8090 \
  ghcr.io/cleeryy/fleett:latest
```

Three things that bite:

1. **Socket GID.** The process runs as UID 10000; it can only open the socket
   if its supplementary groups include the socket's GID
   (`--group-add "$(stat -c '%g' /var/run/docker.sock)"`, typically 998 on
   Debian). `--user 10000:10000` is already the image default. The other way
   round is `--user "10000:$(stat -c '%g' /var/run/docker.sock)"`: the socket
   GID becomes the primary group and `--group-add` is not needed — either way
   the UID stays 10000 so the bind-mounted data root stays writable.
2. **Path parity, not a named volume.** Generated `compose.yaml` files use
   relative volumes (`./data:/opt/data`, `../../shares/x:/opt/data/shared/x`).
   The CLI resolves them *inside this container*, and the daemon then mounts
   that path *on the host*. Bind the data root at the same absolute path
   (`-v /var/lib/fleett:/var/lib/fleett -e FLEETT_ROOT=/var/lib/fleett`); the
   default `/data` volume is fine for the mock driver and for a UI trial, not
   for managing agents.
3. **Egress firewall.** `FLEETT_EGRESS=restrict` (the default) requires the
   marker written by `fleett egress install` on the *host*. Inside a container
   the rules are the host's business: either mount
   `/etc/fleett/egress.json:ro` and install the rules on the host, or set
   `FLEETT_EGRESS=off` and keep the firewall out of band (`docs/EGRESS.md`).

`FLEETT_TOKEN` on the command line is visible in `docker inspect`; prefer
`--env-file /etc/fleett/container.env` (0600) with `FLEETT_TOKEN=...` in it. The
secrets key mount needs the file to exist on the host first: the `.deb` creates
it, otherwise run `fleett secrets init` (or copy it from another host).

The secrets key lives outside the data root on purpose. Mount
`/etc/fleett/secrets.key` read-only (0640 `root:fleett` on the host is readable
by GID 10000) or pass `FLEETT_SECRETS_KEY`; without it, instances that carry
secrets answer `409 secrets_key_missing`.

### Pull the published image

```bash
docker pull ghcr.io/cleeryy/fleett:latest        # or :<version>
# private package: docker login ghcr.io -u <github-user>   (PAT, read:packages)
```

The workflow pushes on every `v*` tag with `GITHUB_TOKEN` and
`permissions: packages: write`. If the package stays private, make it public in
the repository's *Packages* settings or authenticate before pulling.

## Debian package

### Build

```bash
packaging/build-deb.sh 0.3.0          # amd64 (dpkg --print-architecture)
packaging/build-deb.sh 0.3.0 arm64
```

The script builds `web/` and the backend (`--features embed-web`), stages
`fleett_<version>_<arch>/`, writes `DEBIAN/control` (+ `Installed-Size`),
`conffiles` and the maintainer scripts, then runs `dpkg-deb --build`. The
result lands in `dist/` with a `.sha256`. `FLEETT_BINARY=/path/to/fleett`
reuses an existing build (it must be an `embed-web` build).

Build host: a C toolchain and `cmake` are required — the `git2` crate vendors
libgit2 and compiles it. The library is linked statically into `/usr/bin/fleett`,
so the `.deb` keeps zero runtime dependency and never calls a host `git`.

Contents:

```
/usr/bin/fleett                             0755
/lib/systemd/system/fleett.service          0644
/etc/fleett/fleett.env                      0640 conffile, root:fleett after postinst
/etc/fleett/secrets.key                     created by postinst, 0640 root:fleett
/usr/share/doc/fleett/README.md             0644
/usr/share/doc/fleett/copyright             0644
```

`Depends: docker.io | docker-ce | moby-engine, adduser, ca-certificates`.

### Install

```bash
sudo apt install ./fleett_0.3.0_amd64.deb
```

`postinst`:

- creates the `fleett` group and user, UID/GID **10000**, home
  `/var/lib/fleett`, shell `/usr/sbin/nologin`;
- adds `fleett` to the `docker` group (`usermod -aG docker fleett`); if Docker
  is installed afterwards, re-run that command by hand;
- creates `/var/lib/fleett` in 0700 and `/etc/fleett` in 0750 `root:fleett`;
- creates `/etc/fleett/secrets.key` **only if absent**, base64 of 32 random
  bytes (`openssl rand -base64 32`, matching `fleett secrets init`), mode 0640
  `root:fleett`;
- runs `systemctl daemon-reload` — but **never enables or starts** the unit.

Then:

```bash
sudoedit /etc/fleett/fleett.env         # set FLEETT_TOKEN (openssl rand -hex 32)
sudo systemctl enable --now fleett
curl -fsS http://127.0.0.1:8090/api/health
```

Two settings are enough to start (`FLEETT_TOKEN` plus a working Docker group);
the conffile already pins `FLEETT_ROOT=/var/lib/fleett`,
`FLEETT_BIND=127.0.0.1:8090` and `FLEETT_DRIVER=compose`. Build the agent image
once (`docs/DEPLOY.md`) and point `FLEETT_IMAGE` at it.

The unit runs `User=fleett`, `Group=fleett`, `SupplementaryGroups=docker`,
`EnvironmentFile=/etc/fleett/fleett.env`, `ExecStart=/usr/bin/fleett serve`,
`Restart=on-failure`, `UMask=0077`, `NoNewPrivileges=true`, `After=docker.service`.
No `ProtectSystem`/`ProtectHome`/`PrivateDevices`: they would cut the Docker
socket and the data root.

### Secrets key

`/etc/fleett/secrets.key` decrypts the provider keys and bot tokens of every
instance. It is **outside** `FLEETT_ROOT` so a backup of the data root never
carries it. Back it up separately; if it is lost, the encrypted secrets are
unrecoverable (the instances themselves keep running). Rotate with
`fleett secrets init` on a clean host and re-enter the secrets.

### Upgrade

```bash
sudo apt install ./fleett_0.4.0_amd64.deb
sudo systemctl restart fleett           # the running process keeps the old inode
```

`prerm` does not stop or disable the unit on upgrade, and `postinst` keeps the
existing conffile (a modified `/etc/fleett/fleett.env` is preserved; dpkg asks
when the packaged template changed), the data root and the secrets key. Agent
containers are supervised by Docker and are not restarted by a control-plane
upgrade.

### Uninstall

```bash
sudo systemctl disable --now fleett     # optional: dpkg -r does it too
sudo dpkg -r fleett                     # stops+disables, keeps data and key
sudo dpkg -P fleett                     # purge: also drops /etc/fleett/fleett.env
```

`/var/lib/fleett` and `/etc/fleett/secrets.key` survive both `remove` and
`purge` (the key is created by `postinst`, not shipped in the archive). Delete
them by hand to erase the host.

### Validation without installing

```bash
dpkg-deb --info dist/fleett_0.3.0_amd64.deb
dpkg-deb --contents dist/fleett_0.3.0_amd64.deb
dpkg-deb --extract dist/fleett_0.3.0_amd64.deb /tmp/fleett-deb   # inspect the tree
```

`dpkg-deb` is only needed to build and inspect; `build-deb.sh` does not need
root (`--root-owner-group`).

## CI

`release.yml` gains two jobs on top of the existing binary matrix:

- `deb` (ubuntu-24.04): builds the web, the backend and the package with
  `packaging/build-deb.sh "${GITHUB_REF_NAME#v}" amd64`, inspects it, uploads
  `dist/fleett_*` as the `deb-amd64` artifact; the `release` job attaches it to
  the GitHub release next to the tarballs;
- `container`: `docker/build-push-action` on `Dockerfile`, `linux/amd64`,
  tags `ghcr.io/cleeryy/fleett:<version>` and `:latest`, `packages: write`.

The tarball matrix and the `release` job are unchanged apart from `deb` being
added to the release job's `needs`.
