import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ref } from "vue";
import {
  DEFAULT_PAGE_SIZE,
  PAGE_SIZE_OPTIONS,
  PAGE_SIZE_STORAGE_KEY,
  clampPage,
  pageCount,
  pageSlice,
  readPageSize,
  showsNav,
  usePager,
  writePageSize,
} from "./pagination";
import type { StorageLike } from "./utils";

function memoryStorage(seed: Record<string, string> = {}): StorageLike & { entries: Record<string, string> } {
  const entries = { ...seed };
  return {
    entries,
    getItem: (key: string) => (key in entries ? entries[key] : null),
    setItem: (key: string, value: string) => { entries[key] = value; },
    removeItem: (key: string) => { delete entries[key]; },
  };
}

/** Watch effects land on the microtask queue; one turn is enough for the
 *  pager's clamp/reset to have been applied. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("page size preference", () => {
  it("falls back to the default when nothing is stored", () => {
    expect(readPageSize(memoryStorage())).toBe(DEFAULT_PAGE_SIZE);
    expect(readPageSize(null)).toBe(DEFAULT_PAGE_SIZE);
  });

  it("reads back a stored size", () => {
    expect(readPageSize(memoryStorage({ [PAGE_SIZE_STORAGE_KEY]: "50" }))).toBe(50);
  });

  it("ignores a stored value that is not one of the offered sizes", () => {
    for (const stored of ["0", "-10", "7", "1000", "abc", ""]) {
      expect(readPageSize(memoryStorage({ [PAGE_SIZE_STORAGE_KEY]: stored }))).toBe(DEFAULT_PAGE_SIZE);
    }
  });

  it("only stores offered sizes", () => {
    const storage = memoryStorage();
    writePageSize(storage, 100);
    expect(storage.getItem(PAGE_SIZE_STORAGE_KEY)).toBe("100");
    writePageSize(storage, 7);
    expect(storage.getItem(PAGE_SIZE_STORAGE_KEY)).toBe("100");
  });

  it("offers ten as the smallest size", () => {
    expect(PAGE_SIZE_OPTIONS[0]).toBe(DEFAULT_PAGE_SIZE);
  });
});

describe("page arithmetic", () => {
  it("counts pages, never fewer than one", () => {
    expect(pageCount(0, 10)).toBe(1);
    expect(pageCount(10, 10)).toBe(1);
    expect(pageCount(11, 10)).toBe(2);
    expect(pageCount(200, 50)).toBe(4);
  });

  it("falls back to the default size for a nonsense page size", () => {
    expect(pageCount(25, 0)).toBe(3);
    expect(pageCount(25, Number.NaN)).toBe(3);
  });

  it("clamps a page number into range", () => {
    expect(clampPage(0, 25, 10)).toBe(1);
    expect(clampPage(3, 25, 10)).toBe(3);
    expect(clampPage(99, 25, 10)).toBe(3);
    expect(clampPage(Number.NaN, 25, 10)).toBe(1);
  });

  it("slices the requested page", () => {
    const rows = Array.from({ length: 25 }, (_, i) => i + 1);
    expect(pageSlice(rows, 1, 10)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    expect(pageSlice(rows, 3, 10)).toEqual([21, 22, 23, 24, 25]);
    expect(pageSlice(rows, 2, 20)).toEqual([21, 22, 23, 24, 25]);
  });

  it("renders the last page rather than nothing when the page number is stale", () => {
    const rows = Array.from({ length: 25 }, (_, i) => i + 1);
    expect(pageSlice(rows, 9, 10)).toEqual([21, 22, 23, 24, 25]);
    expect(pageSlice([], 4, 10)).toEqual([]);
  });
});

describe("footer navigation", () => {
  it("hides the buttons only when there is nowhere to go", () => {
    expect(showsNav(1)).toBe(false);
    expect(showsNav(2)).toBe(true);
  });

  it("keeps the buttons for a cursor-paged list, which has no last page", () => {
    // The run log is walked one cursor at a time, so having fetched only one
    // page so far says nothing about whether another one is waiting.
    expect(showsNav(undefined)).toBe(true);
  });
});

/**
 * The footer holds the page-size picker and is the only place that setting can
 * be changed. So the footer has to survive the reader's own choice: any rule
 * that made its visibility depend on the list length would let a reader hide
 * the way back to a smaller page — most sharply by choosing 100 rows a page and
 * then opening a list short enough to fit on one. `usePager` therefore exposes
 * nothing to gate a footer on, and the two tests below hold it that way.
 */
