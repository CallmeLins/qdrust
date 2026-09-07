// Local development OpenID Connect Provider used to manually verify
// qdrust-server's OIDC login (Authorization Code + PKCE) end to end.
//
// This is a *mock/dev* IdP: the built-in "dev interactions" render a login +
// consent page, and any username you type is accepted as the account (no real
// password check). It exists so a developer can click through the full flow:
//   qdrust SSO  ->  this IdP login page  ->  consent  ->  back to qdrust, signed in.
//
// Usage (from this directory):
//   npm install
//   npm start                 # listens on http://127.0.0.1:3000 by default
//
// Env:
//   PORT            IdP listen port (default 3000)
//   QDRUST_ORIGIN   the qdrust-server origin whose redirect_uri this IdP will
//                   accept, e.g. http://127.0.0.1:8923 (default)
//
// Start qdrust-server in the same terminal session with, e.g.:
//   QDRUST_AUTH_MODE=hybrid \
//   QDRUST_OIDC_ENABLED=true \
//   QDRUST_OIDC_PROVIDER_NAME="Mock IdP" \
//   QDRUST_OIDC_ISSUER=http://127.0.0.1:3000 \
//   QDRUST_OIDC_CLIENT_ID=qdrust-mock \
//   QDRUST_OIDC_CLIENT_SECRET=mock-client-secret \
//   QDRUST_OIDC_SCOPES="openid profile email" \
//   QDRUST_OIDC_AUTO_CREATE_USERS=true \
//   QDRUST_OIDC_DEFAULT_ROLE=user \
//   QDRUST_OIDC_ADMIN_GROUPS=qdrust-admins \
//   QDRUST_OIDC_GROUPS_CLAIM=groups \
//   cargo run -p qdrust-server
//
// Log in at the mock with username `admin` (its groups claim includes
// `qdrust-admins`, so the first login provisions an admin), or `user`
// (no admin group -> lands in default_role). See the accompanying README.

import Provider from 'oidc-provider';

const PORT = Number(process.env.PORT || 3000);
const issuer = `http://127.0.0.1:${PORT}`;
// Origin qdrust-server runs at; its /auth/oidc/callback redirect_uri must be
// registered on the client below. Defaults to the bare qdrust dev origin.
const qdrustOrigin = (process.env.QDRUST_ORIGIN || 'http://127.0.0.1:8923').replace(/\/$/, '');

const CLIENT_ID = process.env.QDRUST_OIDC_CLIENT_ID || 'qdrust-mock';
const CLIENT_SECRET = process.env.QDRUST_OIDC_CLIENT_SECRET || 'mock-client-secret';
const REDIRECT_URI = `${qdrustOrigin}/api/v1/auth/oidc/callback`;

// Well-known (dev-only) accounts. The `login` you type on the IdP page becomes
// the account id, so any of these keys (or any other username) is accepted.
// `groups` is the claim qdrust reads via QDRUST_OIDC_GROUPS_CLAIM.
const ACCOUNTS = {
  admin: {
    name: 'Mock Admin',
    email: 'admin@example.com',
    preferred_username: 'admin',
    groups: ['qdrust-admins', 'users'],
  },
  user: {
    name: 'Mock User',
    email: 'user@example.com',
    preferred_username: 'user',
    groups: ['users'],
  },
};

const config = {
  clients: [
    {
      client_id: CLIENT_ID,
      client_secret: CLIENT_SECRET,
      grant_types: ['authorization_code'],
      redirect_uris: [REDIRECT_URI],
      response_types: ['code'],
      token_endpoint_auth_method: 'client_secret_post',
    },
  ],
  // qdrust requests "openid profile email"; attach the custom `groups` claim to
  // the `profile` scope so it is emitted into the ID token.
  claims: {
    address: ['address'],
    email: ['email', 'email_verified'],
    phone: ['phone_number', 'phone_number_verified'],
    profile: [
      'birthdate', 'family_name', 'gender', 'given_name', 'locale',
      'middle_name', 'name', 'nickname', 'picture', 'preferred_username',
      'profile', 'updated_at', 'website', 'zoneinfo', 'groups',
    ],
  },
  cookies: {
    keys: ['qdrust-mock-idp-cookie-key-please-change'],
  },
  // By default oidc-provider conforms to the OIDC "basic" profile and pushes
  // scope-derived claims (email, profile, custom `groups`) to the userinfo
  // endpoint instead of the ID token. qdrust reads group membership from the
  // ID token (it does not call userinfo), so we disable that conformance quirk
  // to keep email/profile/groups in the ID token — the same shape authentik and
  // Keycloak-with-a-mapper emit.
  conformIdTokenClaims: false,
  features: {
    // devInteractions defaults to true; keep it so oidc-provider ships the
    // login + consent pages and we need no custom UI.
    devInteractions: { enabled: true },
  },
  // Resolve any typed username to an account whose claims() carries `groups`.
  findAccount: async (_ctx, accountId) => {
    const profile = ACCOUNTS[accountId] || {
      name: accountId,
      email: `${accountId}@example.com`,
      preferred_username: accountId,
      groups: ['users'],
    };
    return {
      accountId,
      async claims(_use, _scope) {
        return {
          sub: accountId,
          name: profile.name,
          email: profile.email,
          email_verified: true,
          preferred_username: profile.preferred_username,
          groups: profile.groups,
        };
      },
    };
  },
};

const provider = new Provider(issuer, config);

provider.on('error', (err) => {
  console.error('oidc-provider error:', err && err.message ? err.message : err);
});

const server = provider.listen(PORT, '127.0.0.1', () => {
  console.log('----------------------------------------');
  console.log(`Mock OIDC IdP listening on ${issuer}`);
  console.log(`Accepted qdrust redirect_uri : ${REDIRECT_URI}`);
  console.log(`Client id / secret           : ${CLIENT_ID} / ${CLIENT_SECRET}`);
  console.log('Accounts:  "admin"  -> groups [qdrust-admins, users]  (provisions an admin)');
  console.log('           "user"   -> groups [users]                 (provisions a normal user)');
  console.log('           any other username is accepted as a plain user.');
  console.log(`Discovery: ${issuer}/.well-known/openid-configuration`);
  console.log('----------------------------------------');
});

function shutdown() {
  console.log('\nStopping mock OIDC IdP...');
  server.close(() => process.exit(0));
  // Force-exit if close hangs on open keep-alive connections.
  setTimeout(() => process.exit(0), 500).unref();
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
