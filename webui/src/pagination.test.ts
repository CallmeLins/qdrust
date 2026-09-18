import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ref } from "vue";
import {
  DEFAULT_PAGE_SIZE,
  LEGACY_PAGE_SIZE_STORAGE_KEY,
  PAGE_SIZE_OPTIONS,
  PAGE_SIZE_SCOPES,
  clampPage,
  pageCount,
  pageSizeKey,
  pageSlice,
  readPageSize,
  showsNav,
  useCursorPager,
  usePageSize,
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
    expect(readPageSize(memoryStorage(), "tasks")).toBe(DEFAULT_PAGE_SIZE);
    expect(readPageSize(null, "tasks")).toBe(DEFAULT_PAGE_SIZE);
  });

  it("reads back a size stored for that list", () => {
    expect(readPageSize(memoryStorage({ [pageSizeKey("tasks")]: "50" }), "tasks")).toBe(50);
  });

  it("keeps one count per list, so changing one leaves the others alone", () => {
    // The behaviour this replaced: a single shared count meant the footer of any
    // list silently re-chunked every other one.
    const storage = memoryStorage();
    writePageSize(storage, "tasks", 50);
    writePageSize(storage, "library", 20);
    expect(readPageSize(storage, "tasks")).toBe(50);
    expect(readPageSize(storage, "library")).toBe(20);
    expect(readPageSize(storage, "subscriptions")).toBe(DEFAULT_PAGE_SIZE);
  });

  it("gives every list its own key, none of them the shared one", () => {
    const keys = PAGE_SIZE_SCOPES.map(pageSizeKey);
    expect(new Set(keys).size).toBe(PAGE_SIZE_SCOPES.length);
    expect(keys).not.toContain(LEGACY_PAGE_SIZE_STORAGE_KEY);
  });

  it("carries a count stored before the lists were separated into each of them", () => {
    // An upgrade, not a fresh browser: the reader's single old choice is the
    // starting point for every list, and each one moves off it independently.
    const storage = memoryStorage({ [LEGACY_PAGE_SIZE_STORAGE_KEY]: "50" });
    expect(readPageSize(storage, "tasks")).toBe(50);
    expect(readPageSize(storage, "runLog")).toBe(50);
    writePageSize(storage, "tasks", 10);
    expect(readPageSize(storage, "tasks")).toBe(10);
    expect(readPageSize(storage, "runLog")).toBe(50);
  });

  it("ignores a stored value that is not one of the offered sizes", () => {
    for (const stored of ["0", "-10", "7", "1000", "abc", ""]) {
      expect(readPageSize(memoryStorage({ [pageSizeKey("tasks")]: stored }), "tasks")).toBe(DEFAULT_PAGE_SIZE);
    }
    for (const stored of ["0", "-10", "7", "abc"]) {
      expect(readPageSize(memoryStorage({ [LEGACY_PAGE_SIZE_STORAGE_KEY]: stored }), "tasks")).toBe(
        DEFAULT_PAGE_SIZE,
      );
    }
  });

  it("only stores offered sizes", () => {
    const storage = memoryStorage();
    writePageSize(storage, "tasks", 100);
    expect(storage.getItem(pageSizeKey("tasks"))).toBe("100");
    writePageSize(storage, "tasks", 7);
    expect(storage.getItem(pageSizeKey("tasks"))).toBe("100");
  });

  it("offers ten as the smallest size", () => {
    expect(PAGE_SIZE_OPTIONS[0]).toBe(DEFAULT_PAGE_SIZE);
  });
});

