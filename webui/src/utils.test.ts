import { describe, expect, it } from "vitest";
import { formatRunTime } from "./utils";

describe("formatRunTime", () => {
  it("describes a task without runs", () => {
    expect(formatRunTime(null)).toBe("尚未运行");
  });

  it("formats unix timestamps", () => {
    expect(formatRunTime(1_700_000_000, "en-GB", "UTC")).toMatch(/14\/11.*22:13/);
  });
});
