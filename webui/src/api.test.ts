import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiRequestError, api, apiPath, errorCode, oidcStartUrl, prefixFromPathname } from "./api";

describe("prefixFromPathname (runtime sub-path detection)", () => {
  it("detects no prefix at the bare root", () => {
    expect(prefixFromPathname("/")).toBe("");
    expect(prefixFromPathname("")).toBe("");
  });

  it("detects a single-segment sub-path prefix", () => {
    expect(prefixFromPathname("/qd/")).toBe("/qd");
    expect(prefixFromPathname("/qd")).toBe("/qd");
  });

  it("detects a multi-segment prefix", () => {
    expect(prefixFromPathname("/tools/qdrust/")).toBe("/tools/qdrust");
  });

  it("ignores the trailing email deep-link segment when detecting the prefix", () => {
    expect(prefixFromPathname("/qd/verify-email")).toBe("/qd");
    expect(prefixFromPathname("/qd/reset-password")).toBe("/qd");
    expect(prefixFromPathname("/qd/reset-password/")).toBe("/qd");
  });

  it("keeps no prefix when an email deep link sits at the bare root", () => {
    expect(prefixFromPathname("/verify-email")).toBe("");
    expect(prefixFromPathname("/reset-password")).toBe("");
  });
});

describe("apiPath (runtime prefix support)", () => {
  it("leaves paths unchanged when served at the root (empty prefix)", () => {
    expect(apiPath("/api/v1/tasks", "")).toBe("/api/v1/tasks");
    expect(apiPath("/ready", "")).toBe("/ready");
  });

  it("prefixes API calls under a sub-path", () => {
    expect(apiPath("/api/v1/tasks", "/qd")).toBe("/qd/api/v1/tasks");
    expect(apiPath("/api/v1/runs/1/cancel", "/qd")).toBe("/qd/api/v1/runs/1/cancel");
  });

  it("keeps liveness/readiness probes at the bare root", () => {
    expect(apiPath("/ready", "/qd")).toBe("/ready");
    expect(apiPath("/health", "/qd")).toBe("/health");
  });

  it("defaults to no prefix outside a DOM (unit-test environment)", () => {
    expect(apiPath("/api/v1/tasks")).toBe("/api/v1/tasks");
  });
});

describe("oidcStartUrl (SSO entry point)", () => {
  it("points at the OIDC start endpoint at the bare root", () => {
    expect(oidcStartUrl("")).toBe("/api/v1/auth/oidc/start");
  });

  it("prefixes the OIDC start endpoint under a sub-path", () => {
    expect(oidcStartUrl("/qd")).toBe("/qd/api/v1/auth/oidc/start");
    expect(oidcStartUrl("/tools/qdrust")).toBe("/tools/qdrust/api/v1/auth/oidc/start");
  });
});

/**
 * The error envelope `request()` hands to its callers.
 *
 * A rejected response used to arrive as a bare `Error` holding only the
 * server's sentence, so no caller could react to a *specific* failure. The
 * templates page needs exactly that: the server answers 409 `template_in_use`
 * when a template still has tasks, and the page has to recognise it and say so
 * in the user's language rather than print an English sentence from the API.
 * These pin both halves, plus the fallbacks for a response that is not an API
 * error at all.
 */
describe("request (rejected responses)", () => {
  afterEach(() => vi.unstubAllGlobals());

  /** Point the globals `request()` reaches for at canned values and record the
   *  fetch calls. `document` needs stubbing too: it reads the CSRF cookie, and
   *  this suite runs without a DOM. */
  function stubFetch(response: Response, cookie = "") {
    const calls: Array<[string, RequestInit | undefined]> = [];
    vi.stubGlobal("document", { cookie });
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init]);
      return response;
    });
    return calls;
  }

  function jsonResponse(status: number, body: unknown): Response {
    return new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" }
    });
  }

  it("keeps the error code alongside the server's message", async () => {
    stubFetch(jsonResponse(409, {
      code: "template_in_use",
      message: "Template is still used by 3 task(s); delete or re-bind them first",
      field_errors: {},
      request_id: "req-1"
    }));

    const failure = await api.deleteTemplate(7).then(
      () => null,
      (cause: unknown) => cause
    );

    expect(failure).toBeInstanceOf(ApiRequestError);
    expect(failure).toBeInstanceOf(Error);
    const error = failure as ApiRequestError;
    // The code is what a caller branches on; `errorCode` is the non-throwing
    // way to ask for it from a `catch`.
    expect(error.code).toBe("template_in_use");
    expect(errorCode(error)).toBe("template_in_use");
    expect(error.status).toBe(409);
    // The message is still exactly what the server said, unedited.
    expect(error.message).toBe("Template is still used by 3 task(s); delete or re-bind them first");
  });

  it("sends the delete to the template's own path", async () => {
    const calls = stubFetch(new Response(null, { status: 204 }));

    await api.deleteTemplate(7);

    expect(calls).toHaveLength(1);
    expect(calls[0][0]).toBe("/api/v1/templates/7");
    expect(calls[0][1]?.method).toBe("DELETE");
  });

  it("carries the CSRF token from the cookie, percent-decoded", async () => {
    const calls = stubFetch(new Response(null, { status: 204 }), "qd_csrf=abc%2F123; other=1");

    await api.deleteTemplate(7);

    expect((calls[0][1]?.headers as Record<string, string>)["x-csrf-token"]).toBe("abc/123");
  });

  it("falls back to the status text when the body is not an API error", async () => {
    // A reverse proxy answering with HTML: nothing to branch on, so nothing is
    // invented -- code stays null and the message is the best the transport had.
    stubFetch(new Response("<html>bad gateway</html>", { status: 502, statusText: "Bad Gateway" }));

    const failure = await api.tasks().then(
      () => null,
      (cause: unknown) => cause
    );

    const error = failure as ApiRequestError;
    expect(error.status).toBe(502);
    expect(error.code).toBeNull();
    expect(errorCode(error)).toBeNull();
    expect(error.message).toBe("Bad Gateway");
  });
});

describe("errorCode (what a caller may branch on)", () => {
  it("is null for anything that was not a rejected API response", () => {
    expect(errorCode(new Error("network down"))).toBeNull();
    expect(errorCode("boom")).toBeNull();
    expect(errorCode(undefined)).toBeNull();
    expect(errorCode(null)).toBeNull();
  });
});