describe("usePageSize", () => {
  it("starts from what the list stored and writes back only its own", () => {
    const storage = memoryStorage({ [pageSizeKey("runLog")]: "20" });
    const { size, setSize } = usePageSize(storage, "runLog");
    expect(size.value).toBe(20);
    setSize(100);
    expect(size.value).toBe(100);
    expect(storage.getItem(pageSizeKey("runLog"))).toBe("100");
    expect(storage.getItem(pageSizeKey("tasks"))).toBeNull();
  });

  it("ignores a repeated value and a value it does not offer", () => {
    const storage = memoryStorage();
    const { size, setSize } = usePageSize(storage, "tasks");
    setSize(50);
    setSize(50);
    expect(storage.entries[pageSizeKey("tasks")]).toBe("50");
    setSize(7);
    expect(size.value).toBe(50);
    expect(storage.entries[pageSizeKey("tasks")]).toBe("50");
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
 * The rules that keep a row count both reachable and private are structural, so
 * they are checked in the template rather than in a function. Three ways to
 * break them: put a footer behind a `v-if` (the picker leaves with it), let two
 * lists share a count (their footers then fight -- the behaviour this replaced),
 * or bind a footer to another list's setter (it shows one number and writes
 * another).
 *
 * The run log is the one list the *server* pages, so its own count also has to
 * reach the request, and changing it has to invalidate the cursors that were cut
 * for pages of the old size.
 */
describe("footer wiring in App.vue", () => {
  const app = readFileSync(new URL("./App.vue", import.meta.url), "utf8");
  const footers = app.match(/<Pager\b[\s\S]*?\/>/g) ?? [];
  const sizes = footers.map((footer) => footer.match(/:page-size="(\w+)"/)?.[1] ?? "");

  it("finds the footers, so the checks below are not vacuous", () => {
    expect(footers.length).toBeGreaterThan(0);
    expect(sizes.every(Boolean), "every footer names the count it shows").toBe(true);
  });

  it("renders every footer unconditionally", () => {
    for (const footer of footers) {
      expect(footer, "a footer behind a v-if takes the page-size picker with it").not.toMatch(/\bv-if\b/);
    }
  });

  it("gives every footer a row count of its own", () => {
    expect(new Set(sizes).size).toBe(sizes.length);
  });

  it("writes back through the setter that belongs to its own count", () => {
    for (const size of sizes) {
      const setter = "set" + size.charAt(0).toUpperCase() + size.slice(1);
      const footer = footers[sizes.indexOf(size)];
      const writes = '@update:page-size="' + setter + '"';
      expect(footer, size + " has to be written through " + setter).toContain(writes);
    }
  });

  it("sends the run log's own row count to the server", () => {
    expect(app, "the run log is fetched, so its count has to reach the request").toMatch(
      /limit: runLogPageSize\.value/,
    );
  });

  it("restarts the run log's paging when a filter or the row count changes", () => {
    // A cursor is a key into one result set: cut for pages of the old size, or
    // for the unfiltered log, it names runs that are not in that list any more.
    // Reusing it makes a working filter look broken — the rows it matches sit
    // above the old cursor, so the fetch comes back empty. One watch over all
    // three inputs, so no individual control has to remember to reset.
    const reaction =
      app.match(/watch\(\[runLogStatus, runLogTaskId, runLogPageSize\][\s\S]*?\n\}\);/)?.[0] ?? "";
    expect(reaction, "every input that changes which runs match resets the paging").toContain(
      "resetRunLogPaging()",
    );
    expect(reaction).toContain("loadRunLog()");
  });

  it("changes the run log's filters through the refs that watch reads", () => {
    // The task filter is a `v-model` dropdown and the status filter a plain
    // setter. Neither fetches on its own, so the one path that only updates the
    // model — a keyboard pick — cannot be the path that never reloads.
    expect(app, "a filter that fetches on its own duplicates the watch").not.toMatch(
      /@change="loadRunLog\(\)"/,
    );
    const setter = app.match(/function setRunLogStatus[\s\S]*?\n\}/)?.[0] ?? "";
    expect(setter, "the status setter only sets the status").not.toContain("loadRunLog");
    expect(setter).toContain("runLogStatus.value = status");
    expect(app, "the task filter has to be wired to the watched ref").toMatch(
      /<Dropdown v-model="runLogTaskId"/,
    );
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

describe("useCursorPager", () => {
  it("starts on page one, with no cursor to send", () => {
    const pager = useCursorPager();
    expect(pager.pageNo.value).toBe(1);
    expect(pager.cursor.value).toBeUndefined();
    expect(pager.canGoBack.value).toBe(false);
  });

  it("advances onto the server's cursor and counts the pages", () => {
    const pager = useCursorPager();
    pager.advance(7);
    expect(pager.cursor.value).toBe(7);
    expect(pager.pageNo.value).toBe(2);
    expect(pager.canGoBack.value).toBe(true);
    pager.advance(3);
    expect(pager.cursor.value).toBe(3);
    expect(pager.pageNo.value).toBe(3);
  });

  it("goes back a page at a time and stops at the first", () => {
    const pager = useCursorPager();
    pager.advance(7);
    pager.advance(3);
    pager.back();
    expect(pager.cursor.value).toBe(7);
    expect(pager.pageNo.value).toBe(2);
    pager.back();
    expect(pager.cursor.value).toBeUndefined();
    expect(pager.canGoBack.value).toBe(false);
    pager.back();
    expect(pager.pageNo.value).toBe(1);
  });

  it("starts over on reset, which is what a filter change needs", () => {
    // The regression this exists for: a cursor cut for the unfiltered log names
    // runs the filtered list does not contain, so a fetch that keeps it comes
    // back empty — a working "filter by task" that looks like it does nothing.
    const pager = useCursorPager();
    pager.advance(7);
    pager.advance(3);
    pager.reset();
    expect(pager.cursor.value).toBeUndefined();
    expect(pager.pageNo.value).toBe(1);
    expect(pager.canGoBack.value).toBe(false);
  });

  it("ignores a 'there is more' that came without a cursor", () => {
    // Pushing it would put a hole in the stack: reading it back would look like
    // page one, and the footer would count a page nobody can reach.
    const pager = useCursorPager();
    pager.advance(7);
    pager.advance(null);
    expect(pager.cursor.value).toBe(7);
    expect(pager.pageNo.value).toBe(2);
  });

  it("keeps one stack per caller", () => {
    const first = useCursorPager();
    const second = useCursorPager();
    first.advance(7);
    expect(first.pageNo.value).toBe(2);
    expect(second.pageNo.value).toBe(1);
    expect(second.cursor.value).toBeUndefined();
  });
});
