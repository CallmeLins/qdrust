import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Personal access tokens (issue #41): the settings panel that mints and revokes
 * them, and the API client it calls. Structural, like task-form.test.ts — this
 * repo has no component harness.
 */
const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
const api = readFileSync(new URL("./api.ts", import.meta.url), "utf8");

describe("API token settings", () => {
  it("loads tokens when the settings view opens", () => {
    expect(app).toContain("async function openSettings");
    expect(app).toMatch(/@click\.prevent="openSettings"/);
  });

  it("creates a token, shows the plaintext once, and revokes", () => {
    expect(app).toContain("async function createApiToken");
    expect(app).toContain("newToken");
    expect(app).toContain("async function revokeApiToken");
  });

  it("talks to the token endpoints through the API client", () => {
    expect(api).toContain('"/api/v1/tokens"');
    expect(api).toContain("deleteApiToken");
  });
});
