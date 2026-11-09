# SSO with OpenID Connect (Authentik)

`fleett` can authenticate browsers against an OpenID Connect provider instead
of asking for the static token. Roles (`admin` / `viewer`), CSRF protection and
an audit trail come with it; the bearer token remains available for automation.

`FLEETT_AUTH` selects the credential types:

| Value | Browser session | Bearer token |
|---|---|---|
| `bearer` (default) | no | `FLEETT_TOKEN` + named tokens |
| `oidc` | yes | no |
| `both` | yes | `FLEETT_TOKEN` + named tokens |

The default is `bearer`: an existing installation keeps working exactly as
before, with no OIDC variable to set.

## Environment

```bash
# ~/.fleett/fleett.env (mode 600) — never commit this file
FLEETT_AUTH=both
FLEETT_PUBLIC_URL=https://fleett.example.com
FLEETT_OIDC_ISSUER=https://sso.example.com/application/o/fleett/
FLEETT_OIDC_CLIENT_ID=fleett
FLEETT_OIDC_CLIENT_SECRET=...            # from the Authentik provider page
FLEETT_SESSION_SECRET=...                # openssl rand -base64 32
# Optional:
# FLEETT_OIDC_REDIRECT_URL=https://fleett.example.com/auth/callback
# FLEETT_OIDC_SCOPES="openid profile email"
# FLEETT_OIDC_ADMIN_GROUP=fleett-admins
# FLEETT_OIDC_VIEWER_GROUP=fleett-viewers
# FLEETT_OIDC_STATE_TTL_SECS=600
# FLEETT_SESSION_TTL_SECS=28800
# FLEETT_COOKIE_SECURE=auto
```

| Variable | Default | Notes |
|---|---|---|
| `FLEETT_PUBLIC_URL` | `http://127.0.0.1:8090` | used to derive the redirect URI and `auto` cookie security |
| `FLEETT_OIDC_ISSUER` | — | required; the `/.../` trailing slash is tolerated |
| `FLEETT_OIDC_CLIENT_ID` | — | required |
| `FLEETT_OIDC_CLIENT_SECRET` | — | required; sent with `client_secret_basic` |
| `FLEETT_OIDC_REDIRECT_URL` | `${PUBLIC_URL}/auth/callback` | must match the provider exactly |
| `FLEETT_OIDC_SCOPES` | `openid profile email` | `openid` is added when missing |
| `FLEETT_OIDC_ADMIN_GROUP` | `fleett-admins` | group name, not a UUID |
| `FLEETT_OIDC_VIEWER_GROUP` | `fleett-viewers` | read-only role |
| `FLEETT_OIDC_STATE_TTL_SECS` | `600` | lifetime of the state cookie, 60–3600 s (300–900 s recommended) |
| `FLEETT_SESSION_SECRET` | — | required; base64 of at least 32 random bytes |
| `FLEETT_SESSION_TTL_SECS` | `28800` | lifetime of the session cookie |
| `FLEETT_COOKIE_SECURE` | `auto` | `auto` = `Secure` when `PUBLIC_URL` starts with `https://` |

The configuration is validated at startup: with `oidc` or `both`, a missing
issuer/client/secret or a session secret shorter than 32 bytes aborts startup
with the variable name. `FLEETT_TOKEN` is only required in `bearer` mode.

Run behind TLS (or a TLS-terminating proxy) in production. `FLEETT_COOKIE_SECURE=auto`
alone is not enough: the cookie is only `Secure` when `FLEETT_PUBLIC_URL` is
`https://`.

## Authentik setup

1. **Create the groups.** `fleett-admins` and `fleett-viewers` (Directory →
   Groups). The role is derived from the `groups` claim, never from the token
   name or the e-mail.
2. **Create an OAuth2/OpenID provider** (Applications → Providers → Create):
   - *Client type*: `Confidential`
   - *Redirect URI/Origins*: `https://fleett.example.com/auth/callback`
     (exactly `FLEETT_OIDC_REDIRECT_URL`)
   - *Signing Key*: any certificate; `RS256` is the only algorithm fleett
     accepts
   - *Scopes*: `openid`, `profile`, `email` (the defaults)
3. **Add the `groups` claim.** The default `profile` mapping does not include
   groups. Create a custom property mapping (Customization → Property Mappings
   → Create → Scope Mapping), assign it to the `profile` scope:

   ```python
   return {"groups": [group.name for group in request.user.ak_groups.all()]}
   ```

   fleett accepts the claim as a JSON array or as a space-separated string.
4. **Create the application** linked to that provider and bind the two groups
   to it (Policy / Group bindings). A user in neither group is refused with
   `403 forbidden` after a successful IdP login.
