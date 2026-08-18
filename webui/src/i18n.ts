import { ref } from "vue";

export type Locale = "zh-CN" | "en-US";
export const locale = ref<Locale>((localStorage.getItem("qdrust.locale") as Locale) || "zh-CN");

const messages = {
  "zh-CN": { tasks:"任务", templates:"模板", notes:"记事本", plugins:"插件", notifications:"通知", login:"登录", bootstrap:"初始化管理员", logout:"退出登录" },
  "en-US": { tasks:"Tasks", templates:"Templates", notes:"Notes", plugins:"Plugins", notifications:"Notifications", login:"Sign in", bootstrap:"Initialize admin", logout:"Sign out" }
} as const;

export function t(key: keyof typeof messages["zh-CN"]): string { return messages[locale.value][key]; }
export function toggleLocale(): void { locale.value = locale.value === "zh-CN" ? "en-US" : "zh-CN"; localStorage.setItem("qdrust.locale", locale.value); }
