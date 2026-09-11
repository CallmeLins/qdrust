import { describe, expect, it } from "vitest";
import { OIDC_LOGOUT_RETURN_KEY, OIDC_LOGOUT_RETURN_TTL_MS, consumeLogoutReturn, formatRunTime, localLoginAvailable, markLogoutReturn, oidcLogoutUrl, ssoAvailable, ssoOnly, type AuthPolicy, type StorageLike } from "./utils";

describe("formatRunTime", () => {
  it("describes a task without runs", () => {
    expect(formatRunTime(null)).toBe("尚未运行");
  });

  it("formats unix timestamps", () => {
    expect(formatRunTime(1_700_000_000, "en-GB", "UTC")).toMatch(/14\/11.*22:13/);
  });
});

describe("external IdP login policy", () => {
  const policy = (p: Partial<AuthPolicy> & Pick<AuthPolicy, "auth_mode">): AuthPolicy => ({
    local_login_enabled: true,
    oidc_enabled: false,
    oidc_provider_name: "",
    header_auth_enabled: false,
    oidc_logout_url: "",
    oidc_post_logout_redirect_uri: "",
    ...p,
  });
  const local: AuthPolicy = policy({ auth_mode: "local", oidc_enabled: false });
  const hybrid: AuthPolicy = policy({ auth_mode: "hybrid", oidc_enabled: true, oidc_provider_name: "Authentik" });
  const oidc: AuthPolicy = policy({ auth_mode: "oidc", local_login_enabled: false, oidc_enabled: true, oidc_provider_name: "Keycloak" });

  it("shows SSO only when OIDC is enabled", () => {
    expect(ssoAvailable(local)).toBe(false);
    expect(ssoAvailable(hybrid)).toBe(true);
    expect(ssoAvailable(oidc)).toBe(true);
    expect(ssoAvailable(null)).toBe(false);
  });

  it("defaults the local form to visible when the policy is unknown", () => {
    expect(localLoginAvailable(local)).toBe(true);
    expect(localLoginAvailable(hybrid)).toBe(true);
    expect(localLoginAvailable(oidc)).toBe(false);
    expect(localLoginAvailable(null)).toBe(true);
  });

  it("only forces SSO in pure-OIDC mode", () => {
    expect(ssoOnly(local)).toBe(false);
    expect(ssoOnly(hybrid)).toBe(false);
    expect(ssoOnly(oidc)).toBe(true);
    expect(ssoOnly(null)).toBe(false);
  });
});

describe("oidcLogoutUrl", () => {
  it("returns '' when no IdP end-session URL is configured", () => {
    expect(oidcLogoutUrl(null)).toBe("");
    expect(oidcLogoutUrl({ auth_mode: "oidc", local_login_enabled: false, oidc_enabled: true, oidc_provider_name: "K", header_auth_enabled: false, oidc_logout_url: "", oidc_post_logout_redirect_uri: "" })).toBe("");
  });

  it("returns the bare URL when no post_logout_redirect_uri is configured", () => {
    const p: AuthPolicy = { auth_mode: "oidc", local_login_enabled: false, oidc_enabled: true, oidc_provider_name: "K", header_auth_enabled: false, oidc_logout_url: "https://idp.example/realms/qdrust/protocol/openid-connect/logout", oidc_post_logout_redirect_uri: "" };
    expect(oidcLogoutUrl(p)).toBe("https://idp.example/realms/qdrust/protocol/openid-connect/logout");
  });

  it("appends post_logout_redirect_uri when configured", () => {
    const p: AuthPolicy = { auth_mode: "oidc", local_login_enabled: false, oidc_enabled: true, oidc_provider_name: "K", header_auth_enabled: false, oidc_logout_url: "https://idp.example/logout?x=1", oidc_post_logout_redirect_uri: "https://app.example/qd/" };
    expect(oidcLogoutUrl(p)).toBe("https://idp.example/logout?x=1&post_logout_redirect_uri=https%3A%2F%2Fapp.example%2Fqd%2F");
  });
});

describe("logout-return marker", () => {
  const fakeStorage = (): StorageLike & { readonly map: Map<string, string> } => {
    const map = new Map<string, string>();
    return {
      map,
      getItem: (key) => map.get(key) ?? null,
      setItem: (key, value) => void map.set(key, value),
      removeItem: (key) => void map.delete(key),
    };
  };

  it("reports true once after a fresh logout and consumes the marker", () => {
    const storage = fakeStorage();
    markLogoutReturn(storage, 1_000);
    expect(storage.map.get(OIDC_LOGOUT_RETURN_KEY)).toBe("1000");
    expect(consumeLogoutReturn(storage, 1_500)).toBe(true);
    // One logout must not auto-start SSO twice.
    expect(consumeLogoutReturn(storage, 1_500)).toBe(false);
    expect(storage.map.has(OIDC_LOGOUT_RETURN_KEY)).toBe(false);
  });

  it("is false when no logout happened", () => {
    expect(consumeLogoutReturn(fakeStorage())).toBe(false);
  });

  it("ignores a stale marker past the TTL but still clears it", () => {
    const storage = fakeStorage();
    markLogoutReturn(storage, 1_000);
    expect(consumeLogoutReturn(storage, 1_000 + OIDC_LOGOUT_RETURN_TTL_MS)).toBe(false);
    expect(storage.map.has(OIDC_LOGOUT_RETURN_KEY)).toBe(false);
  });

  it("ignores a future timestamp and a garbage value", () => {
    const skew = fakeStorage();
    markLogoutReturn(skew, 10_000);
    expect(consumeLogoutReturn(skew, 9_000)).toBe(false);
    const garbage = fakeStorage();
    garbage.setItem(OIDC_LOGOUT_RETURN_KEY, "not-a-number");
    expect(consumeLogoutReturn(garbage)).toBe(false);
  });
});