5. **Issuer** = the provider's *OpenID Configuration Issuer*, typically
   `https://sso.example.com/application/o/fleett/`. fleett fetches
   `/.well-known/openid-configuration` on the first login and verifies that the
   advertised issuer matches the configured one.

An account in **both** groups is an admin: the admin group wins. The role is
baked into the signed session at login, so a group change takes effect at the
next login (or when `FLEETT_SESSION_TTL_SECS` expires); short TTLs limit that
window if group membership must be revocable quickly.

## Flow and cookies

`GET /auth/login` (404 when OIDC is off) redirects to the provider with
`state`, `nonce`, `code_challenge` (S256) and a signed `fleett_oidc` cookie
(`HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` per config) valid for
`FLEETT_OIDC_STATE_TTL_SECS` (600 s by default).

**State lifetime.** Keep `FLEETT_OIDC_STATE_TTL_SECS` short: 300–900 s covers
an interactive login (password, 2FA/WebAuthn on a phone, consent screens) while
bounding how long the state cookie can be replayed. The value (accepted range
60–3600) drives both the cookie `Max-Age` and the signed `exp`, so the two
cannot drift apart. A login that takes longer fails with
`invalid_state` / `details.reason = "expired"`; raise the value rather than
retrying blindly if your users routinely need more.

`GET /auth/callback` verifies the state cookie, exchanges the code (PKCE,
`client_secret_basic`), verifies the ID token (RS256 signature through the
provider JWKS, issuer, audience, expiry, nonce), maps the role and sets:

| Cookie | Content | Attributes |
|---|---|---|
| `fleett_session` | `v1.<base64url(JSON)>.<base64url(HMAC-SHA256)>` with `{sub,email,name,role,iat,exp}` | `HttpOnly`, `SameSite=Lax`, `Secure` per config, `Max-Age=FLEETT_SESSION_TTL_SECS` |
| `fleett_csrf` | 32 random hex | **not** `HttpOnly` (the console must read it), `SameSite=Lax` |

then redirects to `/`. `GET /api/auth/me` returns the identity:

```json
{"authenticated":true,"actor":"cleeryy@example.com","role":"admin",
 "method":"session","user":{"sub":"…","email":"cleeryy@example.com","name":"Cleeryy"}}
```

**CSRF.** A cookie-authenticated mutation (`POST`, `PATCH`, `DELETE`) must echo
the CSRF cookie in the `X-Fleett-CSRF` header. Bearer clients are exempt. A
missing or wrong header returns `403 {"error":{"code":"csrf_required"}}`.

### Callback failures

The callback can fail on state alone, before the provider is ever contacted.
Every `invalid_state` response carries a stable `details.reason`, so a support
report can be triaged without the server logs:

| `details.reason` | Cause | Browser answer |
|---|---|---|
| `cookie_missing` | no `fleett_oidc` cookie: the link was opened in another browser/device, a webview, or cookies are blocked | `302 /auth/login` |
| `state_mismatch` | the cookie is present but does not describe this request (already used, tampered, overwritten by another tab) | HTML page |
| `expired` | the cookie outlived `FLEETT_OIDC_STATE_TTL_SECS` (slow 2FA, interrupted flow) | HTML page |

API clients (`Accept` without `text/html`, e.g. `application/json`) keep the JSON
error and the 400 status:

```json
{"error":{"code":"invalid_state","message":"the login state cookie has expired","details":{"reason":"expired"}}}
```

Browsers (`Accept: text/html…`) get a small self-contained page with the cause
and a **Sign in again** link to `/auth/login`; a missing cookie skips the page
and redirects straight there. Exactly one `WARN` line is logged per failed
callback with `reason`, the requested `Host` and the `User-Agent` — never the
code, state, nonce or cookie.

An `invalid_state` therefore always points at the browser, not the server:
either the link was opened where the cookie is not (another device, an in-app
webview, a private window) or the login window elapsed. A cookie missing on the
*same* browser after a few seconds usually means cookies are blocked, the page
was opened on another host (`http://` vs `https://`), or a reverse proxy
stripped `Set-Cookie`.

**Roles.** `viewer` may only `GET`/`HEAD` on `/api/*`; every mutation returns
`403 {"error":{"code":"forbidden"}}`. `GET /api/audit`, `/api/tokens` and
`DELETE /api/tokens/{id}` are admin-only.

**Named tokens.** `POST /api/tokens` (admin) creates `flt_<32 hex>`; the secret
is returned once and stored only as a SHA-256 digest. `GET /api/tokens` lists
names/roles/revocation dates without the secret, `DELETE /api/tokens/{id}` sets
`revoked_at` and the token stops working on the next request. Tokens are
role-scoped (`admin` or `viewer`) and carry their name as the audit actor.

## Logout

```bash
curl -i -X POST https://fleett.example.com/auth/logout
# 204, Set-Cookie: fleett_session=…; Max-Age=0  (also fleett_csrf, fleett_oidc)
```

