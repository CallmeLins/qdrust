export function formatRunTime(value: number | null, locale = "zh-CN", timeZone?: string): string {
  if (!value) return "尚未运行";
  return new Intl.DateTimeFormat(locale, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone
  }).format(value * 1000);
}

// ---------- external IdP login policy (EXTERNAL_IDP_PLAN.md Phase 3) ----------

/** Public auth policy as delivered by GET /api/v1/auth/config. */
export interface AuthPolicy {
  auth_mode: "local" | "hybrid" | "oidc";
  local_login_enabled: boolean;
  oidc_enabled: boolean;
  oidc_provider_name: string;
  header_auth_enabled: boolean;
  oidc_logout_url: string;
  oidc_post_logout_redirect_uri: string;
}

/** True when the deployment offers SSO through OIDC (an SSO button should be
 *  shown on the sign-in page). */
export function ssoAvailable(policy: AuthPolicy | null): boolean {
  return !!policy?.oidc_enabled;
}

/** The IdP end-session URL to bounce the browser to after local logout, or ""
 *  when the deployment does not configure OIDC single logout. Appends the
 *  optional `post_logout_redirect_uri` so the user returns to this app. */
export function oidcLogoutUrl(policy: AuthPolicy | null): string {
  const base = policy?.oidc_logout_url?.trim();
  if (!base) return "";
  const post = policy?.oidc_post_logout_redirect_uri?.trim();
  if (!post) return base;
  const sep = base.includes("?") ? "&" : "?";
  return `${base}${sep}post_logout_redirect_uri=${encodeURIComponent(post)}`;
}

/** The subset of `Storage` the logout-return marker needs; narrowed so tests can
 *  pass a plain in-memory stub instead of a real `Storage`. */
export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** sessionStorage key for the "we just left for the IdP end-session endpoint"
 *  marker. */
export const OIDC_LOGOUT_RETURN_KEY = "qdrust-oidc-logout-return";

/** How long a return to this app still counts as coming back from an IdP logout.
 *  Without a bound, an abandoned logout (the user hits Back at the IdP, or the
 *  redirect never happens) would leave the marker set forever and a much later
 *  visit would be hijacked into an unexpected SSO redirect. */
export const OIDC_LOGOUT_RETURN_TTL_MS = 300_000;

/** Record that we are leaving for the IdP end-session endpoint and expect to
 *  come back logged out. Stored as a timestamp rather than a flag so
 *  `consumeLogoutReturn` can age it out. */
export function markLogoutReturn(storage: StorageLike, now: number = Date.now()): void {
  storage.setItem(OIDC_LOGOUT_RETURN_KEY, String(now));
}

/** Read-and-clear the logout-return marker. Returns true only when it was set
 *  recently (within `OIDC_LOGOUT_RETURN_TTL_MS`). The marker is consumed either
 *  way, so one logout can trigger at most one automatic SSO start. */
export function consumeLogoutReturn(storage: StorageLike, now: number = Date.now()): boolean {
  const raw = storage.getItem(OIDC_LOGOUT_RETURN_KEY);
  if (raw == null) return false;
  storage.removeItem(OIDC_LOGOUT_RETURN_KEY);
  const stamp = Number(raw);
  if (!Number.isFinite(stamp)) return false;
  const age = now - stamp;
  return age >= 0 && age < OIDC_LOGOUT_RETURN_TTL_MS;
}

/** True when the username/password entry points are enabled on the server and
 *  the user is not forced onto a credential-less path. When false the local
 *  form must be hidden (the server also returns 403 `local_login_disabled`). */
export function localLoginAvailable(policy: AuthPolicy | null): boolean {
  if (!policy) return true; // policy unknown (pre-render) -> show local form by default
  return policy.local_login_enabled;
}

/** In pure-OIDC deployments (`auth_mode === "oidc"`) the local sign-in form is
 *  not shown at all; the page should default to the SSO affordance. */
export function ssoOnly(policy: AuthPolicy | null): boolean {
  return policy?.auth_mode === "oidc";
}
