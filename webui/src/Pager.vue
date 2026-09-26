<script setup lang="ts">
import { computed } from "vue";
import { ChevronLeft, ChevronRight } from "@lucide/vue";
import Dropdown from "./Dropdown.vue";
import { fmt, t } from "./i18n";
import { PAGE_SIZE_OPTIONS, showsNav } from "./pagination";

/**
 * The footer under a paged list: how much there is, where the reader is, and
 * how many rows a page holds.
 *
 * The row count lives here rather than on a settings page: the moment a reader
 * wants a different page size is the moment they are looking at a list, and
 * making them leave to change it loses their place. The picker shows what it
 * will change — the count next to it — instead of asking them to imagine it.
 *
 * Cursor-paged lists (the aggregated run log) have no row count to report and
 * no last page to bound, so `pages` and `total` are optional and such callers
 * drive the buttons through `prevDisabled` / `nextDisabled` instead.
 *
 * The footer itself is not a good place to be clever about whether to appear:
 * it carries the only page-size control, so it renders whenever its list does.
 * Only the navigation inside may hide — on a one-page list it has nothing to
 * do — and the count and the picker stay even then, so a short list reads as a
 * compact status line rather than losing its way back to a smaller page.
 *
 * Laid out as three columns — count, navigation, size picker — so the buttons
 * are centred in the row itself rather than in whatever space the two sides
 * leave over. See `.pager` in style.css: the difference is 29px.
 */
const props = defineProps<{
  page: number;
  pageSize?: number;
  /** Last page number; omitted by cursor paging. */
  pages?: number;
  /** Row count to report; omitted by cursor paging. */
  total?: number;
  /** Disables both buttons while a page is in flight. */
  busy?: boolean;
  prevDisabled?: boolean;
  nextDisabled?: boolean;
  /** Nav-only form: just the prev/position/next pair, no count and no picker.
   *  This is the strip a paged table carries *above* it (issue #29) — a second
   *  pair of buttons so a reader never has to travel to the footer to turn the
   *  page, and the thing paging anchors back to. The footer below the table
   *  stays the one place the row count is set; duplicating the picker here
   *  would be two controls writing one number on one screen. */
  navOnly?: boolean;
}>();

const emit = defineEmits<{
  (e: "prev"): void;
  (e: "next"): void;
  (e: "update:pageSize", value: number): void;
}>();

const sizeOptions = computed(() => PAGE_SIZE_OPTIONS.map((size) => ({ value: size, label: fmt("pageSizeUnit", { n: size }) })));
const canPrev = computed(() => (props.prevDisabled != null ? !props.prevDisabled : props.page > 1));
const canNext = computed(() => (props.nextDisabled != null ? !props.nextDisabled : props.pages != null && props.page < props.pages));

/** Only the navigation is allowed to come and go: on a one-page list it has
 *  nothing to do. The count and the picker stay regardless, because the picker
 *  is the only way to change the page size — see `usePager`. */
const navVisible = computed(() => showsNav(props.pages));
</script>

<template>
  <div v-if="!navOnly || navVisible" class="pager">
    <span v-if="total != null && !navOnly" class="muted pager-total">{{ fmt('pageTotal', { n: total }) }}</span>
    <span v-if="navVisible" class="pager-nav">
      <button class="secondary-button" type="button" :disabled="busy || !canPrev" @click="emit('prev')"><ChevronLeft :size="15" />{{ t('prevPage') }}</button>
      <span class="muted pager-position">{{ pages != null ? fmt('pageOf', { n: page, total: pages }) : fmt('runLogPageNo', { n: page }) }}</span>
      <button class="secondary-button" type="button" :disabled="busy || !canNext" @click="emit('next')">{{ t('nextPage') }}<ChevronRight :size="15" /></button>
    </span>
    <label v-if="!navOnly" class="pager-size">
      <span>{{ t('pageSizeLabel') }}</span>
      <Dropdown :model-value="pageSize ?? null" :options="sizeOptions" compact @change="(value) => emit('update:pageSize', Number(value))" />
    </label>
  </div>
</template>
