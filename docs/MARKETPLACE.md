# Marketplace — finding and installing skills

The skill registry ([docs/SKILLS.md](SKILLS.md)) is the source of truth for
what an instance mounts, but it only knows what an operator published or
imported by hand. The marketplace adds the other half: a catalogue of upstream
skills, a preview before installation, and a traceable update path.

A skill is untrusted content. Nothing in this page is executed on import.

## Providers

`GET /api/marketplace/skills?q=&source=&limit=` queries every enabled provider
and returns the merged entries plus one status per provider:

```json
{
  "entries": [
    {
      "name": "pdf-tools",
      "description": "Work with PDF documents",
      "provider": "github",
      "source": "github:acme/pdf-tools",
      "url": "https://github.com/acme/pdf-tools",
      "author": "acme",
      "tags": ["pdf"]
    }
  ],
  "sources": [
    { "source": "local", "status": "ok", "count": 3 },
    { "source": "git", "status": "disabled", "count": 0, "message": "FLEETT_MARKETPLACE_GIT_INDEX is not set" },
    { "source": "github", "status": "ok", "count": 12 },
    { "source": "skills.sh", "status": "ok", "count": 4 }
  ]
}
```

| Provider | What it reads | Failure behaviour |
|---|---|---|
| `local` | `$FLEETT_ROOT/marketplace/index.json`, written atomically by `fleett marketplace sync` | `status: "error"`, other providers keep working |
| `git` | `index.json` of the repository named by `FLEETT_MARKETPLACE_GIT_INDEX=owner/repo`, fetched over HTTPS and cached 10 minutes | `disabled` when unconfigured, `error` on a fetch failure |
| `github` | `GET {api}/search/repositories?q=<q> skill` | `error` on a rate limit or outage |
| `skills.sh` | light scrape of `{base}/search?q=<q>`, cached 5 minutes, at most one request per 2 s | `disabled` with `FLEETT_MARKETPLACE_SKILLS_SH=off`; a layout change degrades to `count: 0`, never an exception |

A provider failure never fails the search: the console shows the entries that
did resolve and the status of the ones that did not. `?source=` filters to one
provider (`local`, `git`, `github` or `skills.sh`); an unknown id is a `400
invalid_source`. `?limit=` is `1..=200`, default `50`.

### The index format

`$FLEETT_ROOT/marketplace/index.json` (written only by `sync`, mode `0600`):

```json
{
  "skills": [
    {
      "name": "pdf-tools",
      "description": "Work with PDF documents",
      "source": "github:acme/pdf-tools",
      "ref": "main",
      "url": "https://example.com/pdf-tools.zip",
      "author": "acme",
      "tags": ["pdf"],
      "version": "1.2.0",
      "commit": "9f2c1ab"
    }
  ]
}
```

`source` and `url` are both optional, but one of them must be present for the
entry to be installable. `fleett marketplace sync` fetches the git index and
replaces the file with a temporary file plus `rename(2)`, so a reader never
sees a half-written catalogue.

## Source grammar

| Source | Meaning |
|---|---|
| `local:<name>` or `<name>` | an entry of the local index (bare names are a local lookup) |
| `git:<owner>/<repo>[:<path>]` | a directory of a repository, resolved through an HTTPS archive |
| `github:<owner>/<repo>[:<path>][@<ref>]` | same, with an explicit branch, tag or commit |
| `url:<https://…zip\|…md>` | a zip archive or a single `SKILL.md` |
| `skills.sh:<owner>/<name>` | the upstream repository linked by a skills.sh page |

`git:` and `github:` never run a network `git clone`: the repository archive is
downloaded over HTTPS (8 MiB cap, 3 redirects), validated by the shared zip
reader, then filtered to `<path>/`. GitHub origins resolve through
`FLEETT_MARKETPLACE_API_BASE` (default `https://api.github.com`), so a mirror
or a test fixture can stand in for github.com.

## Install, update, promote

### Preview

`POST /api/marketplace/skills/preview` takes `{source, ref}` and returns the
resolved plan without writing anything:

```json
{
  "name": "pdf-tools",
  "description": "Work with PDF documents",
  "version": "1.2.0",
  "author": "acme",
  "files": [{"path": "SKILL.md", "size": 812, "script": false}],
  "total_bytes": 812,
  "skill_md": "---\nname: PDF Tools\n---\n# PDF Tools\n",
  "scripts": [],
  "commit": "9f2c1ab",
  "source": "github:acme/pdf-tools",
  "requires_confirmation": false
}
```

A file is a script when its extension is one of `.sh .bash .zsh .py .js .mjs
.ts .rb .pl .php .ps1 .bat .cmd .exe .jar` or its content starts with `#!`.
Every authenticated role may preview, but the call performs an outbound fetch
and is rate-limited to 30 per minute per process (`429 rate_limited`).

### Install

`POST /api/marketplace/skills/install` takes `{source, ref, name?,
confirm_scripts?}`. It publishes the **next free version** (`v1` first) with a
provenance, and answers `201` with the new `SkillDetail`.

- `409 scripts_require_confirmation` with `details.scripts` when the archive
  contains scripts and `confirm_scripts` is not `true`.
- `409 skill_version_exists` / `skill_exists` behave exactly like the registry.
- Installs are serialised by one process-wide lock so two concurrent requests
  cannot compute the same "next free version".

The first version of a new skill becomes `latest`; later versions do not move
the pointer.

### Update

`POST /api/marketplace/skills/{name}/update` takes `{promote?, confirm_scripts?}`.
It re-resolves the `source`/`ref` recorded in the installed version, diffs the
files and publishes the next version:

