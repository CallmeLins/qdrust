# Mock OIDC IdP (local development)

A throwaway **OpenID Connect Provider** used to manually verify `qdrust-server`'s
OIDC login end to end (Authorization Code + PKCE, `groups → admin` mapping).
Built on [`panva/node-oidc-provider`](https://github.com/panva/node-oidc-provider),
so it behaves like a real IdP (real discovery, real signed RS256 ID tokens,
login + consent pages) — it is *not* a stubbed fake.

> This is a **dev/verification tool only**. It ships no production hardening:
> the built-in dev login accepts any username/password, keys and secrets are
> fixed, and the in-memory adapter forgets everything on restart. Never deploy it.

## Install & run

Requires Node.js (any modern LTS). From this directory:

```bash
npm install        # one-time; installs oidc-provider into ./node_modules
npm start          # listens on http://127.0.0.1:3000
```

On start it prints the accepted `redirect_uri`, client id/secret, accounts, and
the discovery URL.

## Accounts

Any username typed on the IdP login page is accepted as an account (dev mode).
Two pre-seeded accounts matter for the role test:

| Login  | ID-token `groups`                 | qdrust first-login role (with the sample config below) |
|--------|-----------------------------------|---------------------------------------------------------|
| `admin`| `["qdrust-admins", "users"]`      | **admin**  (hits `admin_groups`)                        |
| `user` | `["users"]`                       | **user**   (falls back to `default_role`)               |
| other  | `["users"]`                       | **user**                                                |

## Run qdrust against it

Start the mock (`npm start`), then start `qdrust-server` with OIDC enabled:

```bash
QDRUST_AUTH_MODE=hybrid \
QDRUST_OIDC_ENABLED=true \
QDRUST_OIDC_PROVIDER_NAME="Mock IdP" \
QDRUST_OIDC_ISSUER=http://127.0.0.1:3000 \
QDRUST_OIDC_CLIENT_ID=qdrust-mock \
QDRUST_OIDC_CLIENT_SECRET=mock-client-secret \
QDRUST_OIDC_SCOPES="openid profile email" \
QDRUST_OIDC_AUTO_CREATE_USERS=true \
QDRUST_OIDC_DEFAULT_ROLE=user \
QDRUST_OIDC_ADMIN_GROUPS=qdrust-admins \
QDRUST_OIDC_GROUPS_CLAIM=groups \
cargo run -p qdrust-server
```

Then open qdrust's login page and pick the **SSO / "Mock IdP"** button.

### About `QDRUST_OIDC_REDIRECT_URI`

qdrust derives the callback URL from the incoming request when no override is
set. When you hit it directly over plain HTTP (no reverse proxy setting
`X-Forwarded-Proto` / `X-Forwarded-Host`), that derivation falls back to
`https://localhost/...`, which will not match the redirect URI this IdP has
registered. **Set the override explicitly** for a direct-HTTP dev run:

```bash
export QDRUST_OIDC_REDIRECT_URI=http://127.0.0.1:8923/api/v1/auth/oidc/callback
```

The `index.mjs` `QDRUST_ORIGIN` default (`http://127.0.0.1:8923`) must agree
with the qdrust origin you actually use; set it if you run qdrust elsewhere.

### About claims in the ID token

By default `oidc-provider` conforms to the OIDC "basic" profile and pushes
`email` / `profile` (and any custom claim such as `groups`) to the **userinfo**
endpoint rather than the ID token. Because qdrust reads `groups` from the ID
token, this mock disables that conformance quirk (`conformIdTokenClaims: false`)
so email and `groups` are signed into the ID token — the same shape authentik
and Keycloak-with-a-mapper emit.

## Environment variables

| Var                | Default                                   | Meaning                                  |
|--------------------|-------------------------------------------|------------------------------------------|
| `PORT`             | `3000`                                    | Mock IdP listen port                     |
| `QDRUST_ORIGIN`    | `http://127.0.0.1:8923`                   | qdrust origin whose callback is accepted |
| `QDRUST_OIDC_CLIENT_ID` / `QDRUST_OIDC_CLIENT_SECRET` | `qdrust-mock` / `mock-client-secret` | Registered client credentials (must match qdrust) |

## Headless end-to-end check

You can drive the whole flow from the command line without a browser (a
browser-equivalent: hit `/auth/oidc/start`, do the IdP login + consent, follow
the callback, confirm a `qd_session` is issued and the user row exists with the
expected role). A local throwaway example lives under
`<repo>/scripts/mock-oidc-idp/e2e.mjs`:

```bash
node e2e.mjs admin   # expects user 'admin' -> role 'admin'
node e2e.mjs user    # expects user 'user'  -> role 'user'
```

It prints each hop and the final cookies; verify the role by inspecting the
`users` table in qdrust's database afterwards.
