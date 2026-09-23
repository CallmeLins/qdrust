import { describe, expect, it } from "vitest";
import { OIDC_LOGOUT_RETURN_KEY, OIDC_LOGOUT_RETURN_TTL_MS, consumeLogoutReturn, formatRunTime, harDocumentFrom, localLoginAvailable, markLogoutReturn, oidcLogoutUrl, ssoAvailable, ssoOnly, unboundTemplatesFirst, type AuthPolicy, type StorageLike } from "./utils";

describe("formatRunTime", () => {
  it("describes a task without runs", () => {
    expect(formatRunTime(null)).toBe("尚未运行");
  });

  it("formats unix timestamps", () => {
    expect(formatRunTime(1_700_000_000, "en-GB", "UTC")).toMatch(/14\/11.*22:13/);
  });
});

describe("harDocumentFrom", () => {
  // The shape qd-today/templates publishes (base64 `content` in tpls_history.json,
  // and the repository's *.har files): a bare request array, no `log` wrapper.
  const qdEntry = {
    comment: "获取token",
    request: {
      method: "GET",
      url: "https://example.com/login",
      headers: [{ name: "Host", value: "example.com" }],
      cookies: [],
      data: "",
      mimeType: "",
    },
    rule: {
      success_asserts: [{ re: "200", from: "status" }],
      failed_asserts: [],
      extract_variables: [{ name: "token", re: 'name="token" value="(.+)"', from: "content" }],
    },
  };

  it("turns a QD request array into a HAR document the editor can read", () => {
    const doc = harDocumentFrom([qdEntry]) as any;
    expect(doc.log.version).toBe("1.2");
    expect(doc.log.entries).toHaveLength(1);
    const entry = doc.log.entries[0];
    expect(entry.checked).toBe(true);
    expect(entry.request.method).toBe("GET");
    expect(entry.request.url).toBe("https://example.com/login");
    expect(entry.request.headers).toEqual([{ name: "Host", value: "example.com", checked: true }]);
    expect(entry.comment).toBe("获取token");
  });

  it("flattens the QD `rule` object onto the entry", () => {
    const entry = (harDocumentFrom([qdEntry]) as any).log.entries[0];
    expect(entry.success_asserts).toEqual([{ re: "200", from: "status" }]);
    expect(entry.failed_asserts).toEqual([]);
    expect(entry.extract_variables).toEqual([{ name: "token", re: 'name="token" value="(.+)"', from: "content" }]);
  });

  it("maps a QD body onto postData", () => {
    const withBody = { ...qdEntry, request: { ...qdEntry.request, data: "a=1", mimeType: "application/x-www-form-urlencoded" } };
    const entry = (harDocumentFrom([withBody]) as any).log.entries[0];
    expect(entry.request.postData).toEqual({ mimeType: "application/x-www-form-urlencoded", text: "a=1" });
  });

  it("passes a HAR document through and normalises the version", () => {
    const har = { log: { version: "1.1", creator: { name: "x" }, entries: [{ request: { method: "GET", url: "https://a/" } }] } };
    const doc = harDocumentFrom(har) as any;
    expect(doc.log.version).toBe("1.2");
    expect(doc.log.entries[0].request.url).toBe("https://a/");
    // The log wrapper must not be wrapped a second time.
    expect(doc.log.log).toBeUndefined();
  });

  it("returns null for anything that is neither shape", () => {
    expect(harDocumentFrom(null)).toBeNull();
    expect(harDocumentFrom("{}")).toBeNull();
    expect(harDocumentFrom({ foo: 1 })).toBeNull();
    // A `log` that is not an object is not a HAR document either.
    expect(harDocumentFrom({ log: "nope" })).toBeNull();
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

describe("unboundTemplatesFirst", () => {
  const templates = [{ id: 1 }, { id: 2 }, { id: 3 }, { id: 4 }];

  it("moves the templates no task uses to the front", () => {
    expect(unboundTemplatesFirst(templates, [2, 4]).map((t) => t.id)).toEqual([1, 3, 2, 4]);
  });

  it("keeps the arrival order inside each bucket instead of sorting", () => {
    // The server hands templates over by creation id. The new task's dropdown
    // must only split that list, not reshuffle either half of it.
    const shuffled = [{ id: 30 }, { id: 10 }, { id: 20 }];
    expect(unboundTemplatesFirst(shuffled, [10]).map((t) => t.id)).toEqual([30, 20, 10]);
  });

  it("changes nothing when every template is already used", () => {
    expect(unboundTemplatesFirst(templates, [1, 2, 3, 4]).map((t) => t.id)).toEqual([1, 2, 3, 4]);
  });

  it("keeps every template exactly once, so none drops out of the dropdown", () => {
    const ordered = unboundTemplatesFirst(templates, [3]);
    expect(ordered).toHaveLength(templates.length);
    expect(new Set(ordered.map((t) => t.id)).size).toBe(templates.length);
  });

  it("copes with no tasks bound and with no templates at all", () => {
    expect(unboundTemplatesFirst(templates, [])).toEqual(templates);
    expect(unboundTemplatesFirst([], [1, 2])).toEqual([]);
  });
});


