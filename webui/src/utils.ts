export function formatRunTime(value: number | null, locale = "zh-CN", timeZone?: string): string {
  if (!value) return "尚未运行";
  return new Intl.DateTimeFormat(locale, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone
  }).format(value * 1000);
}
