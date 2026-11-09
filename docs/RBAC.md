# Per-instance RBAC and token scopes

Before 0.6 the only roles were global: an `admin` could do everything, a
`viewer` could read the whole fleet. Issue #37 adds a level *per instance* and
a scope *per named token*, so an agent can be shared with one person without
opening the rest of the fleet.

## Global roles (unchanged)

| Role | Effect |
|---|---|
| `admin` | Full control of the fleet. Bypasses every ACL. Only an admin may create, import or clone an instance, manage tokens/providers/shares, read the audit log, or upgrade the fleet. |
| `viewer` | Read-only. A viewer **session** sees only the instances an ACL grants it (see below). Writes on an instance are allowed when an ACL grants `operator` or `owner`. |

Bearer tokens also carry a global role (`admin` or `viewer`) chosen at
creation; a token without a `scope` keeps the pre-0.6 behaviour (see
[Compatibility](#compatibility)).

## Instance roles

| Level | Read (GET/HEAD) | Write (POST/PATCH/PUT/DELETE) | Exec | Manage the ACL |
|---|---|---|---|---|
| `owner` | yes | yes | yes | yes |
| `operator` | yes | yes | no | no |
| `viewer` | yes | no | no | no |

`exec` is reserved for a dedicated execution route (`/api/instances/{id}/exec`);
no such route exists yet, but the level and the token action are already
enforced by the authorization helpers and the request classifier.

### Where the level comes from

For one `(caller, instance)` pair, in this order:

1. the global `admin` role → `owner`;
2. an explicit `instance_acl` row whose `subject` matches the caller → that
   role;
3. membership of the instance in a scoped token's `instances` list → at most
   `viewer` (the token's `actions` still gate what it can do);
4. otherwise no access (the request answers `403 forbidden` for a known
   instance, `404` for an unknown id).

A token scope is applied first and always intersects: an `admin` token that
carries a `scope` still only reaches the instances and actions that scope
lists, whatever the ACLs say.

**Subject** is the identity matched against `instance_acl.subject`:

| Credential | Subject |
|---|---|
| OIDC session | the account email, falling back to the OIDC `sub` |
| named bearer token | the token name |
| `FLEETT_TOKEN` (bootstrap) | `bootstrap` (admin, so ACLs do not matter) |
| `FLEETT_ALLOW_NO_AUTH` | `anonymous` (admin) |

`GET /api/auth/me` returns it as `subject`. The audit log records the same
value in `actor`, so the two can be compared when debugging a grant.

## ACL API

```http
GET    /api/instances/{id}/acl
PUT    /api/instances/{id}/acl
DELETE /api/instances/{id}/acl/{subject}
```

Admin or an `owner` of the instance only.

`GET` answers:

```json
{"instance": "probe", "acl": [
  {"subject": "cle@example.test", "role": "owner", "created_at": "2026-11-09T10:00:00Z"},
  {"subject": "ci-robot", "role": "viewer", "created_at": "2026-11-09T10:01:00Z"}
]}
```

`PUT` replaces the whole list:

```json
{"acl": [
  {"subject": "cle@example.test", "role": "owner"},
  {"subject": "ci-robot", "role": "operator"}
]}
```

Rules: at most 100 entries; `subject` non-empty, at most 320 characters, no
control characters; `role` is `owner`, `operator` or `viewer`; a subject may
appear once. A known instance with a bad body answers `400 invalid_acl`; an
unknown instance answers `404`.

`DELETE` removes one row and answers `204`, or `404` when the subject has no row
on that instance.

Every change is written to the audit log:

* `instance.acl.update` — `details.subjects` lists every subject now present,
  `details.acl` carries the `{subject, role}` pairs;
* `instance.acl.delete` — `details.subject` names the removed subject.

Grants are read from SQLite on every request; there is no cache and no
token/session re-mint. Removing a row (or lowering a role) applies to the very
next request. For a scoped token, remember that its `instances` membership
still grants `viewer`: removing an ACL row revokes the extra `operator`/`owner`
level, while revoking read itself means removing the instance from the token
scope (that is, revoking the token: there is no token-update endpoint yet).

### CLI

```bash
fleett acl get probe
fleett acl set probe cle@example.test owner
fleett acl set probe ci-robot viewer
fleett acl rm probe ci-robot
```

`acl set` reads the current list first and upserts one subject; the other
entries are preserved. `--json` prints the API payload unchanged.

## Token scopes

`POST /api/tokens` accepts an optional `scope`:

```json
{"name": "probe-bot", "role": "viewer",
 "scope": {"instances": ["probe"], "actions": ["read", "write"]}}
```

* `instances` — non-empty list of existing instance ids; duplicates dropped.
  Every id must exist, otherwise `400 invalid_scope`.
* `actions` — non-empty subset of `read`, `write`, `exec`; duplicates dropped,
  unknown values answer `400 invalid_scope`.

A scope is stored as JSON in `api_tokens.scope`. `GET /api/tokens` returns it
next to the metadata; the one-time secret is still only returned at creation.
`TokenScope` intersects the ACL: the token never reaches an instance outside
`instances`, and never performs an action outside `actions`, even when its
global role is `admin`. In other words, an `admin` token with a scope is a
deliberately restricted admin token.

A `viewer` token that carries `write` in `actions` still needs an ACL row with
`operator` or `owner`: the scope says *where and what*, the ACL says *how much*.

## Server-side filtering

`GET /api/instances` only returns the instances the caller may read:

* admin (without a scope) → everything;
* scoped token → the intersection of its `instances` list and the real fleet;
* viewer session → only the instances with an ACL row for its subject;
* legacy token without scope → everything (pre-0.6 behaviour).

`InstanceSummary` gained an optional `role` field (`owner|operator|viewer`)
describing the caller's level on each row. No other endpoint leaks ids through
this list: the filter happens before the driver is queried.

## Enforcement points

* `authz::require_instance_access` — axum middleware on every protected route.
  It derives the instance id and action from the path
  (`/instances/{id}/…`), maps `GET/HEAD` to `read`, other methods to `write`,
  and any `…/exec` segment to `exec`. A known but forbidden instance answers
  `403 forbidden` with `details.{instance,action,subject}` before the handler
  runs; an unknown id is left to the handler so it stays `404`.
* `instances::list` — filters server-side as described above.
* `api::acl` handlers — owner/admin checks on top of the middleware.
* `instances::delete` — an `owner` (or admin); an `operator` cannot destroy the
  instance even though it may write to it.
* `instances::create`, `import`, `clone` — `admin` only in v1 (`403
  forbidden`).
* `auth::require_auth` — a global `viewer` may attempt a mutation only on an
  instance route; the middleware then decides. Every other mutation remains
  admin-only.

## Compatibility and migration

Schema v10 adds:

```sql
CREATE TABLE instance_acl (
    instance_id TEXT NOT NULL,
    subject     TEXT NOT NULL,
    role        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (instance_id, subject)
);
CREATE INDEX idx_instance_acl_subject ON instance_acl (subject);
ALTER TABLE api_tokens ADD COLUMN scope TEXT;
```

* `api_tokens.scope` is `NULL` for every existing token: it keeps its global
  role and its exact pre-0.6 behaviour (`viewer` reads the whole fleet).
* A **viewer session** with no ACL row no longer reads the fleet: sessions had
  no scope and always meant "the whole fleet", which is the leak this feature
  closes. After upgrading, give each viewer at least one ACL row (or the
  `admin` group) before they sign in again. Admins are unaffected.
* Interrupted upgrade: the migration runs in one transaction per step, so a
  failure leaves `PRAGMA user_version` at 9 and can be retried.
* Deleting an instance deletes its ACL rows.
* An ACL row with an unknown role (hand-edited database) grants nothing.

## Audit identity

The audit log (`GET /api/audit`) keeps `actor`, `role` and `method` for every
mutation. Sessions record the account email, named tokens their name — not
`bootstrap`. ACL changes additionally carry the affected `subject` in
`details`, and token creation records the granted `scope`.

## Known gaps (v1)

* Fleet-wide endpoints other than `/api/instances` still list every id to any
  authenticated reader: `GET /api/messages`, `/api/usage`, `/api/images`,
  `/api/upgrade`, `/api/audit` (admin only). Restrict them in a follow-up if a
  multi-tenant deployment needs it; the per-instance routes are covered.
* `403` versus `404`: a forbidden existing instance answers `403`. A constant
  response for unknown and forbidden ids (issue #49) will replace this later.
* No dedicated `/exec` route exists yet; the classifier and the `owner`/
  `exec` rules are ready for it.
* The web console does not show the Access tab or the "who has access" column
  yet (backend-only change).
