<script setup lang="ts">
import { computed } from "vue";
import { AVATARS } from "./avatars";

const props = withDefaults(
  defineProps<{
    avatarId?: string | null;
    size?: number;
    label?: string;
  }>(),
  {
    avatarId: "avatar-01",
    size: 32,
    label: "",
  },
);

const selected = computed(
  () => AVATARS.find((avatar) => avatar.id === props.avatarId) ?? AVATARS[0],
);
const accessibleLabel = computed(() => props.label || selected.value.label);
</script>

<template>
  <svg
    class="avatar-art"
    :width="size"
    :height="size"
    viewBox="0 0 64 64"
    role="img"
    :aria-label="accessibleLabel"
  >
    <rect width="64" height="64" rx="18" :fill="selected.background" />
    <circle cx="32" cy="32" r="22" :fill="selected.accent" opacity="0.16" />
    <path :d="selected.face" :fill="selected.background" :stroke="selected.ink" stroke-width="1.6" />
    <path :d="selected.detail" fill="none" :stroke="selected.ink" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
</template>

<style scoped>
.avatar-art {
  display: block;
  flex: none;
  border-radius: 30%;
}
</style>
