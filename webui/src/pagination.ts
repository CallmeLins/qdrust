import { computed, ref, watch, type ComputedRef, type Ref } from "vue";
import type { StorageLike } from "./utils";

/**
 * Client-side paging for the long lists (tasks, subscriptions, templates, the
 * two library tables).
 *
 * Every list here is already fully in hand — the API returns the whole set and
 * the page filters it in the browser — so paging is a rendering concern: the
 * point is not to fetch less, it is that a list of two hundred rows is
 * unreadable and pushing the rest of the page below the fold.
 *
 * The page size is a single stored preference rather than a per-list constant,
 * because the reader's appetite for density is a property of the reader, not of
 * the list. Lists opt in by asking for a pager; the count applies to all of
 * them, and a new list gets it for free.
 */

/** Row counts a reader can choose from. */
export const PAGE_SIZE_OPTIONS: readonly number[] = [10, 20, 50, 100];
export type PageSize = (typeof PAGE_SIZE_OPTIONS)[number];
/** Ten rows: enough to see a list's shape, few enough that one screen holds it. */
export const DEFAULT_PAGE_SIZE = 10;
export const PAGE_SIZE_STORAGE_KEY = "qdrust.pageSize";

function isPageSize(value: number): value is PageSize {
  return PAGE_SIZE_OPTIONS.includes(value);
}

/** The stored page size, or the default when nothing usable is stored. */
export function readPageSize(storage: StorageLike | null | undefined): number {
  const raw = storage?.getItem(PAGE_SIZE_STORAGE_KEY);
  const size = raw == null ? Number.NaN : Number(raw);
  return isPageSize(size) ? size : DEFAULT_PAGE_SIZE;
}

/** Persist a page size. A value outside the offered set is dropped rather than
 *  stored: a hand-edited localStorage entry should not be able to make every
 *  list in the app render a single row per page. */
export function writePageSize(storage: StorageLike | null | undefined, size: number): void {
  if (!isPageSize(size)) return;
  storage?.setItem(PAGE_SIZE_STORAGE_KEY, String(size));
}

/** Coerce a page size into something the arithmetic below can use. */
function usableSize(pageSize: number): number {
  return Number.isFinite(pageSize) && pageSize >= 1 ? Math.floor(pageSize) : DEFAULT_PAGE_SIZE;
}

/** How many pages `total` rows take. Never zero: an empty list reads "page 1 of
 *  1" rather than "page 1 of 0". */
export function pageCount(total: number, pageSize: number): number {
  return Math.max(1, Math.ceil(Math.max(0, total) / usableSize(pageSize)));
}

/** Pull a page number back into the range the list currently has. */
export function clampPage(page: number, total: number, pageSize: number): number {
  if (!Number.isFinite(page)) return 1;
  return Math.min(Math.max(1, Math.floor(page)), pageCount(total, pageSize));
}

/** The rows of one page (1-based). Clamps first, so a page number left over
 *  from a longer list renders the last page instead of nothing. */
export function pageSlice<T>(items: readonly T[], page: number, pageSize: number): T[] {
  const size = usableSize(pageSize);
  const start = (clampPage(page, items.length, size) - 1) * size;
  return items.slice(start, start + size);
}

/** Whether a client-paged list has a second page to turn to. Cursor-paged
 *  lists pass `undefined` — their next page is fetched, not counted — and keep
 *  their buttons in every state.
 *
 *  Only the navigation is allowed to come and go. The footer that holds it
 *  holds the page-size picker too, and that one has to stay on screen whatever
 *  the list length is, which is why `usePager` no longer hands out anything
 *  that could hide the footer as a whole. */
export function showsNav(pages: number | undefined): boolean {
  return pages == null || pages > 1;
}

export interface Pager<T> {
  /** 1-based current page. */
  page: Ref<number>;
  pages: ComputedRef<number>;
  /** The rows to render. */
  items: ComputedRef<T[]>;
  total: ComputedRef<number>;
  prev: () => void;
  next: () => void;
  /** Back to page one, for callers whose list is about to change shape (a new
   *  search term, a new filter) and for whom the old page number is meaningless. */
  reset: () => void;
}

/**
 * Pager over a client-side list.
 *
 * `source` is a getter rather than a ref so the caller keeps ownership of
 * filtering and sorting: pass `() => filteredTasks.value` and the pager pages
 * whatever the filters left. `pageSize` is a getter too, so the one stored
 * preference flows into every list without them having to know about it.
 *
 * Note the absence of a `show`: callers render their footer unconditionally.
 * The footer is the only place the page size is set, so any rule that made the
 * footer depend on the list would hand the reader a way to lock themselves out
 * — pick 100 rows a page, open a list of fewer than 100, and the footer goes
 * with the picker inside it, leaving 10 unreachable. Comparing against the
 * smallest offered size instead of the chosen one moves that boundary; it does
 * not remove it. Always rendering the footer, and gating only the navigation on
 * `showsNav`, has no boundary to be caught on the wrong side of. App.vue holds
 * up its end — no `<Pager>` behind a `v-if` — and pagination.test.ts checks it.
 */
export function usePager<T>(source: () => readonly T[], pageSize: () => number): Pager<T> {
  const page = ref(1);
  const total = computed(() => source().length);
  const pages = computed(() => pageCount(total.value, pageSize()));
  const items = computed(() => pageSlice(source(), page.value, pageSize()));

  // A list that shrinks while the reader is on it — a delete, a narrower
  // filter, a source that failed — must not strand them on a page that no
  // longer exists.
  watch(pages, (last) => {
    if (page.value > last) page.value = last;
  });
  // Re-chunking by a different row count invalidates the page number, so go
  // back to the top rather than show an arbitrary slice of the new layout.
  watch(pageSize, () => {
    page.value = 1;
  });

  function prev(): void {
    if (page.value > 1) page.value -= 1;
  }
  function next(): void {
    if (page.value < pages.value) page.value += 1;
  }
  function reset(): void {
    page.value = 1;
  }
  return { page, pages, items, total, prev, next, reset };
}
