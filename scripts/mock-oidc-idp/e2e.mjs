// Headless end-to-end driver for the local mock IdP: walks qdrust-server's
// OIDC login exactly as a browser would (cookie jar + manual redirects + the
// IdP's dev login/consent pages) and reports the final session cookies.
//
// Usage:
//   node e2e.mjs [username]          # default username: admin
//
// Start the mock IdP (`npm start`) and qdrust-server with OIDC enabled first.
// After a SUCCESS you should see user '<username>' in qdrust's `users` table
// with the expected role (admin if the account carries an admin group).
//
// Optional env:
//   QDRUST_ORIGIN   qdrust base URL        (default http://127.0.0.1:8923)
//   QDRUST_ISSUER   mock IdP base URL      (default http://127.0.0.1:3000)
const Q = (process.env.QDRUST_ORIGIN || 'http://127.0.0.1:8923').replace(/\/$/, '');
const IDP = (process.env.QDRUST_ISSUER || 'http://127.0.0.1:3000').replace(/\/$/, '');

const jar = new Map();
const cookieHeader = () => [...jar.entries()].map(([k, v]) => `${k}=${v}`).join('; ');
function storeCookies(setCookieHeader) {
  if (!setCookieHeader) return;
  for (const part of setCookieHeader.split(',')) {
    const [pair] = part.split(';');
    const eq = pair.indexOf('=');
    if (eq < 1) continue;
    const k = pair.slice(0, eq).trim();
    const v = pair.slice(eq + 1).trim();
    if (v === '') jar.delete(k); else jar.set(k, v);
  }
}
async function req(url, { method = 'GET', headers = {}, body } = {}) {
  const res = await fetch(url, { method, redirect: 'manual', headers: { cookie: cookieHeader(), ...headers }, body });
  storeCookies(res.headers.get('set-cookie'));
  return res;
}
const username = process.argv[2] || 'admin';
const rel = (res) => { const l = res.headers.get('location'); return l ? (l.startsWith('http') ? l : `${IDP}${l}`) : null; };
const interactionUid = (url) => (url || '').match(/\/interaction\/([^/?]+)/)?.[1];
const post = (uid, prompt) => req(`${IDP}/interaction/${uid}`, {
  method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
  body: new URLSearchParams({ prompt, login: username, password: 'x' }),
});

// 1. qdrust /oidc/start -> 303 Location: IdP /auth
let res = await req(`${Q}/api/v1/auth/oidc/start`);
let loc = rel(res);
console.log(`[1] qdrust /auth/oidc/start -> ${res.status}`);

// 2. IdP /auth -> 303 /interaction/:uid (login)
res = await req(loc); loc = rel(res);
console.log(`[2] IdP authorize -> ${res.status} -> ${loc.replace(IDP, '')}`);

// 3. POST login
res = await post(interactionUid(loc), 'login');
console.log(`[3] login POST -> ${res.status}`);

// 4. resume /auth/:uid -> 303 /interaction/:uid (consent)
res = await req(rel(res));
console.log(`[4] resume -> ${res.status} -> ${rel(res)?.replace(IDP, '')}`);

// 5. POST consent
res = await post(interactionUid(rel(res)), 'consent');
console.log(`[5] consent POST -> ${res.status}`);

// 6. resume -> 303 callback?code&state (back to qdrust)
res = await req(rel(res));
const cb = rel(res);
console.log(`[6] resume -> ${res.status} -> ${cb?.replace(Q, '')}`);

// 7. qdrust callback
res = await req(cb);
console.log(`[7] qdrust callback -> ${res.status} location=${res.headers.get('location')}`);
const cookies = (res.headers.get('set-cookie') || '').split(',').map((s) => s.split(';')[0].trim()).filter(Boolean);
console.log(`    cookies: ${cookies.join(' | ')}`);
const ok = res.headers.get('location') === '/' && cookies.some((c) => c.startsWith('qd_session='));
console.log(`\n=== ${username}: ${ok ? 'LOGIN SUCCESS (session issued)' : 'CHECK FLOW (no session?)'} ===`);
process.exit(ok ? 0 : 1);
