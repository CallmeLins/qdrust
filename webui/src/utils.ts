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
}

/** True when the deployment offers SSO through OIDC (an SSO button should be
 *  shown on the sign-in page). */
export function ssoAvailable(policy: AuthPolicy | null): boolean {
  return !!policy?.oidc_enabled;
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
