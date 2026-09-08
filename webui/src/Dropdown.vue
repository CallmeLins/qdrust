<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
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

const open = ref(false);
const root = ref<HTMLElement | null>(null);

const selected = computed(() => props.options.find((o) => o.value === props.modelValue));
const selectedLabel = computed(() => selected.value?.label ?? "");
const empty = computed(() => !selected.value);

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
  if (root.value && !root.value.contains(event.target as Node)) open.value = false;
}
function onDocumentKeydown(event: KeyboardEvent) {
  if (!open.value) return;
  if (event.key === "Escape") open.value = false;
}
watch(() => props.modelValue, () => { /* keep open only while choosing */ });

onMounted(() => {
  document.addEventListener("mousedown", onDocumentMouseDown);
  document.addEventListener("keydown", onDocumentKeydown);
});
onBeforeUnmount(() => {
  document.removeEventListener("mousedown", onDocumentMouseDown);
  document.removeEventListener("keydown", onDocumentKeydown);
});
</script>

<template>
  <span
    ref="root"
    class="dd"
    :class="{ compact, disabled: props.disabled }"
  >
    <button
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
    <ul v-if="open" class="dd-pop" role="listbox">
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
  </span>
</template>
