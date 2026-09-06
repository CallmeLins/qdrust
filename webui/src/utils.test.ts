import { describe, expect, it } from "vitest";
import { formatRunTime, localLoginAvailable, ssoAvailable, ssoOnly, type AuthPolicy } from "./utils";

describe("formatRunTime", () => {
  it("describes a task without runs", () => {
    expect(formatRunTime(null)).toBe("尚未运行");
  });

  it("formats unix timestamps", () => {
    expect(formatRunTime(1_700_000_000, "en-GB", "UTC")).toMatch(/14\/11.*22:13/);
  });
});

describe("external IdP login policy", () => {
  const local: AuthPolicy = {
    auth_mode: "local", local_login_enabled: true,
    oidc_enabled: false, oidc_provider_name: "", header_auth_enabled: false,
  };
  const hybrid: AuthPolicy = {
    auth_mode: "hybrid", local_login_enabled: true,
    oidc_enabled: true, oidc_provider_name: "Authentik", header_auth_enabled: false,
  };
  const oidc: AuthPolicy = {
    auth_mode: "oidc", local_login_enabled: false,
    oidc_enabled: true, oidc_provider_name: "Keycloak", header_auth_enabled: false,
  };

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

