import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The notification page, across the four files that have to agree about it.
 *
 * Issues #43 (nothing to preview, no `{log}` in the default wording, no Chinese
 * 成功/失败), #44 (28 flat rows for 14 tasks) and #45 (no per-account default
 * wording) were fixed on both sides of the wire, and every link between them is
 * a *name*: a route string in `api.ts` that the server has to have registered, a
 * translation key App.vue asks for, an endpoint the page calls. None of those
 * are checked by a compiler, and a typo in any one of them is a feature that
 * renders, clicks, and silently does nothing.
 *
 * Structural assertions on sources, like admin-settings.test.ts and
 * task-form.test.ts: this repo has no @vue/test-utils and does not add a
 * dependency for one page. The grouping logic itself is unit-tested in
 * utils.test.ts, and the endpoints end-to-end in qdrust-server's api tests.
 */
const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
const api = readFileSync(new URL("./api.ts", import.meta.url), "utf8");
const i18n = readFileSync(new URL("./i18n.ts", import.meta.url), "utf8");
const server = readFileSync(new URL("../../crates/qdrust-server/src/api.rs", import.meta.url), "utf8");

describe("notification routes", () => {
  /** Both spellings of each route: the template `api.ts` builds, and the literal
   *  the server registers. */
  const routes = [
    { requested: "/api/v1/notification-actions/${id}/preview", registered: "/api/v1/notification-actions/{id}/preview" },
    { requested: "/api/v1/notification-defaults", registered: "/api/v1/notification-defaults" },
    { requested: "/api/v1/notification-defaults/${event}", registered: "/api/v1/notification-defaults/{event}" },
  ];

  it("is requested by the WebUI with the same spelling the server registers", () => {
    for (const route of routes) {
      expect(api, `${route.registered} is not requested by api.ts`).toContain(route.requested);
      expect(server, `${route.registered} is not registered by the server`).toContain(`"${route.registered}"`);
    }
  });
});

describe("notification defaults editor", () => {
  it("loads the account's pair and saves one event at a time", () => {
    expect(app).toContain("api.notificationDefaults()");
    expect(app).toContain("loadNotifyDefaults();");
    expect(app).toMatch(/api\.setNotificationDefault\(defaultsEvent\.value/);
  });

  /* The built-in pair is the last resort in the fallback chain, so the editor
     shows it as the placeholder rather than repeating it as body text: clearing
     a box has to be visibly the same as "use the built-in". */
  it("shows the built-in pair as the placeholder, from the server", () => {
    expect(app).toContain("notifyDefaults?.builtin.title_template");
    expect(app).toContain("notifyDefaults?.builtin.body_template");
  });

  /* A default is the wording for a known outcome; `always` belongs to a binding
     (it is what makes one fire on both), so it is not a default an account can
     write. The server rejects it too. */
  it("only offers the two events a default can exist for", () => {
    const options = app.slice(app.indexOf("const defaultsEventOptions"), app.indexOf("];", app.indexOf("const defaultsEventOptions")));
    expect(options).toContain('"success"');
    expect(options).toContain('"failure"');
    expect(options).not.toContain('"always"');
  });
});

describe("notification preview", () => {
  it("renders a saved binding through the preview endpoint", () => {
    expect(app).toContain("api.previewNotificationAction(action.id)");
    expect(app).toContain("@click=\"openActionPreview(action)\"");
  });

  it("shows the rendered title and body rather than a status message", () => {
    expect(app).toContain("{{ preview.title }}");
    expect(app).toContain("{{ preview.body }}");
  });

  /* The endpoint answers with ids and a `source`; the dialog has to say that
     run-scoped variables are empty when there was no run to render against, or
     an empty `{log}` reads as a bug in the template. */
  it("says so when there is no run behind the sample", () => {
    expect(app).toContain(`v-if="preview.source === 'sample'"`);
    expect(app).toContain("notifyPreviewSample");
  });
});

describe("bindings grouped by task", () => {
  it("renders the grouped list and no longer the flat one", () => {
    expect(app).toContain('v-for="group in actionGroups"');
    expect(app).not.toContain('v-for="action in actions"');
  });

  it("puts the bindings behind a toggle, open already for a single one", () => {
    expect(app).toContain("v-show=\"groupIsOpen(group)\"");
    expect(app).toMatch(/group\.rows\.length === 1 \|\| expandedActionTasks/);
  });

  /* The task name moved from every binding row to the group header, which is
     the change: 28 rows no longer say "任务: x" 28 times. */
  it("names the task once per group", () => {
    expect(app).toContain("<strong>{{ group.name }}</strong>");
  });
});

describe("translation keys", () => {
  const zh = i18n.slice(i18n.indexOf("const zh = {"), i18n.indexOf("const en: Record<MessageKey, string> = {"));
  const en = i18n.slice(i18n.indexOf("const en: Record<MessageKey, string> = {"));
  const keysOf = (block: string) => new Set([...block.matchAll(/^ {2}([A-Za-z0-9_]+):/gm)].map((match) => match[1]));
  const zhKeys = keysOf(zh);
  const enKeys = keysOf(en);

  it("parses both dictionaries, so the checks below are not vacuous", () => {
    expect(zhKeys.size).toBeGreaterThan(400);
    expect(enKeys.size).toBe(zhKeys.size);
  });

  /* A key present in one language and missing from the other renders as
     `undefined` for whoever picked that language — in the middle of a sentence,
     at the one moment they are looking for a notification. */
  it("keeps the two dictionaries in step", () => {
    expect([...zhKeys].filter((key) => !enKeys.has(key))).toEqual([]);
    expect([...enKeys].filter((key) => !zhKeys.has(key))).toEqual([]);
  });

  /* The other half of the same failure: a key the page asks for that no
     dictionary has. `MessageKey` catches this at build time for the literals it
     can see, which is why this assertion is cheap — it is here to stay true if
     a call ever stops being a literal. */
  it("has every key the notification page asks for", () => {
    const asked = new Set([...app.matchAll(/\b(?:t|fmt)\(\s*["']([A-Za-z0-9_]+)["']/g)].map((match) => match[1]));
    expect(asked.size).toBeGreaterThan(400);
    expect([...asked].filter((key) => !zhKeys.has(key))).toEqual([]);
    expect([...asked].filter((key) => !enKeys.has(key))).toEqual([]);
  });

  /* Issue #43's third complaint, in the shape the code has: a template could not
     produce 成功/失败, so an account with both events bound needed twice the
     bindings. The variable and its two words are pinned together. */
  it("offers the two Chinese outcome words a template can print", () => {
    for (const key of ["notifyVarsHint", "notifyPreview", "notifyCustomTemplate", "notifyDefaults"]) {
      expect(zhKeys, `${key} is missing from the Chinese dictionary`).toContain(key);
      expect(enKeys, `${key} is missing from the English dictionary`).toContain(key);
    }
    expect(zh).toContain("{status_cn}");
    expect(en).toContain("{status_cn}");
  });
});