```json
{
  "name": "pdf-tools",
  "from": "v1",
  "to": "v2",
  "commit": "c2",
  "files": [
    {"path": "SKILL.md", "status": "modified"},
    {"path": "notes/forms.md", "status": "added"},
    {"path": "notes/pages.md", "status": "modified"}
  ],
  "promoted": {"latest": "v2", "results": [{"id": "alice", "status": "restarted"}]}
}
```

`409 no_upstream` means the skill was published locally (or imported from a
zip) and has no provenance; `409 upstream_unchanged` means the commit **and**
the content are identical. With `promote: true` the same rolling loop as
`POST /api/skills/{name}/promote` runs: `latest` moves, then every follower
restarts sequentially under its instance lock and must become healthy within
60 seconds. The report is returned under `promoted`.

### Updates

`GET /api/marketplace/updates` lists the installed skills whose upstream commit
differs, cached for 60 seconds:

```json
{
  "updates": [
    {
      "name": "pdf-tools",
      "installed_version": "v1",
      "upstream_version": "1.3.0",
      "commit": "c2",
      "source": "github:acme/pdf-tools",
      "ref": "main"
    }
  ],
  "warnings": []
}
```

A provider outage yields an empty list plus a `warnings` entry — never a `500`.
The cache is invalidated by every install, update and sync.

### Sync

`POST /api/marketplace/sync` (admin) fetches the configured git index and
atomically rewrites the local one:

```json
{"skills": 42, "source": "acme/skills-index", "commit": "…"}
```

`409 no_index_configured` when `FLEETT_MARKETPLACE_GIT_INDEX` is unset.

### Uninstall

`DELETE /api/skills/{name}` is the only uninstall route, and it already refuses
a skill an instance mounts (`409 skill_in_use` with `details.instances`). The
marketplace does not duplicate it.

## Traceability

Every installed version records its origin in `skill.yaml` (all fields
optional, so hand-written files keep working):

```yaml
name: pdf-tools
description: Work with PDF documents
source: github:acme/pdf-tools
ref: main
commit: 9f2c1ab
upstream_version: 1.2.0
author: acme
tags:
  - pdf
installed_at: 2026-11-09T10:00:00Z
```

The same fields are exposed additively by `GET /api/skills`,
`GET /api/skills/{name}` and each `versions[]` entry, so the console can answer
"where did this skill come from?" without reading the filesystem. A marketplace
update threads the provenance through to the new version: it is never erased.

## Security model

- **No execution, ever.** Importing only downloads, validates and writes text
  files. No `build`, no post-install hook, no script run.
- **Read-only mount.** The compose renderer still mounts the registry with
  `:ro`; the agent cannot modify an installed skill.
- **No symlinks.** The shared zip reader refuses symlink entries, and the
  registry refuses symlinks anywhere under a version directory.
- **Zip-slip protected.** Relative paths only, no `..`, no absolute path, no
  backslash; the same checks the zip import always had.
- **Bounded.** Index 1 MiB, archive 8 MiB streamed with a cap, 256 files,
  256 KiB per file, 1 MiB total per version, 3 redirects, 10-second timeout.
- **HTTPS only** by default. Plain HTTP is accepted for loopback
  (`127.0.0.1`, `localhost`, `::1`) and when `FLEETT_MARKETPLACE_INSECURE=1`
  (tests). A skill archive is executable supply-chain content.
- **Scripts require an explicit confirmation**, shown before installation.
- **No secret in a response.** `GITHUB_TOKEN` is never logged nor returned; it
  is only sent to the configured GitHub API origin.

## Environment

| Variable | Default | Effect |
|---|---|---|
| `FLEETT_MARKETPLACE_SKILLS_SH` | `auto` | `off` disables the skills.sh scraper without touching the other providers |
| `FLEETT_MARKETPLACE_GIT_INDEX` | unset | `owner/repo` of the versioned `index.json` |
| `GITHUB_TOKEN` (or `FLEETT_GITHUB_TOKEN`) | unset | raises the GitHub rate limit; never logged nor returned |
| `FLEETT_MARKETPLACE_API_BASE` | `https://api.github.com` | GitHub API origin (mirror or fixture) |
| `FLEETT_MARKETPLACE_SKILLS_SH_BASE` | `https://skills.sh` | skills.sh origin |
| `FLEETT_MARKETPLACE_INSECURE` | `false` | allow plain HTTP downloads from non-loopback hosts |

## Offline behaviour

The marketplace is optional. With no network and no index:

- `GET /api/marketplace/skills` still answers `200`, with the local index (if
  any) and one `error` or `disabled` status per unreachable provider;
- `GET /api/marketplace/updates` answers `200` with an empty list and a warning
  per skill it could not check;
- installs, previews and syncs fail with `502 marketplace_unavailable` (or
  `409 no_index_configured`), never with a hang or a panic;
- `FLEETT_MARKETPLACE_SKILLS_SH=off` removes the only scrape from the picture,
  leaving local/git/GitHub untouched.

## CLI

```sh
fleett marketplace search pdf
fleett marketplace search pdf --source github
fleett marketplace preview github:acme/pdf-tools main
fleett marketplace install github:acme/pdf-tools main
fleett marketplace install github:acme/pdf-tools main --confirm-scripts
fleett marketplace updates
fleett marketplace update pdf-tools --promote
fleett marketplace sync
```

`--json` prints the raw API payload on every command. See
[docs/CLI.md](CLI.md).
