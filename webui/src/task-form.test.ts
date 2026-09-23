import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The task form's "bind to template" dropdown, read off App.vue.
 *
 * The ordering rule itself is unit-tested in utils.test.ts. What cannot be seen
 * from there is whether the form still asks for it: templates that no task uses
 * yet have to come first, and if that call is dropped the list quietly goes back
 * to creation order — the whole request in issue #25, undone with nothing else
 * in the app noticing. Structural assertions on App.vue, like
 * pagination.test.ts: this repo has no @vue/test-utils and does not add a
 * dependency for one form.
 */
const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");

describe("task form template dropdown", () => {
  const options = app.slice(
    app.indexOf("const templateDropdownOptions"),
    app.indexOf("\n});", app.indexOf("const templateDropdownOptions"))
  );

  it("finds the dropdown options, so the checks below are not vacuous", () => {
    expect(options).not.toBe("");
    expect(options).toContain("templatesForSelect");
  });

  it("offers the templates no task uses first", () => {
    expect(options).toContain("unboundTemplatesFirst(");
  });

  it("reads the ids in use off the loaded tasks, not a constant", () => {
    expect(options).toMatch(/\.map\(\(task\)\s*=>\s*task\.template_id\)/);
  });

  it("still offers the no-template fallback, and only on the edit path", () => {
    expect(options).toContain('t("noTemplatesToBind")');
    expect(options).toMatch(/taskForm\.id\s*==\s*null/);
  });
});
