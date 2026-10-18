import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
const css = readFileSync(new URL("./style.css", import.meta.url), "utf8");

describe("notepad slots", () => {
  it("lay out as a responsive tile grid", () => {
    expect(app).toContain('<div class="notepad-grid">');
    expect(css).toMatch(
      /\.notepad-grid\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:\s*repeat\(auto-fill,\s*minmax\(230px,\s*1fr\)\)/s,
    );
    expect(css).not.toContain(".notepad-layout {");
  });

  it("open a slot's editor in a dialog", () => {
    expect(app).toContain('v-if="notepadDialogOpen" class="modal-backdrop"');
    expect(app).toContain('class="modal modal-notepad"');
    expect(app).not.toContain('class="notepad-editor"');
  });
});

describe("settings account band", () => {
  it("uses a compact profile summary instead of field cards", () => {
    expect(app).toContain('<div class="settings-profile">');
    expect(app).toContain('<div class="settings-profile-meta">');
    expect(css).toMatch(
      /\.settings-profile-meta\s*\{[^}]*display:\s*flex;[^}]*flex-wrap:\s*wrap;/s,
    );
    expect(css).not.toContain(".settings-identity {");
  });

  it("opens password change from a heading action in a dialog", () => {
    expect(app).toContain("@click=\"openPasswordDialog\"");
    expect(app).toContain('v-if="showPasswordDialog" class="modal-backdrop"');
    expect(app).toContain('class="modal modal-password"');
    expect(app).not.toContain('class="settings-form settings-password"');
  });

  it("keeps token creation out of the persistent settings page", () => {
    expect(app).toContain('id="system-access-tokens"');
    expect(app).toContain('v-if="showTokenDialog" class="modal-backdrop"');
    expect(app).toContain('class="modal modal-token"');
    expect(app).not.toContain('class="settings-form settings-form-row" @submit.prevent="createApiToken"');
  });

  it("lays token rows out as a compact management list", () => {
    expect(css).toMatch(
      /\.token-row\s*\{[^}]*display:\s*flex;[^}]*align-items:\s*center;/s,
    );
    expect(css).toMatch(/\.token-row \.icon-button\s*\{[^}]*margin-left:\s*auto;/s);
  });
});
