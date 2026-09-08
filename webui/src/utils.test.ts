import { describe, expect, it } from "vitest";
import { formatRunTime, localLoginAvailable, oidcLogoutUrl, ssoAvailable, ssoOnly, type AuthPolicy } from "./utils";

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


