import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");

describe("breadcrumb sections", () => {
  it("uses the sidebar group as the breadcrumb prefix", () => {
    expect(app).toContain("const currentViewSection = computed");
    expect(app).toContain('<div class="breadcrumb">{{ currentViewSection }}');
  });

  it("groups notifications and push under automation", () => {
    expect(app).toMatch(
      /view\.value === "notifications" \|\| view\.value === "push"\) return t\("navAutomation"\)/,
    );
  });

  it("groups admin and settings under management and settings", () => {
    expect(app).toMatch(
      /view\.value === "admin" \|\| view\.value === "settings"\) return t\("navManagement"\)/,
    );
  });
});