describe("footer reachability", () => {
  it("keeps every row on screen whenever the navigation is hidden", () => {
    for (const size of PAGE_SIZE_OPTIONS) {
      for (let total = 0; total <= DEFAULT_PAGE_SIZE * 2; total += 1) {
        const rows = ref(Array.from({ length: total }, (_, i) => i + 1));
        const pager = usePager(() => rows.value, () => size);
        // Rows may only go unseen when there is a footer saying so.
        if (!showsNav(pager.pages.value)) {
          expect(pager.items.value.length, `${total} rows at ${size} a page`).toBe(total);
        }
      }
    }
  });

  it("leaves a short list whole, with its size picker still on screen", () => {
    // The lock-out in miniature: 100 rows a page, 8 rows on the list. There is
    // no second page and so no navigation, but the list is complete and
    // `footer wiring` below makes sure the picker is still rendered with it.
    const rows = ref(Array.from({ length: 8 }, (_, i) => i + 1));
    const pager = usePager(() => rows.value, () => 100);
    expect(pager.pages.value).toBe(1);
    expect(showsNav(pager.pages.value)).toBe(false);
    expect(pager.items.value).toHaveLength(8);
  });

  it("is given no way to hide its own footer", () => {
    const pager = usePager<number>(() => [], () => 10);
    expect(Object.keys(pager).sort()).toEqual(["items", "next", "page", "pages", "prev", "reset", "total"]);
  });
});

/**
 * The rule that keeps the picker reachable is structural, so it is checked in
 * the template rather than in a function. Two ways to break it: put a footer
 * behind a `v-if` (the picker leaves with it), or give one list a private page
 * size (its footer then offers a different setting from every other list).
 */
describe("footer wiring in App.vue", () => {
  const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
  const footers = app.match(/<Pager\b[\s\S]*?\/>/g) ?? [];

  it("finds the footers, so the checks below are not vacuous", () => {
    expect(footers.length).toBeGreaterThan(0);
  });

  it("renders every footer unconditionally", () => {
    for (const footer of footers) {
      expect(footer, "a footer behind a v-if takes the page-size picker with it").not.toMatch(/\bv-if\b/);
    }
  });

  it("drives every footer's picker from the one stored page size", () => {
    for (const footer of footers) {
      expect(footer, "every footer has to write the shared page size").toMatch(/:page-size="pageSize"/);
    }
  });
});

describe("usePager", () => {
  it("pages its source and reports the total", () => {
    const rows = ref(Array.from({ length: 25 }, (_, i) => i + 1));
    const pager = usePager(() => rows.value, () => 10);
    expect(pager.total.value).toBe(25);
    expect(pager.pages.value).toBe(3);
    expect(pager.items.value).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
  });

  it("stops at both ends", () => {
    const rows = ref([1, 2, 3, 4, 5]);
    const pager = usePager(() => rows.value, () => 2);
    pager.prev();
    expect(pager.page.value).toBe(1);
    pager.next();
    pager.next();
    expect(pager.page.value).toBe(3);
    pager.next();
    expect(pager.page.value).toBe(3);
  });

  it("goes back to page one when the page size changes", async () => {
    const rows = ref(Array.from({ length: 25 }, (_, i) => i + 1));
    const size = ref(10);
    const pager = usePager(() => rows.value, () => size.value);
    pager.next();
    expect(pager.page.value).toBe(2);
    size.value = 50;
    await flush();
    expect(pager.pages.value).toBe(1);
    expect(pager.page.value).toBe(1);
    expect(pager.items.value).toHaveLength(25);
  });

  it("clamps when the list shrinks under the reader", async () => {
    const rows = ref(Array.from({ length: 25 }, (_, i) => i + 1));
    const pager = usePager(() => rows.value, () => 10);
    pager.next();
    pager.next();
    expect(pager.page.value).toBe(3);
    rows.value = [1, 2, 3];
    await flush();
    expect(pager.page.value).toBe(1);
    expect(pager.items.value).toEqual([1, 2, 3]);
  });

  it("goes back to page one on reset", () => {
    const rows = ref(Array.from({ length: 30 }, (_, i) => i + 1));
    const pager = usePager(() => rows.value, () => 10);
    pager.next();
    pager.next();
    pager.reset();
    expect(pager.page.value).toBe(1);
  });
});
