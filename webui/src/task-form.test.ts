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

  it("offers the templates no task uses first, newest first inside each half", () => {
    expect(options).toContain("orderTemplatesForNewTask(");
  });

  it("reads the ids in use off the loaded tasks, not a constant", () => {
    expect(options).toMatch(/\.map\(\(task\)\s*=>\s*task\.template_id\)/);
  });

  it("also counts the server's per-template task_count, so a stale task list cannot hide a bound template", () => {
    expect(options).toContain("task_count");
  });

  it("labels each option with its use, so the ordering is legible", () => {
    expect(options).toContain("templateUnused");
    expect(options).toContain("templateUsedCount");
  });

  it("offers the dropdown a filter box, so hundreds of templates can be searched", () => {
    expect(app).toMatch(/<Dropdown[^>]*v-model="taskForm\.templateId"[^>]*filterable/);
  });

  it("still offers the no-template fallback, and only on the edit path", () => {
    expect(options).toContain('t("noTemplatesToBind")');
    expect(options).toMatch(/taskForm\.id\s*==\s*null/);
  });
});

/**
 * Filling the variable rows when a template is chosen.
 *
 * A `watch` on `taskForm.templateId` looked equivalent and was not: cancelling
 * the dialog leaves the form as it was, so "new task from the same template"
 * reset the id to null and set it back within one tick. The watcher saw X -> X,
 * never fired, and the username/password rows stayed empty on the retry.
 */
describe("task form template variables", () => {
  function block(startMarker: string): string {
    const start = app.indexOf(startMarker);
    // The file is checked out with CRLF on Windows, so match the brace, not a
    // literal "\n}\n".
    const end = app.indexOf("\n}", start);
    return start < 0 || end < 0 ? "" : app.slice(start, end + 2);
  }

  it("finds the handler, so the checks below are not vacuous", () => {
    const picked = block("function onTemplatePicked");
    expect(picked).toContain("taskForm.variables");
    expect(picked).toContain("tmpl.variables");
  });

  it("does not rely on a value-change watcher that a cancel-and-retry defeats", () => {
    expect(app).not.toContain("watch(() => taskForm.templateId");
  });

  it("populates on the templates-page button and on a dropdown pick", () => {
    expect(block("function createTaskFromTemplate")).toContain("onTemplatePicked()");
    expect(app).toMatch(/<Dropdown[^>]*v-model="taskForm\.templateId"[^>]*@change="onTemplatePicked"/);
  });

  it("seeds each row from the template's declared defaults", () => {
    expect(block("function onTemplatePicked")).toContain("variable_defaults");
  });

  it("offers a test run for the selected template", () => {
    expect(app).toContain("api.testTemplate(");
    expect(app).toMatch(/@click="runTemplateTest"/);
  });
});
