<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ChevronDown } from "@lucide/vue";

export type DropdownOption = {
  value: string | number | null;
  label: string;
  disabled?: boolean;
};

const props = withDefaults(
  defineProps<{
    modelValue: string | number | null;
    options: DropdownOption[];
    /** Shown when the current modelValue is empty / not among options. */
    placeholder?: string;
    /** Compact control for toolbars / table cells; defaults to full-width like an input. */
    compact?: boolean;
    disabled?: boolean;
  }>(),
  { placeholder: "", compact: false, disabled: false }
);

const emit = defineEmits<{
  (e: "update:modelValue", value: string | number | null): void;
  /** Fired after a selection is committed so parents can react (mirrors @change). */
  (e: "change", value: string | number | null): void;
}>();

/** Gap between the trigger and the list, and the list's own height limits. */
const GAP = 4;
const MAX_LIST_HEIGHT = 260;
const MIN_LIST_HEIGHT = 96;

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const trigger = ref<HTMLElement | null>(null);
const list = ref<HTMLElement | null>(null);
/** Viewport coordinates for the teleported listbox. */
const listStyle = ref<Record<string, string>>({});

const selected = computed(() => props.options.find((o) => o.value === props.modelValue));
const selectedLabel = computed(() => selected.value?.label ?? "");
const empty = computed(() => !selected.value);

/**
 * Anchor the list to its trigger using viewport coordinates.
 *
 * The list is teleported to <body> on purpose: an absolutely positioned list
 * still belongs to the nearest scrolling ancestor, so opening a dropdown near
 * the bottom of a `.modal` grew that modal's scrollable area and pushed the
 * options below the fold — the dialog had to be scrolled before an option could
 * be clicked (issue #12). It is also what makes the `z-index` meaningful: a
 * list rendered inside a `backdrop-filter`/`overflow` container is clipped by
 * it, while a teleported one floats above everything.
 *
 * The list flips above the trigger when there is not enough room underneath.
 */
function place() {
  const anchor = trigger.value;
  if (!anchor) return;
  const rect = anchor.getBoundingClientRect();
  // The trigger scrolled out of view: drop the list instead of floating it in
  // the middle of nowhere.
  if (rect.bottom < 0 || rect.top > window.innerHeight) {
    open.value = false;
    return;
  }
  const below = window.innerHeight - rect.bottom - GAP;
  const above = rect.top - GAP;
  const flip = below < MAX_LIST_HEIGHT && above > below;
  const room = flip ? above : below;
  listStyle.value = {
    left: `${rect.left}px`,
    width: `${rect.width}px`,
    maxHeight: `${Math.min(MAX_LIST_HEIGHT, Math.max(room, MIN_LIST_HEIGHT))}px`,
    [flip ? "bottom" : "top"]: `${flip ? window.innerHeight - rect.top + GAP : rect.bottom + GAP}px`,
  };
}

function toggle() {
  if (props.disabled) return;
  open.value = !open.value;
}
function pick(option: DropdownOption) {
  if (option.disabled) return;
  emit("update:modelValue", option.value);
  emit("change", option.value);
  open.value = false;
}
function move(dir: 1 | -1) {
  if (props.disabled) return;
  const active = props.options.findIndex((o) => o.value === props.modelValue);
  let i = active < 0 ? 0 : (active + dir + props.options.length) % props.options.length;
  while (i !== active && props.options[i].disabled) {
    i = (i + dir + props.options.length) % props.options.length;
  }
  emit("update:modelValue", props.options[i].value);
}
function onDocumentMouseDown(event: MouseEvent) {
  const target = event.target as Node;
  if (root.value?.contains(target) || list.value?.contains(target)) return;
  open.value = false;
}
function onDocumentKeydown(event: KeyboardEvent) {
  if (!open.value) return;
  if (event.key === "Escape") open.value = false;
}
/** Keep the list glued to its trigger while a page or a modal scrolls. */
function onViewportChange() {
  if (open.value) place();
}
// `place()` runs in the same microtask batch as the patch that inserts the list,
// so it is already positioned by the time the browser paints — no jump/flash.
watch(open, async (isOpen) => {
  if (!isOpen) return;
  await nextTick();
  place();
});
watch(() => props.modelValue, () => { /* keep open only while choosing */ });

onMounted(() => {
  document.addEventListener("mousedown", onDocumentMouseDown);
  document.addEventListener("keydown", onDocumentKeydown);
  window.addEventListener("resize", onViewportChange);
  window.addEventListener("scroll", onViewportChange, true);
});
onBeforeUnmount(() => {
  document.removeEventListener("mousedown", onDocumentMouseDown);
  document.removeEventListener("keydown", onDocumentKeydown);
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("scroll", onViewportChange, true);
});
</script>

<template>
  <span
    ref="root"
    class="dd"
    :class="{ compact, disabled: props.disabled }"
  >
    <button
      ref="trigger"
      type="button"
      class="dd-trigger"
      :class="{ open, placeholder: empty }"
      :disabled="props.disabled"
      @click="toggle"
      @keydown.down.prevent="open = true; move(1)"
      @keydown.up.prevent="open = true; move(-1)"
      @keydown.enter.prevent="open = !open"
    >
      <span class="dd-value">{{ empty ? props.placeholder : selectedLabel }}</span>
      <ChevronDown :size="16" class="dd-caret" :class="{ flip: open }" />
    </button>
    <!-- Teleported so the list can never be clipped by, nor grow the scrollable
         area of, the container the trigger lives in (issue #12). -->
    <Teleport to="body">
      <ul
        v-if="open"
        ref="list"
        class="dd-pop"
        :class="{ compact }"
        :style="listStyle"
        role="listbox"
      >
        <li
          v-for="(o, i) in props.options"
          :key="i"
          :class="{ active: o.value === props.modelValue, disabled: o.disabled }"
          role="option"
          :aria-selected="o.value === props.modelValue"
          @mousedown.prevent="pick(o)"
        >
          {{ o.label }}
        </li>
      </ul>
    </Teleport>
  </span>
</template>
