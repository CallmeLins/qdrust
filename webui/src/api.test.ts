import { describe, expect, it } from "vitest";
import { apiPath, prefixFromPathname } from "./api";

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
