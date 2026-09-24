import { beforeEach, describe, expect, it, vi } from "vitest";

/** Minimal localStorage stand-in: `i18n` reads it at import time and the test
 *  environment is node (no DOM), so the module has to be re-imported per case. */
function storageStub(initial: Record<string, string> = {}): Storage {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => map.get(key) ?? null,
    setItem: (key: string, value: string) => void map.set(key, value),
    removeItem: (key: string) => void map.delete(key),
    clear: () => map.clear(),
    key: (index: number) => [...map.keys()][index] ?? null,
    get length() {
      return map.size;
    }
  } as Storage;
}

async function loadI18n(stored?: Record<string, string>) {
  vi.resetModules();
  const storage = storageStub(stored);
  vi.stubGlobal("localStorage", storage);
  return { i18n: await import("./i18n"), storage };
}

beforeEach(() => {
  vi.unstubAllGlobals();
});

describe("isLocale", () => {
  it("accepts the two shipped languages", async () => {
    const { i18n } = await loadI18n();
    expect(i18n.isLocale("zh-CN")).toBe(true);
    expect(i18n.isLocale("en-US")).toBe(true);
  });

  it("rejects anything else, including values an older build may have stored", async () => {
    const { i18n } = await loadI18n();
    for (const value of ["fr", "zh", "en_US", "", null, undefined, 42]) {
      expect(i18n.isLocale(value)).toBe(false);
    }
  });
});

describe("applyDefaultLocale (QDRUST_DEFAULT_LOCALE)", () => {
  it("adopts the deployment default when the visitor has not chosen", async () => {
    const { i18n } = await loadI18n();
    expect(i18n.locale.value).toBe("zh-CN");
    i18n.applyDefaultLocale("en-US");
    expect(i18n.locale.value).toBe("en-US");
  });

  it("never overrides a choice the visitor made", async () => {
    const { i18n } = await loadI18n({ "qdrust.locale": "en-US" });
    i18n.applyDefaultLocale("zh-CN");
    expect(i18n.locale.value).toBe("en-US");
  });

  it("keeps the built-in default for an unset or unrecognised value", async () => {
    const { i18n } = await loadI18n();
    for (const value of [undefined, null, "", "fr", "en-GB"]) {
      i18n.applyDefaultLocale(value);
      expect(i18n.locale.value).toBe("zh-CN");
    }
  });

  it("does not store the server's default as if the visitor had chosen it", async () => {
    // Otherwise an admin changing QDRUST_DEFAULT_LOCALE later would reach
    // nobody: every browser would already look like it had picked for itself.
    const { i18n, storage } = await loadI18n();
    i18n.applyDefaultLocale("en-US");
    expect(storage.getItem("qdrust.locale")).toBeNull();
  });

  it("is what toggleLocale still writes, so the two agree on the key", async () => {
    const { i18n, storage } = await loadI18n();
    i18n.toggleLocale();
    expect(storage.getItem("qdrust.locale")).toBe("en-US");
    expect(i18n.locale.value).toBe("en-US");
  });
});