The endpoint is idempotent and unauthenticated: clearing cookies that are
already absent is harmless. It only clears **fleett** cookies; the SSO session
at Authentik stays alive, so a new `/auth/login` may complete without a
password prompt. To end the IdP session too, redirect the browser to the
provider's end-session endpoint (`https://sso.example.com/application/o/fleett/end-session/`).

## Reverse proxy and forward-auth

fleett never answers an API request with a redirect: an unauthenticated
`/api/*` call returns `401` JSON (`{"error":{"code":"unauthorized",…}}`), never
a `302` to the IdP. That is what keeps `fetch()` calls from following a login
page and failing on HTML.

Consequences for a deployment:

- **Native OIDC (recommended).** Publish `/`, `/api/*`, `/auth/*` straight to
  fleett and let it run the flow. Nothing else to configure.
- **Authentik forward-auth in front.** An Authentik *proxy outpost* in front of
  fleett would intercept `/api/*` before fleett sees it and answer XHR calls
  with a `302` — the exact failure this design avoids. Give the outpost a
  bypass policy for `/api/*` and `/auth/*` (they carry their own credentials),
  or protect only the static console (`/`, `/assets/*`) and let fleett handle
  `/api/*`. In both cases `FLEETT_PUBLIC_URL` must be the external HTTPS URL
  and `FLEETT_COOKIE_SECURE=true` if the proxy terminates TLS.
- If a proxy sets `X-Forwarded-For`, fleett records its first hop as the audit
  `ip`; otherwise the peer address is used.

## Audit trail

Every successful mutation appends to `audit_log`:

| Action | Target |
|---|---|
| `instance.create`, `instance.patch`, `instance.delete` | instance id |
| `instance.start`, `instance.stop`, `instance.restart` | instance id |
| `share.create`, `share.delete` | share name |
| `share.attach`, `share.detach` | share name (`details.instance`) |
| `token.create`, `token.revoke` | token id |

Each row stores `at`, `actor`, `role`, `method` (`session`/`bearer`), `ip`,
`user_agent`, `result` (`ok`, `failed` or `refused`), an optional `request_id`
and JSON `details`. The OIDC flow itself is audited: a successful callback
appends `auth.login` with the email and the issuer, a rejected callback appends
`auth.login` with `result=failed` and `details.reason` (`cookie_missing`,
`state_mismatch`, `expired`, …), and `POST /auth/logout` appends `auth.logout`.
Refused mutations (viewer role, missing CSRF header) are recorded as
`auth.mutation_refused` with `result=refused`.

`GET /api/audit` returns the newest first with filters (`actor`, `action`,
`target`, `from`, `to`, `result`, `limit`, `offset`); the legacy
`?limit=200` call is unchanged (1–1000). See [AUDIT.md](AUDIT.md) for the
signed JSONL export (`GET /api/audit/export?from=&to=`,
`X-Fleett-Signature`, `FLEETT_AUDIT_HMAC_KEY`) and the configurable retention
(`FLEETT_AUDIT_RETENTION`, default `90d`, applied daily and by
`DELETE /api/audit/expired`, which leaves an `audit.purge` entry).

## Testing

Without an IdP (unit and integration tests use a fake verifier):

```bash
cd backend
cargo test auth              # session, role mapping, PKCE, claims
cargo test --test auth       # full flow, CSRF, tokens, audit
```

Against a real provider:

```bash
# 1. advertise the mode
curl -s http://127.0.0.1:8090/api/auth/config
# {"mode":"both","oidc":{"enabled":true,"login_url":"/auth/login"},"bearer":{"enabled":true}}

# 2. exercise the redirect (no follow: inspect the Location and the cookie)
curl -si http://127.0.0.1:8090/auth/login | grep -i -E '^(HTTP|location|set-cookie)'

# 3. complete the login in a browser, then reuse the cookie
curl -s -H 'Cookie: fleett_session=…' http://127.0.0.1:8090/api/auth/me

# 4. a viewer may read…
curl -s -H 'Cookie: fleett_session=…' http://127.0.0.1:8090/api/instances
# …but not mutate (403 forbidden even before the CSRF check)
curl -si -X POST -H 'Cookie: fleett_session=…' -H 'Content-Type: application/json' \
  -d '{"name":"nope"}' http://127.0.0.1:8090/api/shares

# 5. an admin mutation needs the CSRF header
curl -si -X POST -H 'Cookie: fleett_session=…; fleett_csrf=…' \
  -H 'X-Fleett-CSRF: …' -H 'Content-Type: application/json' \
  -d '{"name":"findings"}' http://127.0.0.1:8090/api/shares

# 6. the audit log
curl -s -H 'Cookie: fleett_session=…' 'http://127.0.0.1:8090/api/audit?limit=20'
```
