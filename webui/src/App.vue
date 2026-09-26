<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch, type Ref } from "vue";
import {
  Activity, ArrowLeft, ArrowRight, ArrowUpDown, Bell, CalendarClock, Check, CheckCircle2, ChevronDown, CircleHelp, Copy, Download, FileJson2, FileUp,
  LayoutDashboard, Library as LibraryIcon, Loader2, Mail, Menu, Monitor, Moon, MoreVertical, Pencil, Play, Plus, Power, PowerOff, RefreshCw, Search, Send,
  Settings, Sun, Trash2, Undo2, Upload, Users, X, XCircle, Zap,
} from "@lucide/vue";
import { api, apiPath, errorCode, oidcStartUrl, type AuthConfig, type CreateTask, type Task, type Run, type RunStep, type User, type Template, type Plugin, type NotificationChannel, type NotificationAction, type TemplateSubscription, type PushRequest, type SiteSetting, type LibraryEntry, type LibrarySourceStatus, type TemplateTestResult } from "./api";
import HarEditor from "./HarEditor.vue";
import Dropdown from "./Dropdown.vue";
import Pager from "./Pager.vue";
import { consumeLogoutReturn, emptyHarDoc, formatRunTime, harDocumentFrom, localLoginAvailable, markLogoutReturn, oidcLogoutUrl, orderTemplatesForNewTask, ssoAvailable, ssoOnly } from "./utils";
import { useCursorPager, usePager, usePageSize } from "./pagination";
import { fmt, locale, t, toggleLocale } from "./i18n";

// ---------- toast ----------
type ToastKind = "success" | "error" | "pending";
type Toast = {
  id: number;
  message: string;
  kind: ToastKind;
  /** Optional task name shown above the message */
  title?: string;
  /** Secondary line, e.g. "HTTP 200 · 3/3 steps" */
  meta?: string;
  /** QD-style log line or error text, shown in a scrollable mono block */
  detail?: string;
  /** Enables the "view run history" action when set */
  taskId?: number;
  /** Pending toasts stay until they are updated or dismissed */
  persistent?: boolean;
};
const toasts = ref<Toast[]>([]);
let toastSeq = 0;
function dismissToast(id: number) {
  toasts.value = toasts.value.filter((x) => x.id !== id);
}
function scheduleDismiss(id: number, delay = 4000) {
  setTimeout(() => dismissToast(id), delay);
}
function notify(message: string, kind: ToastKind = "success", extra: Partial<Toast> = {}): number {
  const id = ++toastSeq;
  toasts.value.push({ id, message, kind, ...extra });
  // Long-form toasts (with a log body or an action) need more reading time.
  const delay = extra.detail || extra.meta ? 9000 : 4000;
  if (!extra.persistent) scheduleDismiss(id, delay);
  return id;
}
/** Update a toast in place; finishing a pending toast starts its countdown. */
function updateToast(id: number, patch: Partial<Toast>) {
  const toast = toasts.value.find((x) => x.id === id);
  if (!toast) return;
  Object.assign(toast, patch);
  if (patch.persistent === false || (patch.kind && patch.kind !== "pending")) {
    scheduleDismiss(id, toast.detail || toast.meta ? 9000 : 4000);
  }
}
/** Rows per page, one count per list (see `usePageSize`).
 *
 *  Stored rather than held in memory: a reader who wants 50 rows wants them
 *  on the next visit too. It is a per-browser preference, not a site
 *  setting, so it needs no round trip and no server state.
 *
 *  Each list is set from its own pager footer — that is where a reader
 *  notices the row count, and sending them to a settings page to change it
 *  would lose their place. Keeping one count per list means the footer they
 *  use is the only thing it rearranges. */
const { size: tasksPageSize, setSize: setTasksPageSize } = usePageSize(localStorage, "tasks");
const { size: subsPageSize, setSize: setSubsPageSize } = usePageSize(localStorage, "subscriptions");
const { size: templatesPageSize, setSize: setTemplatesPageSize } = usePageSize(localStorage, "templates");
const { size: publicTemplatesPageSize, setSize: setPublicTemplatesPageSize } = usePageSize(
  localStorage,
  "publicTemplates",
);
const { size: libraryPageSize, setSize: setLibraryPageSize } = usePageSize(localStorage, "library");
const { size: runLogPageSize, setSize: setRunLogPageSize } = usePageSize(localStorage, "runLog");

// ---------- theme (light / dark / system, mirrors collector) ----------
type ThemeMode = "light" | "dark" | "system";
const THEME_STORAGE_KEY = "qdrust-theme-mode";
const THEME_MODES: ThemeMode[] = ["light", "dark", "system"];
const SYSTEM_THEME_MEDIA = "(prefers-color-scheme: dark)";

const storedTheme = localStorage.getItem(THEME_STORAGE_KEY) as ThemeMode | null;
const themeMode = ref<ThemeMode>(THEME_MODES.includes(storedTheme as ThemeMode) ? (storedTheme as ThemeMode) : "system");
let themeMediaQuery: MediaQueryList | null = null;

function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode === "system") return themeMediaQuery?.matches ? "dark" : "light";
  return mode;
}

function applyTheme(mode: ThemeMode) {
  const resolved = resolveTheme(mode);
  document.documentElement.setAttribute("data-theme", resolved);
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", resolved === "dark" ? "#121212" : "#f7f7f7");
}

function setTheme(mode: ThemeMode) {
  themeMode.value = mode;
  localStorage.setItem(THEME_STORAGE_KEY, mode);
  applyTheme(mode);
}

function handleSystemThemeChange() {
  if (themeMode.value === "system") applyTheme("system");
}

onMounted(() => {
  themeMediaQuery = window.matchMedia(SYSTEM_THEME_MEDIA);
  applyTheme(themeMode.value);
  themeMediaQuery.addEventListener("change", handleSystemThemeChange);
});

onUnmounted(() => {
  themeMediaQuery?.removeEventListener("change", handleSystemThemeChange);
});

// ---------- app / auth state ----------
const ready = ref(false);
const authenticated = ref(false);
const currentUser = ref<User | null>(null);
const authMode = ref<"login" | "bootstrap" | "register" | "forgot" | "reset">("login");
const authForm = reactive({ username: "", password: "", email: "", token: "", newPassword: "" });
const authNotice = ref("");
const authPolicy = ref<AuthConfig | null>(null);
const ssoError = ref("");
const verifyResult = ref<"ok" | "fail" | null>(null);
const forgotResult = ref<{ sent: boolean; token?: string } | null>(null);

// External-IdP policy affordances (docs/design/EXTERNAL_IDP_PLAN.md Phase 3).
const showSso = computed(() => ssoAvailable(authPolicy.value));
const ssoProviderName = computed(() => authPolicy.value?.oidc_provider_name?.trim() || "SSO");
const localAuthAvailable = computed(() => localLoginAvailable(authPolicy.value));
// In pure-OIDC mode we do not show the username/password form at all.
const ssoForced = computed(() => ssoOnly(authPolicy.value));
// Local form (login/bootstrap/register) is visible only when local login is
// enabled and we are not on a pure-OIDC deployment.
const showLocalForm = computed(
  () =>
    localAuthAvailable.value &&
    !ssoForced.value &&
    (authMode.value === "login" || authMode.value === "bootstrap" || authMode.value === "register"),
);

const view = ref<"tasks" | "taskRuns" | "templates" | "plugins" | "notifications" | "push" | "admin" | "settings">("tasks");
const menuOpen = ref(false);
const showCreate = ref(false);
const showImport = ref(false);
const showHelp = ref(false);

const currentViewName = computed(() => ({
  tasks: t("tasks"), taskRuns: t("runHistory"), templates: t("templates"), plugins: t("pluginsTitle"),
  notifications: t("notificationsTitle"),
  push: t("pushTitle"), admin: t("adminTitle"), settings: t("settingsTitle"),
}[view.value]));

// ---------- tasks ----------
const tasks = ref<Task[]>([]);
const taskGroups = ref<string[]>([]);
const loading = ref(false);
const search = ref("");
const groupFilter = ref("");
const groupFilterDropdownOptions = computed(() => [
  { value: "", label: t("all") },
  ...taskGroups.value.map((g) => ({ value: g, label: g })),
]);
const selected = reactive(new Set<number>());
const runsByTask = ref<Record<number, Run[]>>({});
const runHistoryTask = ref<Task | null>(null);
/** Task whose overflow (kebab) menu is open on narrow screens; null = none. */
const openRowMenu = ref<number | null>(null);
function toggleRowMenu(id: number) {
  openRowMenu.value = openRowMenu.value === id ? null : id;
}

interface TaskForm {
  id: number | null;
  name: string;
  cron: string;
  scheduleTime: string;
  scheduleDays: string;
  scheduleAdvanced: boolean;
  randomDelay: string;
  disabled: boolean;
  grp: string;
  templateId: number | null;
  timeoutSeconds: string;
  retryCount: string;
  retryInterval: string;
  priority: string;
  timezone: string;
  variables: { name: string; value: string }[];
}
const blankTaskForm = (): TaskForm => ({ id: null, name: "", cron: "", scheduleTime: "08:00:00", scheduleDays: "1", scheduleAdvanced: false, randomDelay: "", disabled: false, grp: "", templateId: null, timeoutSeconds: "", retryCount: "", retryInterval: "", priority: "", timezone: "", variables: [] });
const taskForm = reactive<TaskForm>(blankTaskForm());
const templatesForSelect = computed(() => templates.value);
/** Result of the last "test" run in the task dialog; cleared when the dialog or
 *  the selected template changes. */
const testing = ref(false);
const testResult = ref<TemplateTestResult | null>(null);
/**
 * Dropdown options for the "bind to template" field in the task form.
 *
 * A task's request comes from its template — the scheduler replays the template
 * and ignores any request stored next to it — so the form asks for a template
 * instead of a method/URL pair. For the same reason "（无模板）" is only offered
 * while editing a task that predates that rule and carries a standalone request;
 * hiding it would silently force such a task onto a template it never ran.
 *
 * Templates that no task uses yet come first (issue #25): with a library of
 * hundreds, the unused ones are what someone adding a task is looking for, and
 * they are otherwise scattered through a list too long to scan.
 */
const templateDropdownOptions = computed(() => {
  // "Has a task" comes from two sources on purpose. The server counts tasks per
  // template (`task_count`), which is right as of the read; the client's task
  // list is refreshed after every task change, which closes the gap until the
  // template list is re-read. Taking the union means a template is only offered
  // as unused when neither side knows a task for it.
  const used = new Set<number>(
    tasks.value.map((task) => task.template_id).filter((id): id is number => id != null)
  );
  for (const tpl of templatesForSelect.value) if ((tpl.task_count ?? 0) > 0) used.add(tpl.id);
  const options = orderTemplatesForNewTask(templatesForSelect.value, used).map((tpl) => ({
    value: tpl.id as string | number | null,
    // The suffix is what makes the ordering legible: without it "unused first"
    // is only visible by comparing rows, and a library that is entirely unused
    // looks exactly like one where the rule did nothing.
    label: `${tpl.name}（${tpl.source_format}） · ${
      (tpl.task_count ?? 0) > 0 ? fmt("templateUsedCount", { n: tpl.task_count ?? 0 }) : t("templateUnused")
    }`,
  }));
  if (taskForm.id == null || taskForm.templateId != null) return options;
  return [{ value: null as string | number | null, label: t("noTemplatesToBind") }, ...options];
});

// Full IANA timezone list for the task scheduling select. `Intl.supportedValuesOf`
// is available in modern browsers; fall back to a curated subset where missing
// so the field still offers sensible choices without a hard-coded single option.
const ALL_TIMEZONES: string[] = [
  "Africa/Cairo","Africa/Johannesburg","Africa/Lagos","Africa/Nairobi","America/Argentina/Buenos_Aires",
  "America/Bogota","America/Caracas","America/Chicago","America/Denver","America/Halifax",
  "America/Lima","America/Los_Angeles","America/Mexico_City","America/New_York","America/Phoenix",
  "America/Santiago","America/Sao_Paulo","America/Toronto","America/Vancouver","Asia/Almaty",
  "Asia/Bangkok","Asia/Dhaka","Asia/Dubai","Asia/Ho_Chi_Minh","Asia/Hong_Kong","Asia/Jakarta",
  "Asia/Jerusalem","Asia/Karachi","Asia/Kolkata","Asia/Kuala_Lumpur","Asia/Manila","Asia/Seoul",
  "Asia/Shanghai","Asia/Singapore","Asia/Taipei","Asia/Tokyo","Asia/Yangon","Australia/Adelaide",
  "Australia/Brisbane","Australia/Melbourne","Australia/Perth","Australia/Sydney","Europe/Amsterdam",
  "Europe/Berlin","Europe/Brussels","Europe/Dublin","Europe/Helsinki","Europe/Lisbon","Europe/London",
  "Europe/Madrid","Europe/Moscow","Europe/Oslo","Europe/Paris","Europe/Prague","Europe/Rome",
  "Europe/Stockholm","Europe/Vienna","Europe/Warsaw","Europe/Zurich","Pacific/Auckland",
  "Pacific/Honolulu","Pacific/Port_Moresby","UTC",
];
const timezoneOptions: string[] = (() => {
  if (typeof Intl !== "undefined" && typeof (Intl as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf === "function") {
    try {
      return (Intl as { supportedValuesOf: (k: string) => string[] }).supportedValuesOf("timeZone");
    } catch {
      /* fall through */
    }
  }
  return ALL_TIMEZONES;
})();
/** Dropdown options for the timezone field: an empty "follow server default"
 *  entry plus the full IANA list. */
const timezoneDropdownOptions: { value: string; label: string }[] = [
  { value: "", label: t("timezoneDefault") },
  ...timezoneOptions.map((tz) => ({ value: tz, label: tz })),
];

function variablesToRows(value: unknown): { name: string; value: string }[] {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return Object.entries(value as Record<string, unknown>).map(([name, v]) => ({
      name,
      value: typeof v === "string" ? v : v == null ? "" : JSON.stringify(v),
    }));
  }
  return [];
}
function rowsToVariables(rows: { name: string; value: string }[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const row of rows) if (row.name.trim()) out[row.name.trim()] = row.value;
  return out;
}
function addVariableRow() { taskForm.variables.push({ name: "", value: "" }); }
function removeVariableRow(index: number) { taskForm.variables.splice(index, 1); }

/** 可视化定时 → cron（7 段含秒）：每天/每 N 天在指定时刻执行 */
function buildCron(): string {
  const parts = taskForm.scheduleTime.split(":").map((x) => String(parseInt(x, 10) || 0));
  const h = parts[0] ?? "0";
  const m = parts[1] ?? "0";
  const s = parts[2] ?? "0";
  const days = Math.max(1, Math.min(366, Math.floor(Number(taskForm.scheduleDays) || 1)));
  return `${s} ${m} ${h} */${days} * * *`;
}
/** 尝试把 cron 解析回可视化字段（非该模式生成的表达式返回 null，走高级模式） */
function parseVisualCron(cron: string): { time: string; days: string } | null {
  const match = cron.match(/^(\d+) (\d+) (\d+) \*\/(\d+) \* \* \*$/);
  if (!match) return null;
  const [, s, m, h, d] = match;
  const pad = (x: string) => x.padStart(2, "0");
  return { time: `${pad(h)}:${pad(m)}:${pad(s)}`, days: d };
}

const filteredTasks = computed(() => {
  const term = search.value.trim().toLowerCase();
  return tasks.value.filter((task) => {
    if (groupFilter.value && (task.grp ?? "") !== groupFilter.value) return false;
    if (!term) return true;
    return `${task.name} ${task.url}`.toLowerCase().includes(term);
  });
});
/** One page of the filtered list. The summary tiles below stay account-wide,
 *  so they never report a page's worth as if it were the whole picture. */
const tasksAnchor = ref<HTMLElement | null>(null);
const subscriptionsAnchor = ref<HTMLElement | null>(null);
const templatesAnchor = ref<HTMLElement | null>(null);
const publicTemplatesAnchor = ref<HTMLElement | null>(null);
const libraryAnchor = ref<HTMLElement | null>(null);
const runLogAnchor = ref<HTMLElement | null>(null);

/** Bring the top of a paged form back on screen after a page change (issue
 *  29). Replacing the rows under an unmoved viewport left the scroll position
 *  to the browser's scroll anchoring, which picked a different anchor node
 *  every time — sometimes nothing moved, sometimes the view jumped to the page
 *  top, sometimes it pinned itself to the pager buttons. Anchoring to a marker
 *  above the table instead is deterministic and reads as "the list starts
 *  here" rather than as a jump. `scroll-margin-top` (see `.paged-anchor`)
 *  keeps the sticky app header from covering the target. The footer stays the
 *  one pager: `pager-sticky` pins it to the viewport bottom while its section
 *  is on screen, so it is always within reach without a second pair on top. */
function scrollToPaged(anchor: Ref<HTMLElement | null>): void {
  void nextTick().then(() => anchor.value?.scrollIntoView({ block: "start" }));
}

const {
  page: tasksPage, pages: tasksTotalPages, items: pagedTasks,
  prev: tasksPrevPage, next: tasksNextPage, reset: resetTasksPage,
} = usePager(() => filteredTasks.value, () => tasksPageSize.value, { onPageChange: () => scrollToPaged(tasksAnchor) });
// A new search term or group is a different list rather than a later page of
// the same one, so it starts over.
watch([search, groupFilter], resetTasksPage);

const activeCount = computed(() => tasks.value.filter((task) => !task.disabled).length);
const successCount = computed(() => tasks.value.filter((task) => task.last_status != null && task.last_status < 400).length);
const taskName = (taskId: number) => tasks.value.find((task) => task.id === taskId)?.name ?? `#${taskId}`;
const channelName = (id: number) => channels.value.find((c) => c.id === id)?.name ?? `#${id}`;

async function loadTasks() {
  loading.value = true;
  try {
    const [list, groups] = await Promise.all([api.tasks(), api.taskGroups().catch(() => [])]);
    tasks.value = list;
    taskGroups.value = groups;
    // drop selections pointing at removed tasks
    for (const id of [...selected]) if (!tasks.value.some((x) => x.id === id)) selected.delete(id);
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    loading.value = false;
  }
  // A task change moves templates in and out of the "unused" bucket, and both
  // the templates table and the create-task dropdown read that count. Refresh
  // it whenever the template list is on screen; before that there is nothing to
  // correct, and a task-page visit must not fetch templates at all.
  if (templates.value.length) void refreshTemplates();
}
async function refreshTaskStatuses() {
  if (!tasks.value.length) return;
  try {
    const latest = await api.tasks();
    const byId = new Map(latest.map((task) => [task.id, task]));
    for (const task of tasks.value) {
      const fresh = byId.get(task.id);
      if (fresh) {
        task.disabled = fresh.disabled;
        task.last_status = fresh.last_status;
        task.last_run_at = fresh.last_run_at;
      }
    }
  } catch {
    // Background status refresh is best-effort; keep the current table intact.
  }
}

async function submitTask() {
  // A task runs a template; the request fields are gone from this form because
  // the scheduler replays the template anyway. Only a task that predates the
  // rule (and therefore still carries a standalone request) may be saved
  // without one — the server keeps its stored URL untouched.
  if (taskForm.templateId == null && taskForm.id == null) {
    return void notify(t("templateRequired"), "error");
  }
  if (taskForm.timeoutSeconds && !(Number(taskForm.timeoutSeconds) > 0)) return void notify("timeout must be a positive number", "error");
  if (taskForm.retryInterval && !(Number(taskForm.retryInterval) > 0)) return void notify("retry interval must be a positive number", "error");
  if (taskForm.priority && Number.isNaN(Number(taskForm.priority))) return void notify("priority must be a number", "error");
  const cron = taskForm.scheduleAdvanced ? taskForm.cron : buildCron();
  const payload: CreateTask = {
    name: taskForm.name,
    cron,
    disabled: taskForm.disabled,
    grp: taskForm.grp || null,
    template_id: taskForm.templateId,
    timeout_seconds: taskForm.timeoutSeconds ? Number(taskForm.timeoutSeconds) : null,
    retry_count: taskForm.retryCount ? Number(taskForm.retryCount) : null,
    retry_interval_seconds: taskForm.retryInterval ? Number(taskForm.retryInterval) : null,
    priority: taskForm.priority ? Number(taskForm.priority) : null,
    timezone: taskForm.timezone || null,
    random_delay_max_seconds: Number(taskForm.randomDelay) > 0 ? Math.floor(Number(taskForm.randomDelay)) : null,
    variables: taskForm.variables.length ? rowsToVariables(taskForm.variables) : null,
  };
  try {
    if (taskForm.id != null) {
      await api.updateTask(taskForm.id, { ...payload });
      notify(t("taskUpdated"));
    } else {
      await api.createTask(payload);
      notify(t("taskCreated"));
    }
    Object.assign(taskForm, blankTaskForm());
    showCreate.value = false;
    await loadTasks();
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  }
}

function openCreateTask() {
  Object.assign(taskForm, blankTaskForm());
  testResult.value = null;
  showCreate.value = true;
  // Always re-read, not only when the list is empty: the dropdown's order and
  // its "unused" suffix are computed from these rows, so a template bound since
  // the last read would otherwise still be offered as unused.
  void refreshTemplates();
}function openEditTask(task: Task) {
  const visual = parseVisualCron(task.cron);
  testResult.value = null;
  Object.assign(taskForm, {
    id: task.id,
    name: task.name,
    cron: task.cron,
    scheduleTime: visual?.time ?? "08:00:00",
    scheduleDays: visual?.days ?? "1",
    scheduleAdvanced: !visual,
    randomDelay: task.random_delay_max_seconds != null && task.random_delay_max_seconds > 0 ? String(task.random_delay_max_seconds) : "",
    disabled: task.disabled,
    grp: task.grp ?? "",
    templateId: task.template_id ?? null,
    timeoutSeconds: task.timeout_seconds != null ? String(task.timeout_seconds) : "",
    retryCount: task.retry_count != null ? String(task.retry_count) : "",
    retryInterval: task.retry_interval_seconds != null ? String(task.retry_interval_seconds) : "",
    priority: task.priority != null ? String(task.priority) : "",
    timezone: task.timezone ?? "",
    variables: variablesToRows(task.variables),
  });
  showCreate.value = true;
}

async function toggleTask(task: Task) {
  try {
    await api.updateTask(task.id, { disabled: !task.disabled });
    await loadTasks();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
const TERMINAL_RUN_STATUSES = ["succeeded", "failed", "cancelled"];

/** Poll the task's runs until the given one reaches a terminal status. */
async function waitForRun(taskId: number, runId: number): Promise<Run | null> {
  const deadline = Date.now() + 5 * 60 * 1000;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 1000));
    const runs = await api.taskRuns(taskId);
    const run = runs.find((x) => x.id === runId);
    if (run && TERMINAL_RUN_STATUSES.includes(run.status)) return run;
  }
  return null;
}

async function runNow(task: Task) {
  // Keep a pending toast open and fill it with the result once the run settles.
  const toastId = notify(t("runNowRunning"), "pending", {
    title: task.name,
    persistent: true,
  });
  try {
    const queued = await api.runTask(task.id);
    const run = await waitForRun(task.id, queued.id);
    if (!run) {
      updateToast(toastId, { message: t("runNowTimeout"), taskId: task.id, persistent: false });
      return;
    }
    let steps: RunStep[] = [];
    try { steps = await api.runSteps(run.id); } catch { steps = []; }
    const done = steps.filter((s) => s.status === "succeeded").length;
    const meta = [
      run.http_status != null ? `HTTP ${run.http_status}` : "",
      steps.length ? fmt("runStepsCount", { done, total: steps.length }) : "",
    ].filter(Boolean).join(" · ");
    const log = runLogText(run);
    updateToast(toastId, {
      kind: run.status === "succeeded" ? "success" : "error",
      message: run.status === "succeeded" ? t("runNowSucceeded") : runStatusLabel(run.status),
      meta: meta || undefined,
      detail: log && log !== "–" ? log : undefined,
      taskId: task.id,
      persistent: false,
    });
    await loadTasks();
    if (runHistoryTask.value?.id === task.id) await loadTaskRuns(task.id);
  } catch (cause) {
    updateToast(toastId, {
      kind: "error",
      message: cause instanceof Error ? cause.message : t("genericError"),
      persistent: false,
    });
  }
}
async function removeTask(task: Task) {
  if (!window.confirm(fmt("deleteTaskConfirm", { name: task.name }))) return;
  try {
    await api.deleteTask(task.id);
    notify(t("taskDeleted"));
    await loadTasks();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function batchTasks(action: "enable" | "disable" | "delete" | "run") {
  if (selected.size === 0) return;
  if (action === "delete" && !window.confirm(fmt("selectedCount", { n: selected.size }) + " · " + t("confirmDelete"))) return;
  try {
    const result = await api.batchTasks([...selected], action);
    notify(`${fmt("selectedCount", { n: result.updated })}`);
    selected.clear();
    await loadTasks();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function toggleSelect(id: number) { selected.has(id) ? selected.delete(id) : selected.add(id); }
/** Select-all covers the page on screen, not the whole filtered set: the rows
 *  it can reach are exactly the ones the reader can watch it tick, and no row
 *  hidden on another page is swept into a batch action. */
function selectAllVisible() {
  const ids = pagedTasks.value.map((x) => x.id);
  const allSelected = ids.length > 0 && ids.every((id) => selected.has(id));
  for (const id of ids) allSelected ? selected.delete(id) : selected.add(id);
}

// ---------- runs / steps ----------
async function loadTaskRuns(taskId: number) {
  try { runsByTask.value[taskId] = await api.taskRuns(taskId); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function openRunHistory(task: Task) {
  runHistoryTask.value = task;
  view.value = "taskRuns";
  await loadTaskRuns(task.id);
}
function backToTasks() {
  runHistoryTask.value = null;
  view.value = "tasks";
}
/** "View run history" action on a run-result toast. */
function openRunHistoryFromToast(toast: Toast) {
  const task = tasks.value.find((x) => x.id === toast.taskId);
  dismissToast(toast.id);
  if (task) void openRunHistory(task);
}
/** Jump from an aggregated-log row to that task's own history. */
function openRunHistoryById(taskId: number) {
  const task = tasks.value.find((x) => x.id === taskId);
  if (task) void openRunHistory(task);
}
/** The task-name cell in the run-log modal leaves it for the history page. */
function openRunLogHistory(taskId: number) {
  showRunLog.value = false;
  openRunHistoryById(taskId);
}
async function cancelRun(run: Run) {
  try {
    await api.cancelRun(run.id);
    await refreshRunViews();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeRun(run: Run) {
  if (!window.confirm(t("confirmDeleteRun"))) return;
  try {
    await api.deleteRun(run.id);
    await refreshRunViews();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
/** Reload whichever run list is on screen; both the task-run page and the
 *  run-log panel cancel and delete runs. */
async function refreshRunViews() {
  if (view.value === "taskRuns" && runHistoryTask.value != null) await loadTaskRuns(runHistoryTask.value.id);
  else if (showRunLog.value) await loadRunLog();
}
async function clearTaskRuns() {
  const task = runHistoryTask.value;
  if (task == null) return;
  if (!window.confirm(fmt("confirmClearRuns", { name: task.name }))) return;
  try {
    await api.deleteTaskRuns(task.id);
    await loadTaskRuns(task.id);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function runStatusLabel(status: string): string {
  const key = ({ succeeded: "runStSucceeded", failed: "runStFailed", running: "runStRunning", leased: "runStRunning", pending: "runStPending", cancelled: "runStCancelled" } as Record<string, Parameters<typeof t>[0]>)[status];
  return key ? t(key) : status;
}
function taskStatusLabel(task: Task): string {
  if (task.disabled) return "禁用";
  if (task.last_status == null) return "正常";
  return task.last_status < 400 ? "正常" : "失败";
}
/** QD 式日志列：成功显示 __log__ 摘要，失败显示错误详情 */
function runLogText(run: Run): string {
  if (run.log) {
    // HAR logs may contain escaped newline sequences; decode them for display only.
    return run.log.replace(/\\r\\n/g, "\n").replace(/\\n/g, "\n").replace(/\\r/g, "\r");
  }
  if (run.error) return run.error;
  if (run.http_status) return `HTTP ${run.http_status}`;
  return "–";
}
function runStatusClass(status: string): string {
  if (status === "succeeded") return "run-ok";
  if (status === "failed") return "run-bad";
  if (status === "cancelled") return "run-cancelled";
  return "run-active";
}

// ---------- aggregated run log ----------
// "Are all 30 tasks still green?" used to mean opening 30 run-history pages.
// One filtered list answers it instead; the per-task page stays for depth.
// Rows per fetch follow the app-wide page size (see pagination.ts): the run
// log is a list like any other, and it holds the longest rows in the app.
const allRuns = ref<Run[]>([]);
const runLogStatus = ref<"" | "succeeded" | "failed">("");
const runLogTaskId = ref(0);
const runLogLoading = ref(false);
/** Whether the run-log modal is open. */
const showRunLog = ref(false);
const runLogNextCursor = ref<number | null>(null);
const runLogHasMore = ref(false);
/** Cursor paging as prev/next (see `useCursorPager`): the stack's last entry is
 *  the `beforeId` the current page was fetched with, so "previous page" is a pop
 *  and "next" a push — no page numbers to invent on a cursor API. */
const {
  cursor: runLogCursor, pageNo: runLogPageNo, canGoBack: runLogCanGoBack,
  advance: runLogAdvance, back: runLogBack, reset: resetRunLogPaging,
} = useCursorPager();
const runLogStatusOptions = computed(() => [
  { value: "" as const, label: t("runLogAll") },
  { value: "succeeded" as const, label: t("runStSucceeded") },
  { value: "failed" as const, label: t("runStFailed") },
]);
const runLogTaskDropdownOptions = computed(() => [
  { value: 0, label: t("runLogAllTasks") },
  ...tasks.value.map((task) => ({ value: task.id, label: task.name })),
]);
function runTaskName(taskId: number): string {
  return tasks.value.find((task) => task.id === taskId)?.name ?? `#${taskId}`;
}
function taskTimezone(taskId: number): string | undefined {
  return tasks.value.find((task) => task.id === taskId)?.timezone ?? undefined;
}
/** Open the run-log modal and fetch its first page. */
async function openRunLog() {
  showRunLog.value = true;
  resetRunLogPaging();
  await loadRunLog();
}
/** Fetch the page the cursor stack currently points at. */
async function loadRunLog() {
  runLogLoading.value = true;
  try {
    const page = await api.runs({
      status: runLogStatus.value || undefined,
      taskId: runLogTaskId.value || undefined,
      beforeId: runLogCursor.value,
      limit: runLogPageSize.value,
    });
    allRuns.value = page.items;
    runLogHasMore.value = page.has_more;
    runLogNextCursor.value = page.next_cursor;
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    runLogLoading.value = false;
  }
}
async function nextRunLogPage() {
  if (!runLogHasMore.value || runLogLoading.value) return;
  runLogAdvance(runLogNextCursor.value);
  await loadRunLog();
  scrollToPaged(runLogAnchor);
}
async function prevRunLogPage() {
  if (!runLogCanGoBack.value || runLogLoading.value) return;
  runLogBack();
  await loadRunLog();
  scrollToPaged(runLogAnchor);
}
/** The status is set on its ref and nothing else: the watch below is what
 *  refetches, so this stays a setter and there is one reload path, not two. */
function setRunLogStatus(status: "" | "succeeded" | "failed") {
  runLogStatus.value = status;
}
/** Anything that changes *which* runs match — the status, the task, the row
 *  count — starts the paging over.
 *
 *  The cursors were cut for the previous result set, so keeping them asks for
 *  "the page after X" of a list that no longer exists: the filtered runs sit
 *  mostly above the old cursor, and the modal comes back empty. That is what
 *  "filter by task does nothing" looked like — the filter worked, the cursor
 *  was answering for a different list.
 *
 *  A watch rather than a step in each setter — the row count's reset used to be
 *  one — because it covers the dropdown's keyboard path (which only updates the
 *  model), and any control added later inherits it without having to remember.
 *  The guard skips the fetch while the modal is shut: opening it fetches anyway. */
watch([runLogStatus, runLogTaskId, runLogPageSize], () => {
  resetRunLogPaging();
  if (showRunLog.value) void loadRunLog();
});
// A new row count re-chunks the list, so it anchors like a page turn does;
// filters intentionally do not — they change which runs match, not where the
// reader was in the list.
watch(runLogPageSize, () => scrollToPaged(runLogAnchor));

// ---------- templates ----------
const templates = ref<Template[]>([]);
const publicTemplates = ref<Template[]>([]);
const templateSearch = ref("");
/** "Unused only" narrows the table to the templates no task is bound to — the
 *  list the create-task dropdown floats anyway, asked as a filter so a large
 *  library can be audited instead of guessed at. */
const unusedOnly = ref(false);
const editingTemplateId = ref<number | null>(null);
const importForm = reactive({ name: "", description: "" });
const harEditorDoc = ref<object | null>(null);
/** The "published by others" list is secondary to the two lists the page is
 *  for, so it stays folded away until asked for. */
const showPublicTemplates = ref(false);

type SortDir = "asc" | "desc";
/** Sort state per table. Both lists used to arrive in source order — the
 *  templates by creation id, the library in manifest order — so the newest
 *  thing was never anywhere in particular. Defaults follow what these lists are
 *  scanned for: what changed last, first. */
const templateSort = reactive<{ key: "name" | "grp" | "variables" | "tasks" | "updated_at"; dir: SortDir }>({ key: "updated_at", dir: "desc" });
const publicSort = reactive<{ key: "name" | "updated_at"; dir: SortDir }>({ key: "updated_at", dir: "desc" });
const librarySort = reactive<{ key: "name" | "author" | "source" | "version" | "date"; dir: SortDir }>({ key: "date", dir: "desc" });

/** Flip the sort column, or reverse it when the same header is clicked again.
 *  Text columns open ascending (A→Z is what a name column means to a reader),
 *  date and count columns descending. */
function toggleSort<T extends string>(sort: { key: T; dir: SortDir }, key: T, text = false) {
  if (sort.key === key) {
    sort.dir = sort.dir === "asc" ? "desc" : "asc";
  } else {
    sort.key = key;
    sort.dir = text ? "asc" : "desc";
  }
}
function sortAria(sort: { key: string; dir: SortDir }, key: string): "ascending" | "descending" | "none" {
  if (sort.key !== key) return "none";
  return sort.dir === "asc" ? "ascending" : "descending";
}
function sortDir(sort: { key: string; dir: SortDir }, key: string): SortDir | null {
  return sort.key === key ? sort.dir : null;
}
/** Sort a copy by the clicked header. Numbers compare as numbers (so 10 follows
 *  9 rather than 1), everything else through `localeCompare` with `numeric` on,
 *  which is what keeps QD's `20230112`-style versions and `v9`/`v10` names in a
 *  believable order. Rows with no value sort last in both directions. */
function sortRows<T>(
  rows: T[],
  sort: { key: string; dir: SortDir },
  pick: (row: T, key: string) => string | number | null,
): T[] {
  const factor = sort.dir === "asc" ? 1 : -1;
  return [...rows].sort((a, b) => {
    const left = pick(a, sort.key);
    const right = pick(b, sort.key);
    if (left == null || right == null) {
      if (left == null && right == null) return 0;
      return left == null ? 1 : -1;
    }
    if (typeof left === "number" && typeof right === "number") return (left - right) * factor;
    return String(left).localeCompare(String(right), undefined, { numeric: true, sensitivity: "base" }) * factor;
  });
}

const filteredTemplates = computed(() => {
  const term = templateSearch.value.trim().toLowerCase();
  const matched = templates.value.filter((x) => {
    if (unusedOnly.value && (x.task_count ?? 0) > 0) return false;
    if (!term) return true;
    return `${x.name} ${x.description ?? ""} ${x.grp ?? ""}`.toLowerCase().includes(term);
  });
  return sortRows(matched, templateSort, (item, key) => {
    switch (key) {
      case "name": return item.name;
      case "grp": return item.grp ?? "";
      case "variables": return item.variables?.length ?? 0;
      case "tasks": return item.task_count ?? 0;
      default: return item.updated_at;
    }
  });
});
const sortedPublicTemplates = computed(() =>
  sortRows(publicTemplates.value, publicSort, (item, key) => (key === "name" ? item.name : item.updated_at)),
);
/** Both template lists are paged like everything else. This is the list that
 *  most often runs into the hundreds, and the one whose rows are tallest —
 *  name, description and four buttons each. */
const {
  page: templatesPage, pages: templatesTotalPages, items: pagedTemplates,
  prev: templatesPrevPage, next: templatesNextPage, reset: resetTemplatesPage,
} = usePager(() => filteredTemplates.value, () => templatesPageSize.value, { onPageChange: () => scrollToPaged(templatesAnchor) });
const {
  page: publicTemplatesPage, pages: publicTemplatesTotalPages, items: pagedPublicTemplates,
  prev: publicTemplatesPrevPage, next: publicTemplatesNextPage,
} = usePager(() => sortedPublicTemplates.value, () => publicTemplatesPageSize.value, { onPageChange: () => scrollToPaged(publicTemplatesAnchor) });
watch([templateSearch, unusedOnly], resetTemplatesPage);

/** Headers of the "my templates" table. `text` marks the columns whose natural
 *  first order is A→Z rather than newest-first. */
const templateColumns = computed(() => [
  { key: "name" as const, label: t("name"), text: true },
  { key: "grp" as const, label: t("group"), text: true },
  { key: "variables" as const, label: t("variables"), text: false },
  { key: "tasks" as const, label: t("templateTaskCount"), text: false },
  { key: "updated_at" as const, label: t("updatedAt"), text: false },
]);
/** The public list is every published template, so it doubles as the publish-state index. */
const publishedTemplateIds = computed(() => new Set(publicTemplates.value.map((x) => x.id)));
/** Re-read the owner's templates, keeping the current list if the read fails.
 *
 *  Both the templates table and the create-task dropdown derive their "unused"
 *  state from these rows, so this is called after a task change and whenever
 *  the new-task dialog opens — not only when the list is empty. */
async function refreshTemplates() {
  try {
    templates.value = await api.allTemplates();
  } catch (cause) {
    // A failed refresh must not blank a list that is already on screen; report
    // it only when there is nothing to show instead.
    if (!templates.value.length) notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  }
}
async function openTemplates() {
  view.value = "templates";
  try {
    const [mine, pub, subs] = await Promise.all([
      api.allTemplates(),
      api.publicTemplates(),
      api.subscriptions(),
    ]);
    templates.value = mine;
    publicTemplates.value = pub;
    subscriptions.value = subs;
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
  // The library half of the page is what makes this page the entry point, so it
  // fills itself in on arrival — but only once: a later visit (or the refresh
  // after a publish) reuses the catalogue instead of hitting GitHub again.
  await ensureLibraryLoaded();
}
function onTemplatePicked() {
  const tmpl = templatesForSelect.value.find((x) => x.id === taskForm.templateId);
  if (!tmpl) return;
  if (!taskForm.name) taskForm.name = tmpl.name;
  // QD 式变量联动：变量清单由服务端按 QD 的 HARSave.get_variables 语义算出
  // （过滤器和函数名不算变量，且只有被 extract_variables 提取之后的引用才不算输入），
  // 这里只负责生成填值行，并保留用户已输入的同名值。
  // A different template invalidates any earlier test result.
  testResult.value = null;
  const names = tmpl.variables ?? [];
  if (names.length) {
    const existing = new Map(taskForm.variables.filter((row) => row.name.trim()).map((row) => [row.name.trim(), row.value]));
    // A declared default (`{{x|default("...")}}` in the HAR, or the native
    // variables map) seeds the row, so `{{_proxy|default("")}}` and the like do
    // not have to be typed in by hand. A value the user already entered wins.
    const defaults = tmpl.variable_defaults ?? {};
    taskForm.variables = names.map((name) => ({ name, value: existing.get(name) ?? defaults[name] ?? "" }));
    notify(fmt("templateVarsFound", { n: names.length }));
  }
}

/** Run the selected template with the variables currently in the form.
 *
 *  Goes through the server's normal execution path, so the SSRF guard, the
 *  request/loop limits and the per-run timeout all apply — a test cannot be
 *  more permissive than the task it stands in for. Nothing is persisted. */
async function runTemplateTest() {
  if (taskForm.templateId == null || testing.value) return;
  testing.value = true;
  testResult.value = null;
  try {
    const variables = Object.fromEntries(
      taskForm.variables.filter((row) => row.name.trim()).map((row) => [row.name.trim(), row.value])
    );
    testResult.value = await api.testTemplate(taskForm.templateId, variables);
    notify(t("templateTestDone"));
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    testing.value = false;
  }
}
// This used to be a `watch` on `taskForm.templateId` that fired on a *change*.
// It missed the ordinary retry: cancelling leaves the form as it was, so the
// next "new task from this template" resets the id to null and sets it back to
// the same value within one tick — the watcher sees X -> X, never fires, and the
// variable rows silently stay empty. The two places a template is actually
// chosen call this directly instead.
async function publishTemplate(id: number) {
  try { await api.publishTemplate(id); notify(t("publishDone")); await openTemplates(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function unpublishTemplate(id: number) {
  try { await api.unpublishTemplate(id); notify(t("unpublishDone")); await openTemplates(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function copyTemplate(id: number) {
  try { await api.copyPublicTemplate(id); notify(t("importDone")); await openTemplates(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
/** Delete one of the caller's templates.
 *
 *  `tasks.template_id` is declared `ON DELETE RESTRICT`, so a template that
 *  still has tasks cannot go — the server refuses it. The table already renders
 *  `task_count`, so the ordinary case is settled here, before any request, with
 *  a sentence naming how many tasks are in the way and what to do about them.
 *
 *  That count is only as fresh as the last list read, which is why this is a
 *  courtesy rather than the guard: bind a task in another tab and the count is
 *  stale. The server re-counts and answers 409 `template_in_use`, handled
 *  below, so the race still ends in a sentence instead of a 500.
 *
 *  Kept clickable on purpose rather than disabled while `task_count > 0`. A
 *  greyed-out button cannot say what is holding the template or what to do
 *  next, and a stale zero would leave the user pressing a dead control with no
 *  way to learn why. */
async function removeTemplate(id: number, name: string, taskCount: number) {
  if (taskCount > 0) { notify(fmt("templateInUse", { name, n: taskCount }), "error", { meta: t("templateInUseHint") }); return; }
  if (!window.confirm(fmt("deleteTemplateConfirm", { name }))) return;
  try { await api.deleteTemplate(id); await openTemplates(); }
  catch (cause) {
    // The server refused because a task appeared after this list was read. Its
    // 409 does carry the fresh count, but a number the user can check against
    // the table is worth more than one in a toast: re-read the list and let the
    // row report it, so the next attempt is specific.
    if (errorCode(cause) === "template_in_use") {
      notify(fmt("templateInUseStale", { name }), "error", { meta: t("templateInUseHint") });
      await openTemplates();
      return;
    }
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  }
}
function openImportModal(template?: Template) {
  editingTemplateId.value = template?.id ?? null;
  // Every other way into this editor comes through here, so clearing the
  // library link here is what keeps them all writing to the local templates.
  libraryPreview.value = null;
  Object.assign(importForm, { name: template?.name ?? "", description: template?.description ?? "" });
  // A stored template is not necessarily a HAR document: one imported from a
  // subscription keeps whatever shape the source published, which for the QD
  // libraries is a bare request array. Normalise on the way in, or the editor
  // opens empty on a template that runs perfectly well.
  harEditorDoc.value = harDocumentFrom(template?.qd_har) ?? emptyHarDoc();
  showImport.value = true;
}

/** Close the editor and drop everything it was holding, so the next open starts
 *  from nothing rather than from whatever was being previewed. */
function closeHarEditor() {
  showImport.value = false;
  editingTemplateId.value = null;
  libraryPreview.value = null;
  Object.assign(importForm, { name: "", description: "" });
  harEditorDoc.value = null;
}

/** "New task" on a templates-page row: the create dialog opens with this
 *  template already chosen, which also fills in the task name and one variable
 *  row per variable the template expects (see `onTemplatePicked`). Picking a
 *  template and filling its variables is what a QD user actually came to do, so
 *  it is one click from the list rather than a detour through the task page. */
function createTaskFromTemplate(item: Template) {
  openCreateTask();
  taskForm.templateId = item.id;
  onTemplatePicked();
}

/** 导入本地模板文件：兼容标准 HAR 与 QD 导出的请求数组两种格式 */
async function onHarFile(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = ""; // 允许重复选择同一文件
  if (!file) return;
  let parsed: unknown;
  try {
    parsed = JSON.parse(await file.text());
  } catch {
    notify(t("harJsonError"), "error");
    return;
  }
  const doc = harDocumentFrom(parsed);
  if (!doc) {
    notify(t("harJsonError"), "error");
    return;
  }
  harEditorDoc.value = doc;
  if (!importForm.name.trim()) importForm.name = file.name.replace(/\.(har|json)$/i, "");
  const entries = (doc as Record<string, any>).log?.entries;
  notify(fmt("harLoaded", { name: file.name, n: Array.isArray(entries) ? entries.length : 0 }));
}
async function saveHar(doc: object) {
  if (!importForm.name.trim()) { notify(t("templateName"), "error"); return; }
  const preview = libraryPreview.value;
  try {
    if (preview) {
      // Saving from a library preview writes back through `apply`, which also
      // records the provenance row: that is what marks the entry imported and
      // lets a later upstream revision show up as an update. `templateId` is
      // the local copy being refreshed, absent the first time.
      const outcome = await api.applySubscriptionTemplate(preview.subscriptionId, {
        entry: preview.entry,
        template_id: preview.templateId,
        name: importForm.name,
        description: importForm.description.trim() || null,
        har: doc,
      });
      notify(outcome.updated ? fmt("libraryUpdatedCount", { n: 1 }) : fmt("libraryImportedCount", { n: 1 }));
    } else if (editingTemplateId.value) {
      await api.updateQdHar(editingTemplateId.value, importForm.name, importForm.description, doc);
      notify(t("importDone"));
    } else {
      await api.importQdHar(importForm.name, importForm.description, doc);
      notify(t("importDone"));
    }
    closeHarEditor();
    await openTemplates();
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  }
}

// ---------- plugins ----------
const plugins = ref<Plugin[]>([]);
const pluginForm = reactive({ name: "", command: "" });
const invokeForm = reactive({ action: "run", query: "{}" });
const pluginResult = ref("");
async function openPlugins() {
  view.value = "plugins";
  try { plugins.value = await api.plugins(); } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function savePlugin() {
  try {
    await api.createPlugin(pluginForm.name, pluginForm.command);
    Object.assign(pluginForm, { name: "", command: "" });
    await openPlugins();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function togglePlugin(plugin: Plugin) {
  try { await api.updatePlugin(plugin.id, !plugin.enabled); await openPlugins(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removePlugin(id: number, name: string) {
  if (!window.confirm(fmt("deletePluginConfirm", { name }))) return;
  try { await api.deletePlugin(id); await openPlugins(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function invokePlugin(plugin: Plugin) {
  pluginResult.value = "";
  try {
    const query = JSON.parse(invokeForm.query) as Record<string, string>;
    pluginResult.value = JSON.stringify(await api.invokePlugin(plugin.id, invokeForm.action, query), null, 2);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- notifications ----------
const channels = ref<NotificationChannel[]>([]);
const actions = ref<NotificationAction[]>([]);
/** One form serves both creating and editing a channel; a non-null `id` is
 *  what turns it into an edit, the same way the task form works. */
function blankChannelForm() {
  return {
    id: null as number | null,
    name: "",
    kind: "webhook" as NotificationChannel["kind"],
    url: "", sound: "", group: "", sendkey: "", tgToken: "", tgChatId: "", tgHost: "",
    dingToken: "", wxToken: "", wxUid: "", spt: "", corpId: "", agentId: "",
    secret: "", toUser: "", wecomKey: "", to: "", subject: "", customMethod: "POST", customHeaders: "{}", customBody: "",
  };
}
const channelForm = reactive(blankChannelForm());
/** The editor sits above the list, so opening it has to bring it into view. */
const channelFormEl = ref<HTMLElement | null>(null);
const actionForm = reactive({ taskIds: [] as number[], channelId: 0, event: "failure", failureThreshold: "1", automaticOnly: false, titleTemplate: "", bodyTemplate: "" });
/** Inline editor for one saved binding: the row swaps itself for a form. */
const editingActionId = ref<number | null>(null);
const actionEdit = reactive({ channelId: 0, event: "failure", failureThreshold: "1", automaticOnly: false, titleTemplate: "", bodyTemplate: "" });
// One checkbox list replaces the old single-task <select> + ctrl-click multiple
// <select>: the ticks pick which tasks a new binding is written to, while the
// list below reads the whole account, so a rule you just saved is never hidden
// behind an empty selection.
const actionTaskFilter = ref("");
const actionTasks = computed(() => {
  const needle = actionTaskFilter.value.trim().toLowerCase();
  if (!needle) return tasks.value;
  return tasks.value.filter((task) => {
    const url = task.url ?? "";
    return task.name.toLowerCase().includes(needle) || url.toLowerCase().includes(needle);
  });
});
const allActionTasksChecked = computed(() => actionTasks.value.length > 0 && actionTasks.value.every((task) => actionForm.taskIds.includes(task.id)));
function toggleActionTask(id: number) {
  const at = actionForm.taskIds.indexOf(id);
  if (at >= 0) actionForm.taskIds.splice(at, 1); else actionForm.taskIds.push(id);
}
function toggleAllActionTasks() {
  const ids = actionTasks.value.map((task) => task.id);
  actionForm.taskIds = allActionTasksChecked.value
    ? actionForm.taskIds.filter((id) => !ids.includes(id))
    : [...new Set([...actionForm.taskIds, ...ids])];
}
const actionChannelDropdownOptions = computed(() => [
  { value: 0, label: t("chooseChannel"), disabled: true },
  ...channels.value.map((ch) => ({ value: ch.id, label: ch.name })),
]);
const eventDropdownOptions: { value: string; label: string }[] = [
  { value: "success", label: t("eventSuccess") },
  { value: "failure", label: t("eventFailure") },
  { value: "always", label: t("eventAlways") },
];
const channelKindLabels: Record<NotificationChannel["kind"], string> = {
  webhook: t("webhookKind"), email: t("emailKind"), bark: t("barkKind"),
  serverchan: t("serverchanKind"), telegram: t("telegramKind"), dingtalk: t("dingtalkKind"),
  wxpusher: t("wxpusherKind"), wxpusher_spt: t("wxpusherSptKind"),
  wecom_app: t("wecomAppKind"), wecom_webhook: t("wecomWebhookKind"), custom_http: t("customHttpKind"),
};
const channelKindDropdownOptions: { value: string; label: string }[] = Object.entries(channelKindLabels).map(([value, label]) => ({ value, label }));
function channelKindLabel(kind: string): string {
  return channelKindLabels[kind as NotificationChannel["kind"]] ?? kind;
}
function buildChannelConfig(): Record<string, unknown> {
  const f = channelForm;
  const trim = (value: string) => value.trim();
  const optional = (value: string) => trim(value) || undefined;
  switch (f.kind) {
    case "webhook": return { url: trim(f.url) };
    case "custom_http": {
      let headers: Record<string, string> = {};
      try { headers = JSON.parse(f.customHeaders || "{}"); } catch { throw new Error("custom HTTP headers must be valid JSON"); }
      return { url: trim(f.url), method: trim(f.customMethod).toUpperCase() || "POST", headers, body: f.customBody };
    }
    case "email": return { to: trim(f.to), ...(optional(f.subject) ? { subject: trim(f.subject) } : {}) };
    case "bark": return { url: trim(f.url), ...(optional(f.sound) ? { sound: trim(f.sound) } : {}), ...(optional(f.group) ? { group: trim(f.group) } : {}) };
    case "serverchan": return { sendkey: trim(f.sendkey) };
    case "telegram": return { token: trim(f.tgToken), chat_id: trim(f.tgChatId), ...(optional(f.tgHost) ? { host: trim(f.tgHost) } : {}) };
    case "dingtalk": return { access_token: trim(f.dingToken) };
    case "wxpusher": return { app_token: trim(f.wxToken), uid: trim(f.wxUid) };
    case "wxpusher_spt": return { spt: trim(f.spt) };
    case "wecom_app": return { corpid: trim(f.corpId), agentid: trim(f.agentId), secret: trim(f.secret), ...(optional(f.toUser) ? { to_user: trim(f.toUser) } : {}) };
    case "wecom_webhook": return { key: trim(f.wecomKey) };
  }
}
async function openNotifications() {
  view.value = "notifications";
  try {
    const [ch, list] = await Promise.all([api.notificationChannels(), api.tasks()]);
    channels.value = ch;
    tasks.value = list;
    // Drop tasks that vanished while the page was open, then refresh the
    // bindings of whatever is still ticked.
    const known = new Set(list.map((task) => task.id));
    actionForm.taskIds = actionForm.taskIds.filter((id) => known.has(id));
    await loadActions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
/** Jump to the notification page with the given tasks already ticked. */
function openNotificationsFor(taskIds: number[]) {
  actionForm.taskIds = [...taskIds];
  return openNotifications();
}
/** Reverse of `buildChannelConfig`, so the editor shows what is stored. */
function openEditChannel(channel: NotificationChannel) {
  const config = (channel.config ?? {}) as Record<string, unknown>;
  const text = (value: unknown) => (value == null ? "" : String(value));
  const next = blankChannelForm();
  next.id = channel.id;
  next.name = channel.name;
  next.kind = channel.kind;
  switch (channel.kind) {
    case "webhook": next.url = text(config.url); break;
    case "custom_http":
      next.url = text(config.url);
      next.customMethod = text(config.method) || "POST";
      next.customHeaders = JSON.stringify(config.headers ?? {}, null, 2);
      next.customBody = text(config.body);
      break;
    case "email": next.to = text(config.to); next.subject = text(config.subject); break;
    case "bark":
      next.url = text(config.url); next.sound = text(config.sound); next.group = text(config.group);
      break;
    case "serverchan": next.sendkey = text(config.sendkey); break;
    case "telegram":
      next.tgToken = text(config.token); next.tgChatId = text(config.chat_id); next.tgHost = text(config.host);
      break;
    case "dingtalk": next.dingToken = text(config.access_token); break;
    case "wxpusher": next.wxToken = text(config.app_token); next.wxUid = text(config.uid); break;
    case "wxpusher_spt": next.spt = text(config.spt); break;
    case "wecom_app":
      next.corpId = text(config.corpid); next.agentId = text(config.agentid);
      next.secret = text(config.secret); next.toUser = text(config.to_user);
      break;
    case "wecom_webhook": next.wecomKey = text(config.key); break;
  }
  Object.assign(channelForm, next);
  void nextTick().then(() => channelFormEl.value?.scrollIntoView({ behavior: "smooth", block: "center" }));
}
function cancelChannelEdit() {
  Object.assign(channelForm, blankChannelForm());
}
async function saveChannel() {
  try {
    const config = buildChannelConfig();
    if (channelForm.id != null) {
      // The type is never sent: the config shape follows from it, so changing
      // one without the other would store a channel that cannot deliver.
      await api.updateNotificationChannel(channelForm.id, { name: channelForm.name, config });
      notify(t("channelUpdated"));
    } else {
      await api.createNotificationChannel(channelForm.name, channelForm.kind, config);
    }
    Object.assign(channelForm, blankChannelForm());
    await openNotifications();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleChannel(channel: NotificationChannel) {
  try { await api.updateNotificationChannel(channel.id, { enabled: !channel.enabled }); await openNotifications(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeChannel(channel: NotificationChannel) {
  if (!window.confirm(fmt("deleteChannelConfirm", { name: channel.name }))) return;
  try { await api.deleteNotificationChannel(channel.id); await openNotifications(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
// A channel that never delivers is the hardest notification bug to notice, so
// let the user fire one message and read the transport error inline.
const testingChannelId = ref<number | null>(null);
async function testChannel(channel: NotificationChannel) {
  testingChannelId.value = channel.id;
  try {
    await api.testNotificationChannel(channel.id);
    notify(fmt("testChannelSent", { name: channel.name }));
  } catch (cause) {
    notify(fmt("testChannelFailed", { message: cause instanceof Error ? cause.message : t("genericError") }), "error");
  } finally {
    testingChannelId.value = null;
  }
}
async function loadActions() {
  try {
    actions.value = await api.allNotificationActions();
    // A row can vanish under the editor — deleted here or in another tab.
    if (editingActionId.value != null && !actions.value.some((action) => action.id === editingActionId.value)) {
      editingActionId.value = null;
    }
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function saveAction() {
  try {
    if (!actionForm.taskIds.length) { notify(t("chooseTask"), "error"); return; }
    if (!actionForm.channelId) { notify(t("chooseChannel"), "error"); return; }
    const threshold = Math.max(1, Number(actionForm.failureThreshold) || 1);
    // The ticks survive a save, so filter out the pairs that already exist
    // instead of inserting duplicates. The list covers the whole account now,
    // so a duplicate is caught even on a task the form is not showing.
    const bound = new Set(actions.value.map((action) => `${action.task_id}:${action.channel_id}:${action.event}`));
    const pending = actionForm.taskIds.filter((id) => !bound.has(`${id}:${actionForm.channelId}:${actionForm.event}`));
    if (!pending.length) { notify(t("notifyAlreadyBound"), "error"); return; }
    await api.batchCreateNotificationActions(pending, actionForm.channelId, actionForm.event, threshold, actionForm.automaticOnly, actionForm.titleTemplate, actionForm.bodyTemplate);
    notify(fmt("notifyBoundCount", { n: pending.length }));
    Object.assign(actionForm, { failureThreshold: "1", automaticOnly: false, titleTemplate: "", bodyTemplate: "" });
    await loadActions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeAction(id: number) {
  try { await api.deleteNotificationAction(id); await loadActions(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function startEditAction(action: NotificationAction) {
  editingActionId.value = action.id;
  Object.assign(actionEdit, {
    channelId: action.channel_id,
    event: action.event,
    failureThreshold: String(action.failure_threshold),
    automaticOnly: action.automatic_only,
    titleTemplate: action.title_template ?? "",
    bodyTemplate: action.body_template ?? "",
  });
}
function cancelEditAction() {
  editingActionId.value = null;
}
async function saveActionEdit() {
  const id = editingActionId.value;
  if (id == null) return;
  try {
    await api.updateNotificationAction(id, {
      channel_id: actionEdit.channelId,
      event: actionEdit.event as NotificationAction["event"],
      failure_threshold: Math.max(1, Number(actionEdit.failureThreshold) || 1),
      automatic_only: actionEdit.automaticOnly,
      // An emptied template clears it; the server keeps whatever is left out.
      title_template: actionEdit.titleTemplate,
      body_template: actionEdit.bodyTemplate,
    });
    notify(t("actionUpdated"));
    editingActionId.value = null;
    await loadActions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- subscriptions (a section of the templates page, not a page of its own) ----------
const subscriptions = ref<TemplateSubscription[]>([]);
const {
  page: subsPage, pages: subsTotalPages, items: pagedSubscriptions,
  prev: subsPrevPage, next: subsNextPage,
} = usePager(() => subscriptions.value, () => subsPageSize.value, { onPageChange: () => scrollToPaged(subscriptionsAnchor) });
const subForm = reactive<{ name: string; url: string }>({ name: "", url: "" });
/** Add/edit dialog state: a null edit target means "create". */
const showSubModal = ref(false);
const subEditing = ref<TemplateSubscription | null>(null);
/** Refresh subscriptions and templates without touching the active view, so an
 *  import from the library can pick up the new templates in place. */
async function loadSubscriptions() {
  [subscriptions.value, templates.value] = await Promise.all([api.subscriptions(), api.allTemplates()]);
}
function openSubModal(sub?: TemplateSubscription) {
  subEditing.value = sub ?? null;
  Object.assign(subForm, { name: sub?.name ?? "", url: sub?.url ?? "" });
  showSubModal.value = true;
}
function closeSubModal() {
  showSubModal.value = false;
  subEditing.value = null;
}
async function saveSubModal() {
  try {
    if (subEditing.value) {
      await api.updateSubscription(subEditing.value.id, { name: subForm.name, url: subForm.url });
    } else {
      await api.createSubscription(subForm.name, subForm.url);
    }
    closeSubModal();
    await loadSubscriptions();
    // A new source changes the aggregate listing (a renamed one changes the
    // source labels), so refresh it - cached sources come back cheaply.
    if (library.value) await loadLibrary();
    else await ensureLibraryLoaded();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleSubscription(sub: TemplateSubscription) {
  try {
    await api.updateSubscription(sub.id, { enabled: !sub.enabled });
    await loadSubscriptions();
    // A disabled source drops out of the aggregate, so the table changes with it.
    if (library.value && libraryViewingAll.value) await loadLibrary();
  }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeSubscription(id: number) {
  if (!window.confirm(t("deleteSubConfirm"))) return;
  try {
    await api.deleteSubscription(id);
    // A deleted source must not leave the public list showing its catalogue.
    if (librarySourceId.value === id) {
      librarySourceId.value = null;
      library.value = null;
    }
    await loadSubscriptions();
    await ensureLibraryLoaded();
  }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- template library (the public half of the templates page) ----------
/** One shape for both ways of listing a library: a single source, and every
 *  source at once. The aggregate endpoint already answers in this shape and a
 *  single-source response is folded into it, so the table below renders one
 *  thing rather than branching on which request produced it. */
interface LibraryView {
  entries: LibraryEntry[];
  sources: LibrarySourceStatus[];
  truncated: boolean;
}
const library = ref<LibraryView | null>(null);
/** Which source the table shows: a subscription id, or `all` for the sum of
 *  every source. `all` is the default because "what is on offer" is one
 *  question; answering it one repository at a time is what made browsing feel
 *  like filing. */
const librarySourceId = ref<number | "all" | null>(null);
const libraryLoading = ref(false);
const libraryImporting = ref(false);
const librarySearch = ref("");
const librarySelected = ref<Set<string>>(new Set());
const libraryFailures = ref<{ name: string; error: string }[]>([]);
/** Entry whose per-row action is in flight — held as the row's selection key,
 *  because the aggregate can show the same name from two sources — so one row
 *  shows progress instead of the whole table. */
const libraryBusyKey = ref("");
/** Ticking entries is the exception, not the path (a source in "import all"
 *  mode needs no ticking at all), so the checkbox column only appears when the
 *  user asks for it. */
const libraryBatch = ref(false);
/** The library entry the open editor came from. While this is set, saving goes
 *  through `apply` — which also records the provenance row that marks the entry
 *  imported — instead of the local-file path. Cleared by `openImportModal`, so
 *  every other way into that editor is unaffected. */
const libraryPreview = ref<{ subscriptionId: number; entry: string; templateId: number | null } | null>(null);
const libraryViewingAll = computed(() => librarySourceId.value === "all");
const librarySource = computed(() =>
  typeof librarySourceId.value === "number"
    ? subscriptions.value.find((sub) => sub.id === librarySourceId.value) ?? null
    : null,
);
const librarySourceOptions = computed(() => [
  { value: "all" as const, label: t("libraryAllSources") },
  ...subscriptions.value.map((sub) => ({ value: sub.id, label: sub.name })),
]);
/** How the source's catalogue was read, so a plain repository (no manifest) is
 *  not mistaken for a broken library. Only meaningful for one source: across
 *  several, each reads its own way. */
const librarySourceKind = computed(() => {
  if (!library.value || libraryViewingAll.value) return "";
  const kind = library.value.sources[0]?.source_kind;
  if (!kind) return "";
  return kind === "files" ? t("librarySourceFiles") : t("librarySourceManifest");
});
/** Sources that could not be read. Listed so a dead repository explains itself
 *  instead of only contributing no rows. */
const librarySourceFaults = computed(() => (library.value?.sources ?? []).filter((source) => source.error));
/** The server caps how many entries it will put in one aggregate response; when
 *  the cap is hit the list is short of what the sources offer, which is worth
 *  saying rather than showing a silently incomplete table. */
const libraryTruncated = computed(() => library.value?.truncated === true);
/** A row's source: the aggregate fills it in per entry, and a single-source
 *  listing *is* the selection. */
function entrySourceId(entry: LibraryEntry): number | null {
  if (entry.source_id != null) return entry.source_id;
  return typeof librarySourceId.value === "number" ? librarySourceId.value : null;
}
/** Selection identity. A name is only unique inside one source — two libraries
 *  can each offer one — so the key carries the source as well. */
function libraryKey(entry: LibraryEntry): string {
  return `${entrySourceId(entry) ?? 0}:${entry.name}`;
}
/** Entries matching the current search, in the clicked sort order. */
const libraryEntries = computed<LibraryEntry[]>(() => {
  const entries = library.value?.entries ?? [];
  const query = librarySearch.value.trim().toLowerCase();
  const matched = entries.filter((entry) => {
    if (!query) return true;
    return `${entry.name} ${entry.author ?? ""} ${entry.source_name ?? ""}`.toLowerCase().includes(query);
  });
  return sortRows(matched, librarySort, (entry, key) => {
    switch (key) {
      case "name": return entry.name;
      case "author": return entry.author ?? "";
      case "source": return entry.source_name ?? "";
      case "version": return entry.version ?? "";
      default: return entry.date ?? "";
    }
  });
});
const librarySelectedCount = computed(() => librarySelected.value.size);
/** Paging over the filtered catalogue: a library can run to hundreds of
 *  entries and one long table is unreadable. Any change to the list
 *  underneath (search, filter, sort, source, refresh) restarts at page 1. */
const {
  page: libraryPage, pages: libraryTotalPages, items: pagedLibraryEntries,
  prev: libraryPrevPage, next: libraryNextPage, reset: resetLibraryPage,
} = usePager(() => libraryEntries.value, () => libraryPageSize.value, { onPageChange: () => scrollToPaged(libraryAnchor) });
watch(libraryEntries, resetLibraryPage);
/** Only entries currently visible — the page on screen — can be selected, so a
 *  filter or a page turn never hides part of the selection. */
const libraryAllVisibleSelected = computed(
  () => pagedLibraryEntries.value.length > 0 && pagedLibraryEntries.value.every((entry) => librarySelected.value.has(libraryKey(entry))),
);
/** Headers of the library table; `date` is the manifest's own timestamp, which
 *  is why it — not our `imported_at` — is what the rows are ordered by. The
 *  source column appears only where more than one source can appear in it. */
const libraryColumns = computed(() => [
  { key: "name" as const, label: t("name"), text: true },
  { key: "author" as const, label: t("libraryAuthor"), text: true },
  ...(libraryViewingAll.value ? [{ key: "source" as const, label: t("librarySource"), text: true }] : []),
  { key: "version" as const, label: t("libraryVersion"), text: true },
  { key: "date" as const, label: t("libraryDate"), text: false },
]);
/** Read the source list into the section on arrival, so the catalogue is there
 *  without a navigation — but never re-fetch a source already in hand, and never
 *  fetch just because a publish/unpublish refreshed the page. */
async function ensureLibraryLoaded() {
  if (library.value || librarySourceId.value != null) return;
  if (subscriptions.value.length === 0) return;
  librarySourceId.value = "all";
  await loadLibrary();
}
/** Point the section at another source, or at all of them. */
async function selectLibrarySource(id: number | "all") {
  if (id === librarySourceId.value && library.value) return;
  librarySourceId.value = id;
  library.value = null;
  librarySelected.value = new Set();
  libraryFailures.value = [];
  await loadLibrary();
}
async function loadLibrary(refresh = false) {
  const source = librarySourceId.value;
  if (source == null) return;
  libraryLoading.value = true;
  try {
    // Both responses land in one shape, so the table has a single input.
    if (source === "all") {
      const result = await api.libraryOverview(refresh);
      library.value = { entries: result.entries, sources: result.sources, truncated: result.truncated };
    } else {
      const result = await api.browseSubscriptionLibrary(source);
      library.value = {
        entries: result.entries,
        sources: [
          {
            subscription_id: result.subscription_id,
            name: librarySource.value?.name ?? "",
            source_kind: result.source_kind,
            entries: result.entries.length,
            cached: false,
          },
        ],
        truncated: false,
      };
    }
    // Drop selections the source no longer offers.
    const keys = new Set(library.value.entries.map(libraryKey));
    librarySelected.value = new Set([...librarySelected.value].filter((key) => keys.has(key)));
  } catch (cause) {
    library.value = null;
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    libraryLoading.value = false;
  }
}
function toggleLibraryEntry(entry: LibraryEntry) {
  const next = new Set(librarySelected.value);
  const key = libraryKey(entry);
  if (next.has(key)) next.delete(key); else next.add(key);
  librarySelected.value = next;
}
function toggleLibraryAllVisible() {
  librarySelected.value = libraryAllVisibleSelected.value
    ? new Set()
    : new Set(pagedLibraryEntries.value.map(libraryKey));
}
function clearLibrarySelection() {
  librarySelected.value = new Set();
}
/** The row action: fetch this entry and hand it to the editor, writing nothing.
 *
 *  This is the whole point of the page. What a library offers is upstream
 *  content the user has not chosen yet, so it is shown first and only a save
 *  puts it among their templates — the old "import" was a black box that
 *  reported a count and left nothing to act on. */
async function previewLibraryEntry(entry: LibraryEntry) {
  const sourceId = entrySourceId(entry);
  if (sourceId == null) return;
  libraryBusyKey.value = libraryKey(entry);
  libraryFailures.value = [];
  try {
    const preview = await api.previewSubscriptionEntry(sourceId, entry.name);
    // The source publishes a bare request array, the endpoint hands it back
    // untouched, and the editor only reads HAR documents — this is the step
    // that used to be missing, which is why the editor opened empty and had
    // nothing to save.
    const doc = harDocumentFrom(preview.har);
    if (!doc) {
      notify(t("harJsonError"), "error");
      return;
    }
    openImportModal();
    importForm.name = preview.entry.name;
    // The manifest's variable notes are what a reader wants as the description,
    // and the field is a single line, so fold them onto one.
    importForm.description = (preview.entry.comments ?? "").replace(/\s+/g, " ").trim();
    harEditorDoc.value = doc;
    // Set after `openImportModal`, which clears it along with every other entry
    // point into this editor.
    libraryPreview.value = {
      subscriptionId: sourceId,
      entry: preview.entry.name,
      templateId: preview.entry.installed_template_id ?? null,
    };
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    libraryBusyKey.value = "";
  }
}
/** Entries already in sync have nothing to fetch, so their row opens the local
 *  copy straight away. A missing local copy falls back to the preview path,
 *  which keeps the row honest when a template was deleted here while the
 *  provenance row survived. */
function openLibraryEntry(entry: LibraryEntry) {
  const local = entry.installed_template_id != null
    ? templates.value.find((item) => item.id === entry.installed_template_id)
    : undefined;
  if (local) openImportModal(local);
  else void previewLibraryEntry(entry);
}
/** The bulk path, kept as a secondary way in: a source in "import all" mode
 *  syncs by itself, so ticking entries is only ever needed to pre-load a
 *  handful.
 *
 *  Unlike the row action this does not open the editor — a batch has no single
 *  template to open — so it only reports what happened, per entry. A selection
 *  can span sources when the table shows all of them and the import endpoint is
 *  per source, so the calls are grouped and the totals reported together. */
async function importSelectedLibraryEntries() {
  const selected = new Set(librarySelected.value);
  const bySource = new Map<number, string[]>();
  for (const entry of library.value?.entries ?? []) {
    if (!selected.has(libraryKey(entry))) continue;
    const sourceId = entrySourceId(entry);
    if (sourceId == null) continue;
    const names = bySource.get(sourceId) ?? [];
    names.push(entry.name);
    bySource.set(sourceId, names);
  }
  if (bySource.size === 0) {
    notify(t("libraryNothingSelected"), "error");
    return;
  }
  libraryImporting.value = true;
  libraryFailures.value = [];
  try {
    let imported = 0;
    let updated = 0;
    const failed: { name: string; error: string }[] = [];
    for (const [sourceId, names] of bySource) {
      const result = await api.importSubscriptionTemplates(sourceId, names);
      imported += result.imported;
      updated += result.updated;
      failed.push(...result.failed);
    }
    const parts: string[] = [];
    if (imported) parts.push(fmt("libraryImportedCount", { n: imported }));
    if (updated) parts.push(fmt("libraryUpdatedCount", { n: updated }));
    if (failed.length) parts.push(fmt("libraryFailedCount", { n: failed.length }));
    if (parts.length) notify(parts.join(" · "), failed.length ? "error" : "success");
    libraryFailures.value = failed;
    librarySelected.value = new Set();
    // Reflect the new installed/update state, and pull the new templates into
    // the list above without leaving the page.
    await Promise.all([loadLibrary(), loadSubscriptions()]);
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally {
    libraryImporting.value = false;
  }
}

// ---------- push ----------
const myPushRequests = ref<PushRequest[]>([]);
const pendingPushRequests = ref<PushRequest[]>([]);
const pushNote = ref("");
const pushTemplateId = ref(0);
const pushTemplateDropdownOptions = computed(() => [
  { value: 0, label: t("chooseTask"), disabled: true },
  ...templates.value.map((tpl) => ({ value: tpl.id, label: tpl.name })),
]);
const isAdmin = computed(() => currentUser.value?.role === "admin");
async function openPush() {
  view.value = "push";
  try {
    const [mine, tpls] = await Promise.all([api.myPushRequests(), api.allTemplates()]);
    myPushRequests.value = mine;
    templates.value = tpls;
    pendingPushRequests.value = isAdmin.value ? await api.adminPushRequests("pending").catch(() => []) : [];
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function submitPush() {
  try {
    if (!pushTemplateId.value) { notify(t("chooseTask"), "error"); return; }
    await api.createPushRequest(pushTemplateId.value, pushNote.value);
    pushNote.value = "";
    pushTemplateId.value = 0;
    notify(t("pushDone"));
    await openPush();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function decidePush(id: number, approve: boolean) {
  try { await api.decidePushRequest(id, approve); await openPush(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function pushStatusLabel(status: string): string {
  if (status === "approved") return t("pushApproved");
  if (status === "rejected") return t("pushRejected");
  return t("pushPending");
}

// ---------- admin ----------
const adminUsers = ref<User[]>([]);
const adminSettings = ref<SiteSetting[]>([]);
/** ADR-0008's two relaxations. Each key is the whole contract with the server:
 *  an unknown key is stored happily and then ignored, so a typo here would
 *  present as "the switch does nothing". Both are pinned against the matching
 *  Rust constants by admin-settings.test.ts. */
const ALLOW_PRIVATE_NETWORK_KEY = "security.allow_private_network";
const ALLOW_INVALID_CERTIFICATES_KEY = "security.allow_invalid_certificates";
const settingsForm = reactive({
  requireEmail: false, gaKey: "", retentionDays: 0,
  allowPrivateNetwork: false, allowInvalidCertificates: false,
});
/** The two risk-gated fields, named as the form names them. */
type RiskField = "allowPrivateNetwork" | "allowInvalidCertificates";
/** Which risk-gated switch is waiting for a confirmation, if any. Drives the
 *  dialog; the *notice* it shows is chosen in the template so every switch
 *  keeps its own sentence rather than sharing one generic warning. */
const riskPrompt = ref<RiskField | null>(null);
/** The checkbox the user just ticked, kept only so a cancelled prompt can put
 *  it back. Deliberately not reactive: nothing renders from it. */
let riskCheckbox: HTMLInputElement | null = null;
/** Turning a guarded switch on asks first; turning one off is taken as given.
 *
 *  The guarded checkboxes are `:checked`-bound rather than `v-model`-bound, and
 *  that is the point: `v-model` writes the new value into the form before this
 *  handler runs, so "cancel" would be undoing a change that may or may not have
 *  landed yet — correct only if Vue happens to call the two listeners in one
 *  order. Here the form keeps its old value until 确认, and 取消 restores the
 *  box itself, so neither button depends on listener order. */
function onRiskToggle(field: RiskField, event: Event) {
  const box = event.target as HTMLInputElement;
  if (!box.checked) { settingsForm[field] = false; return; }
  riskCheckbox = box;
  riskPrompt.value = field;
}
function acceptRisk() {
  if (riskPrompt.value) settingsForm[riskPrompt.value] = true;
  riskPrompt.value = null; riskCheckbox = null;
}
function cancelRisk() {
  if (riskCheckbox) riskCheckbox.checked = false;
  riskPrompt.value = null; riskCheckbox = null;
}
async function openAdmin() {
  view.value = "admin";
  try {
    [adminUsers.value, adminSettings.value] = await Promise.all([api.adminUsers(), api.adminSettings()]);
    const requireEmail = adminSettings.value.find((s) => s.key === "require_email_verification");
    const ga = adminSettings.value.find((s) => s.key === "ga_key");
    const retention = adminSettings.value.find((s) => s.key === "logs.retention_days");
    const privateNetwork = adminSettings.value.find((s) => s.key === ALLOW_PRIVATE_NETWORK_KEY);
    const invalidCertificates = adminSettings.value.find((s) => s.key === ALLOW_INVALID_CERTIFICATES_KEY);
    settingsForm.requireEmail = requireEmail?.value === true;
    settingsForm.gaKey = typeof ga?.value === "string" ? ga.value : "";
    settingsForm.retentionDays = typeof retention?.value === "number" ? retention.value : 0;
    settingsForm.allowPrivateNetwork = privateNetwork?.value === true;
    settingsForm.allowInvalidCertificates = invalidCertificates?.value === true;
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleUser(user: User) {
  try { await api.adminUpdateUser(user.id, { disabled: !user.disabled }); await openAdmin(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
const roleDropdownOptions: { value: string; label: string }[] = [
  { value: "user", label: t("roleUser") },
  { value: "admin", label: t("roleAdmin") },
];
async function changeUserRole(user: User, role: string) {
  try { await api.adminUpdateUser(user.id, { role }); await openAdmin(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function deleteUser(user: User) {
  if (!window.confirm(fmt("deleteUserConfirm", { name: user.username }))) return;
  try { await api.adminDeleteUser(user.id); notify(t("taskDeleted")); await openAdmin(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function saveAdminSettings() {
  try {
    await api.adminSetSetting("require_email_verification", settingsForm.requireEmail);
    await api.adminSetSetting("ga_key", settingsForm.gaKey);
    await api.adminSetSetting("logs.retention_days", settingsForm.retentionDays);
    await api.adminSetSetting(ALLOW_PRIVATE_NETWORK_KEY, settingsForm.allowPrivateNetwork);
    await api.adminSetSetting(ALLOW_INVALID_CERTIFICATES_KEY, settingsForm.allowInvalidCertificates);
    notify(t("settingsSaved"));
    await openAdmin();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function cleanupLogs() {
  try {
    const result = await api.adminClearLogs(settingsForm.retentionDays);
    notify(fmt("logsCleaned", { n: result.deleted }));
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function downloadBackup() {
  api.adminBackup()
    .then((backup) => {
      const blob = new Blob([JSON.stringify(backup, null, 2)], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `qdrust-backup-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.json`;
      a.click();
      URL.revokeObjectURL(url);
      notify(t("backupDone"));
    })
    .catch((cause) => notify(cause instanceof Error ? cause.message : t("genericError"), "error"));
}
async function restoreBackup(file: File | undefined) {
  if (!file) return; // user cancelled the picker
  if (!window.confirm(t("restoreConfirm"))) return;
  try {
    const text = await file.text();
    await api.adminRestore(JSON.parse(text));
    notify(t("restoreDone"));
    await openAdmin();
    await loadTasks();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function onRestoreFile(event: Event) {
  const input = event.target as HTMLInputElement;
  restoreBackup(input.files?.[0]);
  input.value = "";
}

// ---------- settings / account ----------
const pwdForm = reactive({ current: "", next: "" });
async function changePassword() {
  try {
    await api.changePassword(pwdForm.current, pwdForm.next);
    notify(t("passwordChanged"));
    Object.assign(pwdForm, { current: "", next: "" });
    // backend revokes all sessions after password change
    await logout(true);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function rotateCsrf() {
  try { await api.rotateCsrf(); notify(t("csrfRotated")); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function resendVerification() {
  try { await api.resendVerification(); notify(t("verifySent")); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- auth ----------
async function authenticate() {
  authNotice.value = "";
  try {
    if (authMode.value === "reset") {
      await api.resetPassword(authForm.token, authForm.newPassword);
      notify(t("resetDone"));
      authMode.value = "login";
      Object.assign(authForm, { username: "", password: "", token: "", newPassword: "" });
      return;
    }
    const session = authMode.value === "login"
      ? await api.login(authForm.username, authForm.password)
      : authMode.value === "bootstrap"
        ? await api.bootstrap(authForm.username, authForm.password)
        : await api.register(authForm.username, authForm.password, authForm.email || undefined);
    currentUser.value = session.user;
    authenticated.value = true;
    await loadTasks();
  } catch (cause) {
    authNotice.value = cause instanceof Error ? cause.message : t("genericError");
  }
}
async function submitForgot() {
  authNotice.value = "";
  try {
    const result = await api.forgotPassword(authForm.username);
    forgotResult.value = { sent: result.sent, token: result.reset_token };
    authNotice.value = t("forgotSent");
    if (result.reset_token) {
      // dev mode: jump straight into the reset form
      authForm.token = result.reset_token;
    }
  } catch (cause) { authNotice.value = cause instanceof Error ? cause.message : t("genericError"); }
}
async function logout(silent = false) {
  try { await api.logout(); } catch { /* ignore */ }
  authenticated.value = false;
  currentUser.value = null;
  tasks.value = [];
  runsByTask.value = {};
  authMode.value = "login";
  Object.assign(authForm, { username: "", password: "" });
  // OIDC single logout: once the local session is gone, also end the external
  // IdP session with a top-level navigation (explicit user logout only). The
  // configured post_logout_redirect_uri (if any) brings the user back to this
  // app's now-logged-out page.
  const idpLogout = silent ? "" : oidcLogoutUrl(authPolicy.value);
  if (idpLogout) {
    markLogoutReturn(sessionStorage);
    window.location.assign(idpLogout);
    return;
  }
  if (!silent) notify(t("logout"));
}

// ---------- external IdP (SSO) ----------
/** Send the browser to the OIDC start endpoint (top-level navigation so the
 *  IdP redirect + SameSite=Lax state cookie round-trip works normally). */
function startSso() {
  authNotice.value = "";
  ssoError.value = "";
  window.location.assign(oidcStartUrl());
}

// In a pure-OIDC deployment (auth_mode === "oidc") the only way in is the
// external IdP, so on landing unauthenticated we jump straight to it rather
// than showing an empty login card that just waits for a click. We never
// auto-redirect when the IdP bounced us back with a `login_error` (that would
// loop): the SSO-only panel remains as the error/retry landing page.
function maybeAutoStartSso() {
  if (authenticated.value) return;
  // Read-and-clear: the marker makes exactly one post-logout landing page jump
  // to the IdP, and ages out so an abandoned logout cannot hijack a later visit.
  const returningFromLogout = consumeLogoutReturn(sessionStorage);
  if (!ssoForced.value && !returningFromLogout) return;
  // Do not hijack a page that is reporting an earlier SSO failure.
  const url = new URLSearchParams(location.search).get("login_error");
  if (url) return;
  startSso();
}

/** Map a server `login_error=<code>` to a human-readable message. Falls back
 *  to the raw code so an unknown/forward-compatible reason stays visible. */
function ssoErrorMessage(code: string): string {
  const map: Record<string, string> = {
    oidc_provider_error: t("ssoProviderError"),
    oidc_state_mismatch: t("ssoStateError"),
    oidc_state_invalid: t("ssoStateError"),
    oidc_integrity_failed: t("ssoStateError"),
    oidc_redirect_mismatch: t("ssoStateError"),
    external_identity_conflict: t("ssoConflict"),
    user_disabled: t("ssoDisabled"),
    provisioning_disabled: t("ssoProvisioningDisabled"),
    oidc_not_configured: t("ssoNotConfigured"),
  };
  return map[code] ?? code;
}

// ---------- boot ----------
onMounted(async () => {
  // Handle deep-link tokens: /reset-password?token=... and /verify-email?token=...
  const params = new URLSearchParams(location.search);
  const token = params.get("token");
  const kind = params.get("type") ?? (location.pathname.includes("reset") ? "reset" : location.pathname.includes("verify") ? "verify" : "");
  if (token) {
    if (kind === "verify") {
      try {
        await api.verifyEmail(token);
        verifyResult.value = "ok";
      } catch { verifyResult.value = "fail"; }
      history.replaceState(null, "", location.pathname);
    } else {
      authMode.value = "reset";
      authForm.token = token;
      history.replaceState(null, "", location.pathname);
    }
  }
  // Surface an OIDC callback failure (server redirects back with
  // `?login_error=<code>`) and clear the query param so a reload does not
  // show a stale error. (Matches the deep-link handling that also clears the
  // query string on a top-level landing.)
  const urlError = new URLSearchParams(location.search).get("login_error");
  if (urlError) {
    ssoError.value = urlError;
    history.replaceState(null, "", location.pathname);
  }
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      if (showCreate.value) showCreate.value = false;
      if (showImport.value) closeHarEditor();
      if (showHelp.value) showHelp.value = false;
      menuOpen.value = false;
    }
  });
  try {
    const session = await api.session();
    currentUser.value = session.user;
    authenticated.value = true;
    // A live session proves the backend is reachable -> the "service ok"
    // indicator is on without a separate readiness round-trip.
    ready.value = true;
    // Best-effort load of the task list. Do NOT gate the authenticated boot on
    // liveness/readiness probes: under a sub-path deployment the backend keeps
    // `/ready`/`/health` at the bare root (Docker HEALTHCHECK), so a request to
    // them may 404/502 at the reverse proxy and must never log a valid session
    // back out (that made SSO "never log in" under /qd).
    await loadTasks();
  } catch {
    authenticated.value = false;
    ready.value = true;
  }
  // Best-effort: learn the auth policy to shape the sign-in page. Never fails
  // the boot — if it errors the page falls back to the local form defaults.
  // The `ssoForced` computed then drives the pure-OIDC (SSO-only) panel.
  try {
    authPolicy.value = await api.authConfig();
  } catch {
    /* keep authPolicy null -> local-form default */
  }
  // Pure-OIDC mode: jump straight to the IdP unless a prior SSO attempt failed.
  maybeAutoStartSso();
});
const refreshTimer = window.setInterval(() => {
  if (!authenticated.value) return;
  void refreshTaskStatuses();
  if (runHistoryTask.value != null) void loadTaskRuns(runHistoryTask.value.id);
}, 5000);
onUnmounted(() => window.clearInterval(refreshTimer));
</script>

<template>
  <!-- ============ AUTH ============ -->
  <main v-if="!authenticated" class="auth-page">
    <!-- Pure-OIDC deployment (auth_mode === "oidc"): only the SSO affordance. -->
    <div v-if="ssoForced" class="auth-panel">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <h1>{{ t('loginTitle') }}</h1>
      <p v-if="!ssoError" class="auth-hint">{{ t('ssoHint') }}</p>
      <div v-if="ssoError" class="auth-notice auth-error">{{ ssoErrorMessage(ssoError) }}</div>
      <button class="primary-button sso-button" type="button" @click="startSso">
        <span class="sso-label">{{ t('ssoAction') }} · {{ ssoProviderName }}</span><ArrowRight :size="16" />
      </button>
    </div>

    <!-- Hybrid / local mode: the conventional form, plus an SSO entry when the
         deployment offers OIDC. Local fields are hidden when the server closes
         local login (local_login_enabled = false). -->
    <form v-else class="auth-panel" @submit.prevent="authMode === 'forgot' ? submitForgot() : authenticate()">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <h1>{{ authMode === "login" ? t('loginTitle') : authMode === "bootstrap" ? t('bootstrapTitle') : authMode === "register" ? t('registerTitle') : authMode === "forgot" ? t('forgotTitle') : t('resetTitle') }}</h1>

      <!-- SSO shortcut (shown above the local form when offered). -->
      <button v-if="showSso && !ssoForced" class="secondary-button sso-button" type="button" @click="startSso">
        <span class="sso-label">{{ t('ssoAction') }} · {{ ssoProviderName }}</span>
      </button>
      <template v-if="showSso && !ssoForced && localAuthAvailable && (authMode === 'login' || authMode === 'bootstrap' || authMode === 'register')">
        <div class="auth-divider" role="separator"><span>{{ t('ssoOrLocal') }}</span></div>
      </template>

      <div v-if="ssoError" class="auth-notice auth-error">{{ ssoErrorMessage(ssoError) }}</div>

      <!-- Local credential entry (hidden in pure-SSO and when local disabled). -->
      <template v-if="showLocalForm">
        <label>{{ t('username') }}<input v-model="authForm.username" required autocomplete="username" minlength="3" /></label>
        <label v-if="authMode === 'register'">{{ t('email') }}<input v-model="authForm.email" type="email" autocomplete="email" /></label>
        <label v-if="authMode === 'register'" class="auth-hint">{{ t('registerHint') }}</label>
        <label>{{ t('password') }}<input v-model="authForm.password" required minlength="12" type="password" :autocomplete="authMode === 'login' ? 'current-password' : 'new-password'" /></label>
        <label v-if="authMode === 'register'" class="auth-hint">{{ t('passwordMinHint') }}</label>
      </template>

      <!-- Forgot-password flow (local email/username based). -->
      <template v-else-if="authMode === 'forgot'">
        <label>{{ t('username') }}<input v-model="authForm.username" required autocomplete="username" /></label>
        <p class="auth-hint">{{ t('forgotHint') }}</p>
        <div v-if="forgotResult?.token" class="dev-token">
          {{ t('forgotDevToken') }}<code>{{ forgotResult.token }}</code>
          <button type="button" class="secondary-button" @click="authMode = 'reset'">{{ t('resetTitle') }}<ArrowRight :size="15" /></button>
        </div>
      </template>

      <!-- Reset-password flow. -->
      <template v-else-if="authMode === 'reset'">
        <label>{{ t('username') }}<input :value="authForm.token" disabled /></label>
        <label>{{ t('resetNewPassword') }}<input v-model="authForm.newPassword" required minlength="12" type="password" /></label>
      </template>

      <div v-if="authNotice" class="auth-notice">{{ authNotice }}</div>
      <div v-if="verifyResult" class="auth-notice">{{ verifyResult === 'ok' ? t('verifyDone') : t('verifyFail') }}</div>

      <!-- Local actions (submit + mode switch) are only meaningful when a local
           credential entry is present or a forgot/reset flow is active. -->
      <button v-if="showLocalForm || authMode === 'forgot' || authMode === 'reset'" class="primary-button" type="submit">
        {{ authMode === 'login' ? t('login') : authMode === 'bootstrap' ? t('createAdmin') : authMode === 'register' ? t('register') : authMode === 'forgot' ? t('forgotSubmit') : t('resetSubmit') }}
      </button>

      <button v-if="showLocalForm && authMode === 'login'" class="secondary-button" type="button" @click="authMode='forgot'; authNotice=''; ssoError=''">{{ t('forgotPassword') }}</button>
      <button v-if="(authMode === 'forgot' || authMode === 'reset') && localAuthAvailable" class="secondary-button" type="button" @click="authMode='login'; authNotice=''; ssoError=''">{{ t('backToLogin') }}</button>
      <button v-if="showLocalForm && (authMode === 'login' || authMode === 'bootstrap' || authMode === 'register')" class="secondary-button" type="button" @click="authMode = authMode === 'login' ? 'bootstrap' : authMode === 'bootstrap' ? 'register' : 'login'; authNotice=''; verifyResult=null">
        {{ authMode === 'login' ? t('initAdmin') : authMode === 'bootstrap' ? t('needAccount') : t('haveAccount') }}
      </button>

      <div class="surface-theme-switch auth-theme" role="group" :aria-label="t('theme')">
        <button
          v-for="mode in THEME_MODES"
          :key="mode"
          type="button"
          class="surface-theme-option"
          :class="{ 'is-active': themeMode === mode }"
          :title="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
          :aria-label="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
          :aria-pressed="themeMode === mode"
          @click="setTheme(mode)"
        >
          <Sun v-if="mode === 'light'" :size="15" />
          <Moon v-else-if="mode === 'dark'" :size="15" />
          <Monitor v-else :size="15" />
        </button>
      </div>
    </form>

    <!-- Theme switch for the pure-SSO landing panel. -->
    <div v-if="ssoForced" class="surface-theme-switch auth-theme" role="group" :aria-label="t('theme')">
      <button
        v-for="mode in THEME_MODES"
        :key="mode"
        type="button"
        class="surface-theme-option"
        :class="{ 'is-active': themeMode === mode }"
        :title="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
        :aria-label="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
        :aria-pressed="themeMode === mode"
        @click="setTheme(mode)"
      >
        <Sun v-if="mode === 'light'" :size="15" />
        <Moon v-else-if="mode === 'dark'" :size="15" />
        <Monitor v-else :size="15" />
      </button>
    </div>
  </main>

  <!-- ============ APP ============ -->
  <div v-else class="app-shell">
    <aside :class="['sidebar', { open: menuOpen }]">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <nav aria-label="主导航">
        <a :class="['nav-link', { active: view === 'tasks' }]" href="#" @click.prevent="view='tasks'"><LayoutDashboard :size="18" />{{ t('tasks') }}</a>
        <a :class="['nav-link', { active: view === 'templates' }]" href="#" @click.prevent="openTemplates"><FileJson2 :size="18" />{{ t('templates') }}</a>
        <a :class="['nav-link', { active: view === 'plugins' }]" href="#" @click.prevent="openPlugins"><Settings :size="18" />{{ t('plugins') }}</a>
        <a :class="['nav-link', { active: view === 'notifications' }]" href="#" @click.prevent="openNotifications"><Bell :size="18" />{{ t('notifications') }}</a>
        <a :class="['nav-link', { active: view === 'push' }]" href="#" @click.prevent="openPush"><Send :size="18" />{{ t('push') }}</a>
        <a v-if="isAdmin" :class="['nav-link', { active: view === 'admin' }]" href="#" @click.prevent="openAdmin"><Users :size="18" />{{ t('admin') }}</a>
        <a :class="['nav-link', { active: view === 'settings' }]" href="#" @click.prevent="view='settings'"><Settings :size="18" />{{ t('settings') }}</a>
      </nav>
      <div class="sidebar-bottom">
        <a class="nav-link" href="#" @click.prevent="showHelp = true"><CircleHelp :size="18" />{{ t('help') }}</a>
        <div class="system-state"><span :class="['state-dot', { online: ready }]" />{{ ready ? t('serviceOk') : t('connecting') }}</div>
      </div>
    </aside>

    <div v-if="menuOpen" class="scrim" @click="menuOpen = false" />

    <main class="app-main">
      <header class="topbar">
        <button class="icon-button mobile-menu" :title="t('menu')" @click="menuOpen = true"><Menu :size="20" /></button>
        <div class="breadcrumb">{{ t('workspace') }} <span>/</span> {{ currentViewName }}</div>
        <div class="topbar-right">
          <span v-if="currentUser?.email && !currentUser.email_verified" class="verify-hint" :title="t('emailVerifyBanner')">
            <Mail :size="14" />{{ t('emailVerifyBanner') }}
            <button class="text-button" @click="resendVerification">{{ t('resendVerify') }}</button>
          </span>
          <div class="surface-theme-switch" role="group" :aria-label="t('theme')">
            <button
              v-for="mode in THEME_MODES"
              :key="mode"
              type="button"
              class="surface-theme-option"
              :class="{ 'is-active': themeMode === mode }"
              :title="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
              :aria-label="mode === 'light' ? t('themeLight') : mode === 'dark' ? t('themeDark') : t('themeSystem')"
              :aria-pressed="themeMode === mode"
              @click="setTheme(mode)"
            >
              <Sun v-if="mode === 'light'" :size="15" />
              <Moon v-else-if="mode === 'dark'" :size="15" />
              <Monitor v-else :size="15" />
            </button>
          </div>
          <button class="icon-button" :title="locale" @click="toggleLocale">{{ locale === 'zh-CN' ? 'EN' : '中' }}</button>
          <span class="account-name">{{ currentUser?.username }} · {{ currentUser?.role === 'admin' ? t('roleAdmin') : t('roleUser') }}</span>
          <button class="avatar" :title="t('logout')" @click="logout()">{{ currentUser?.username.slice(0, 2).toUpperCase() }}</button>
        </div>
      </header>

      <Transition name="view" mode="out-in">
      <!-- ===== TASKS ===== -->
      <div v-if="view === 'tasks'" class="page">
        <section class="page-heading">
          <div><h1>{{ t('tasks') }}</h1><p>{{ t('createFirst') }}</p></div>
          <div class="heading-actions">
            <button class="primary-button" @click="openCreateTask"><Plus :size="17" />{{ t('createTaskShort') }}</button>
            <button class="secondary-button" @click="openRunLog"><Activity :size="16" />{{ t('runLogTitle') }}</button>
          </div>
        </section>

        <section class="stats" aria-label="任务概览">
          <div><span>{{ t('totalTasks') }}</span><strong>{{ tasks.length }}</strong><small><CalendarClock :size="14" />{{ t('configured') }}</small></div>
          <div><span>{{ t('enabledTasks') }}</span><strong>{{ activeCount }}</strong><small class="positive"><Activity :size="14" />{{ t('scheduledEnabled') }}</small></div>
          <div><span>{{ t('lastSuccess') }}</span><strong>{{ successCount }}</strong><small><Check :size="14" />{{ t('hasResults') }}</small></div>
        </section>

        <section class="task-section">
          <div class="toolbar">
            <label class="search"><Search :size="17" /><input v-model="search" type="search" :placeholder="t('search')" /></label>
            <label class="group-filter">
              <span>{{ t('groupFilter') }}</span>
              <Dropdown v-model="groupFilter" :options="groupFilterDropdownOptions" compact />
            </label>
            <button class="icon-button" :title="t('refresh')" @click="loadTasks"><RefreshCw :class="{ spin: loading }" :size="18" /></button>
          </div>

          <div v-if="selected.size > 0" class="batch-bar">
            <span>{{ fmt('selectedCount', { n: selected.size }) }}</span>
            <button class="secondary-button" @click="batchTasks('enable')">{{ t('batchEnable') }}</button>
            <button class="secondary-button" @click="batchTasks('disable')">{{ t('batchDisable') }}</button>
            <button class="secondary-button" @click="batchTasks('run')">{{ t('batchRun') }}</button>
            <button class="secondary-button" @click="openNotificationsFor([...selected])"><Bell :size="15" />{{ t('batchNotify') }}</button>
            <button class="secondary-button danger" @click="batchTasks('delete')">{{ t('batchDelete') }}</button>
            <button class="icon-button" :title="t('close')" @click="selected.clear()"><X :size="16" /></button>
          </div>

          <div v-if="loading" class="loading-state"><RefreshCw class="spin" :size="22" />{{ t('loading') }}</div>
          <div v-else-if="filteredTasks.length === 0" class="empty-state">
            <span><CalendarClock :size="25" /></span>
            <h2>{{ search || groupFilter ? t('noTasksMatch') : t('createFirst') }}</h2>
            <button v-if="!search && !groupFilter" class="secondary-button" @click="openCreateTask"><Plus :size="16" />{{ t('createTaskShort') }}</button>
          </div>
          <template v-else>
          <div class="paged-anchor" ref="tasksAnchor"></div>
          <div class="table-wrap tasks-wrap">
            <table class="tasks-table">
              <thead><tr>
                <th class="col-check"><input type="checkbox" :checked="pagedTasks.length > 0 && pagedTasks.every(x => selected.has(x.id))" :title="t('selectAll')" @change="selectAllVisible" /></th>
                <th>{{ t('name') }}</th><th class="col-schedule">{{ t('schedule') }}</th><th>{{ t('lastRunAt') }}</th><th>{{ t('status') }}</th><th class="col-group">{{ t('group') }}</th><th><span class="sr-only">{{ t('more') }}</span></th>
              </tr></thead>
              <tbody>
                <template v-for="task in pagedTasks" :key="task.id">
                  <tr>
                    <td class="col-check"><input type="checkbox" :checked="selected.has(task.id)" @change="toggleSelect(task.id)" /></td>
                    <td class="col-name"><div class="task-name"><strong>{{ task.name }}</strong></div></td>
                    <td class="col-schedule"><code>{{ task.cron }}</code></td>
                    <td class="col-time">{{ formatRunTime(task.last_run_at, undefined, task.timezone || undefined) }}</td>
                    <td class="col-status"><button :class="['status-pill', { paused: task.disabled, 'run-bad': taskStatusLabel(task) === '失败' }]" @click="toggleTask(task)"><span />{{ taskStatusLabel(task) }}</button></td>
                    <td class="col-group">{{ task.grp ?? '–' }}</td>
                    <td class="row-actions">
                      <button class="icon-button row-act" :title="t('runNow')" @click="runNow(task)"><Play :size="17" /></button>
                      <button class="icon-button row-act" :title="t('runHistory')" @click="openRunHistory(task)"><Activity :size="17" /></button>
                      <button class="icon-button row-act" :title="t('editTask')" @click="openEditTask(task)"><Pencil :size="17" /></button>
                      <button class="icon-button row-act" :title="t('deleteTask')" @click="removeTask(task)"><Trash2 :size="17" /></button>
                      <button class="icon-button row-more" :title="t('more')" :aria-expanded="openRowMenu === task.id" @click="toggleRowMenu(task.id)"><MoreVertical :size="17" /></button>
                      <div v-if="openRowMenu === task.id" class="row-menu">
                        <button @click="openRowMenu = null; runNow(task)"><Play :size="15" />{{ t('runNow') }}</button>
                        <button @click="openRowMenu = null; openRunHistory(task)"><Activity :size="15" />{{ t('runHistory') }}</button>
                        <button @click="openRowMenu = null; openNotificationsFor([task.id])"><Bell :size="15" />{{ t('batchNotify') }}</button>
                        <button @click="openRowMenu = null; openEditTask(task)"><Pencil :size="15" />{{ t('editTask') }}</button>
                        <button class="danger" @click="openRowMenu = null; removeTask(task)"><Trash2 :size="15" />{{ t('deleteTask') }}</button>
                      </div>
                      <div v-if="openRowMenu === task.id" class="row-menu-backdrop" @click="openRowMenu = null" />
                    </td>
                  </tr>
                </template>
              </tbody>
            </table>
          </div>
          </template>
          <Pager
            class="pager-sticky"
            :page="tasksPage"
            :pages="tasksTotalPages"
            :total="filteredTasks.length"
            :page-size="tasksPageSize"
            @prev="tasksPrevPage"
            @next="tasksNextPage"
            @update:page-size="setTasksPageSize"
          />
        </section>
      </div>

      <!-- ===== TASK RUNS ===== -->
      <div v-else-if="view === 'taskRuns'" class="page">
        <section class="page-heading">
          <div><h1>{{ t('runHistory') }}</h1><p v-if="runHistoryTask">{{ runHistoryTask.name }}</p></div>
          <button class="secondary-button" @click="backToTasks"><ArrowLeft :size="16" />{{ t('back') }}</button>
        </section>
        <section class="task-section">
          <div class="toolbar">
            <span class="run-toolbar">
              <button class="icon-button" :title="t('refresh')" @click="runHistoryTask && loadTaskRuns(runHistoryTask.id)"><RefreshCw :size="18" /></button>
              <button class="icon-button" :title="t('clearRuns')" @click="clearTaskRuns"><Trash2 :size="18" /></button>
            </span>
          </div>
          <div v-if="!(runsByTask[runHistoryTask?.id ?? -1]?.length)" class="empty-state"><Activity :size="24" /><h2>{{ t('noRuns') }}</h2></div>
          <div v-else class="table-wrap">
            <table>
              <thead><tr>
                <th>{{ t('time') }}</th><th>{{ t('status') }}</th><th class="run-log-col">{{ t('log') }}</th><th>{{ t('manage') }}</th>
              </tr></thead>
              <tbody>
                <tr v-for="run in runsByTask[runHistoryTask?.id ?? -1]" :key="run.id">
                  <td class="run-time">{{ formatRunTime(run.started_at ?? run.created_at, undefined, runHistoryTask?.timezone || undefined) }}</td>
                  <td><strong :class="runStatusClass(run.status)">{{ runStatusLabel(run.status) }}</strong></td>
                  <td class="run-log-col"><span class="run-log">{{ runLogText(run) }}</span></td>
                  <td class="row-actions">
                    <button v-if="['pending','leased','running'].includes(run.status)" class="icon-button" :title="t('cancelRun')" @click="cancelRun(run)"><X :size="15" /></button>
                    <button class="icon-button" :title="t('deleteRun')" @click="removeRun(run)"><Trash2 :size="15" /></button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>
      </div>

      <!-- ===== TEMPLATES ===== -->
      <div v-else-if="view === 'templates'" class="page">
        <section class="page-heading">
          <div><h1>{{ t('templates') }}</h1><p>{{ t('templateHint') }}</p></div>
          <div class="heading-actions">
            <button class="primary-button" @click="openImportModal()"><Plus :size="17" />{{ t('importLocal') }}</button>
            <button class="secondary-button" @click="openSubModal()"><RefreshCw :size="16" />{{ t('addSub') }}</button>
          </div>
        </section>

        <!-- Where the public library below comes from: the subscription
             sources, managed up here so they sit before the lists they feed. -->
        <section class="task-section">
          <h2>{{ t('subscriptionsTitle') }}</h2>
          <p class="muted section-hint">{{ t('subHint') }}</p>
          <div v-if="subscriptions.length === 0" class="muted">{{ t('noSubs') }}</div>
          <template v-else>
          <div class="paged-anchor" ref="subscriptionsAnchor"></div>
          <div class="table-wrap">
            <table class="templates-table">
              <thead><tr>
                <th>{{ t('name') }}</th><th>{{ t('subUrl') }}</th><th>{{ t('status') }}</th>
                <th><span class="sr-only">{{ t('manage') }}</span></th>
              </tr></thead>
              <tbody>
                <tr v-for="sub in pagedSubscriptions" :key="sub.id">
                  <td class="task-name"><strong>{{ sub.name }}</strong></td>
                  <td><a class="library-row-link" :href="sub.url" target="_blank" rel="noopener noreferrer">{{ sub.url }}</a></td>
                  <td><span v-if="sub.enabled" class="chip chip-ok">{{ t('subEnabled') }}</span><span v-else class="chip">{{ t('subDisabled') }}</span></td>
                  <td class="row-actions">
                    <button class="secondary-button" @click="openSubModal(sub)"><Pencil :size="14" />{{ t('edit') }}</button>
                    <button class="icon-button" :title="sub.enabled ? t('disableSub') : t('enableSub')" @click="toggleSubscription(sub)"><PowerOff v-if="sub.enabled" :size="15" /><Power v-else :size="15" /></button>
                    <button class="icon-button danger" :title="t('deleteSub')" @click="removeSubscription(sub.id)"><Trash2 :size="16" /></button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          </template>
          <Pager
            class="pager-sticky"
            :page="subsPage"
            :pages="subsTotalPages"
            :total="subscriptions.length"
            :page-size="subsPageSize"
            @prev="subsPrevPage"
            @next="subsNextPage"
            @update:page-size="setSubsPageSize"
          />
        </section>

        <!-- 1) What is in use. One row per template, columns aligned, every
             header clickable, and the one action a reader wants (build a task)
             first. -->
        <section class="task-section">
          <div class="toolbar">
            <label class="search"><Search :size="17" /><input v-model="templateSearch" type="search" :placeholder="t('templateSearch')" /></label>
            <label class="group-filter"><input v-model="unusedOnly" type="checkbox" />{{ t('unusedOnly') }}</label>
            <span v-if="filteredTemplates.length" class="muted">{{ fmt('libraryEntryCount', { n: filteredTemplates.length }) }}</span>
          </div>
          <h2>{{ t('myTemplates') }}</h2>
          <div v-if="filteredTemplates.length === 0" class="empty-state">
            <span><FileJson2 :size="25" /></span>
            <h2>{{ unusedOnly ? t('noUnusedTemplates') : templateSearch ? t('noTemplates') : t('templateEmptyHint') }}</h2>
          </div>
          <template v-else>
          <div class="paged-anchor" ref="templatesAnchor"></div>
          <div class="table-wrap">
            <table class="templates-table">
              <thead><tr>
                <th v-for="column in templateColumns" :key="column.key" class="sortable" :aria-sort="sortAria(templateSort, column.key)">
                  <button type="button" @click="toggleSort(templateSort, column.key, column.text)">{{ column.label }}<ChevronUp v-if="sortDir(templateSort, column.key) === 'asc'" :size="13" /><ChevronDown v-else-if="sortDir(templateSort, column.key) === 'desc'" :size="13" /><ArrowUpDown v-else :size="13" class="sort-idle" /></button>
                </th>
                <th><span class="sr-only">{{ t('manage') }}</span></th>
              </tr></thead>
              <tbody>
                <tr v-for="item in pagedTemplates" :key="item.id">
                  <td class="task-name">
                    <strong>{{ item.name }}</strong>
                    <span v-if="publishedTemplateIds.has(item.id)" class="chip chip-published">{{ t('published') }}</span>
                    <span v-if="item.description" class="muted row-description">{{ item.description }}</span>
                  </td>
                  <td><span v-if="item.grp" class="chip">{{ item.grp }}</span><span v-else class="muted">—</span></td>
                  <td class="num">{{ item.variables?.length ?? 0 }}</td>
                  <td class="num">{{ item.task_count ?? 0 }}</td>
                  <td class="run-time">{{ formatRunTime(item.updated_at) }}</td>
                  <td class="row-actions">
                    <button class="primary-button" :title="t('useTemplateTitle')" @click="createTaskFromTemplate(item)"><Play :size="14" />{{ t('useTemplate') }}</button>
                    <button v-if="item.source_format === 'qd_har'" class="secondary-button" @click="openImportModal(item)"><Pencil :size="14" />{{ t('editTemplate') }}</button>
                    <button v-if="publishedTemplateIds.has(item.id)" class="secondary-button" @click="unpublishTemplate(item.id)"><Undo2 :size="14" />{{ t('unpublish') }}</button>
                    <button v-else class="secondary-button" @click="publishTemplate(item.id)"><Upload :size="14" />{{ t('publish') }}</button>
                    <button class="icon-button danger" :title="t('deleteTemplate')" @click="removeTemplate(item.id, item.name, item.task_count ?? 0)"><Trash2 :size="16" /></button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          </template>
          <Pager
            class="pager-sticky"
            :page="templatesPage"
            :pages="templatesTotalPages"
            :total="filteredTemplates.length"
            :page-size="templatesPageSize"
            @prev="templatesPrevPage"
            @next="templatesNextPage"
            @update:page-size="setTemplatesPageSize"
          />
        </section>

        <!-- 2) What can be taken: every subscribed library in one list, newest
             first, one action per row. Subscribing opens the entry for review
             and the save is what files it, so this is where the click that used
             to cost four now stops. -->
        <section class="task-section">
          <h2>{{ t('publicLibrary') }}</h2>
          <p class="muted section-hint">{{ t('publicLibraryHint') }}</p>
          <div v-if="subscriptions.length === 0" class="empty-state">
            <span><LibraryIcon :size="25" /></span>
            <h2>{{ t('publicLibraryNoSource') }}</h2>
            <button class="secondary-button" @click="openSubModal()">{{ t('addSub') }}</button>
          </div>
          <template v-else>
            <div class="toolbar library-toolbar">
              <label class="group-filter">
                <span>{{ t('librarySource') }}</span>
                <Dropdown :model-value="librarySourceId" :options="librarySourceOptions" compact @change="selectLibrarySource($event as number | 'all')" />
              </label>
              <span v-if="librarySourceKind" class="chip">{{ librarySourceKind }}</span>
              <button class="icon-button" :title="t('refresh')" :disabled="libraryLoading" @click="loadLibrary(true)"><RefreshCw :class="{ spin: libraryLoading }" :size="18" /></button>
              <input v-model="librarySearch" class="library-search" type="search" :placeholder="t('librarySearchPlaceholder')" />
              <span class="muted">{{ fmt('libraryEntryCount', { n: libraryEntries.length }) }}</span>
              <button class="text-button" :aria-expanded="libraryBatch" @click="libraryBatch = !libraryBatch">{{ libraryBatch ? t('libraryBatchOff') : t('libraryBatchOn') }}</button>
            </div>

            <!-- Bulk import, kept but demoted: a source in "import all" mode
                 syncs itself, so ticking entries is only for pre-loading a few. -->
            <div v-if="libraryBatch" class="batch-bar">
              <span>{{ fmt('librarySelected', { n: librarySelectedCount }) }}</span>
              <button class="secondary-button" :disabled="pagedLibraryEntries.length === 0" @click="toggleLibraryAllVisible">{{ t('librarySelectAll') }}</button>
              <button class="secondary-button" :disabled="librarySelectedCount === 0" @click="clearLibrarySelection">{{ t('libraryClear') }}</button>
              <button class="primary-button" :disabled="libraryImporting || librarySelectedCount === 0" @click="importSelectedLibraryEntries">
                <Download :size="16" />{{ libraryImporting ? t('libraryImporting') : t('libraryImport') }}
              </button>
            </div>

            <!-- One dead repository must not read as an empty library, so the
                 sources that failed say so next to the entries that arrived. -->
            <div v-if="librarySourceFaults.length" class="source-faults">
              <span class="muted">{{ t('librarySourceErrors') }}</span>
              <span v-for="fault in librarySourceFaults" :key="fault.subscription_id" class="error-text">
                <strong>{{ fault.name }}</strong> · {{ fault.error }}
              </span>
            </div>
            <p v-if="libraryTruncated" class="muted section-hint">{{ t('libraryTruncated') }}</p>

            <div v-if="libraryLoading" class="loading-state"><RefreshCw class="spin" :size="22" />{{ t('libraryLoading') }}</div>
            <div v-else-if="libraryEntries.length === 0" class="empty-state">
              <span><LibraryIcon :size="25" /></span>
              <h2>{{ t('libraryEmpty') }}</h2>
            </div>
            <template v-else>
            <div class="paged-anchor" ref="libraryAnchor"></div>
            <div class="table-wrap">
              <table class="library-table">
                <thead><tr>
                  <th v-if="libraryBatch" class="col-check"><input type="checkbox" :checked="libraryAllVisibleSelected" :aria-label="t('librarySelectAll')" @change="toggleLibraryAllVisible" /></th>
                  <th v-for="column in libraryColumns" :key="column.key" class="sortable" :aria-sort="sortAria(librarySort, column.key)">
                    <button type="button" @click="toggleSort(librarySort, column.key, column.text)">{{ column.label }}<ChevronUp v-if="sortDir(librarySort, column.key) === 'asc'" :size="13" /><ChevronDown v-else-if="sortDir(librarySort, column.key) === 'desc'" :size="13" /><ArrowUpDown v-else :size="13" class="sort-idle" /></button>
                  </th>
                  <th>{{ t('status') }}</th>
                  <th><span class="sr-only">{{ t('manage') }}</span></th>
                </tr></thead>
                <tbody>
                  <tr v-for="entry in pagedLibraryEntries" :key="libraryKey(entry)">
                    <td v-if="libraryBatch" class="col-check"><input type="checkbox" :checked="librarySelected.has(libraryKey(entry))" :aria-label="entry.name" @change="toggleLibraryEntry(entry)" /></td>
                    <td class="task-name">
                      <strong>{{ entry.name }}</strong>
                      <a v-if="entry.comment_url" class="library-row-link" :href="entry.comment_url" target="_blank" rel="noopener noreferrer">{{ t('libraryOpenIssue') }}</a>
                      <span v-if="entry.comments" class="muted row-description" :title="t('libraryComments')">{{ entry.comments }}</span>
                    </td>
                    <td>{{ entry.author ?? '—' }}</td>
                    <td v-if="libraryViewingAll" class="library-source-cell">{{ entry.source_name ?? '—' }}</td>
                    <td class="num">{{ entry.version ?? '—' }}</td>
                    <td class="run-time">{{ entry.date ?? '—' }}</td>
                    <td><span v-if="entry.update_available" class="chip chip-warn">{{ t('libraryUpdates') }}</span><span v-else-if="entry.installed" class="chip chip-ok">{{ t('libraryInstalled') }}</span><span v-else class="muted">—</span></td>
                    <td class="row-actions">
                      <button v-if="libraryBusyKey === libraryKey(entry)" class="primary-button" disabled><Loader2 class="spin" :size="14" />{{ t('libraryFetching') }}</button>
                      <button v-else-if="!entry.installed" class="primary-button" :title="t('librarySubscribeHint')" @click="previewLibraryEntry(entry)"><Plus :size="14" />{{ t('librarySubscribe') }}</button>
                      <button v-else-if="entry.update_available" class="primary-button" :title="t('librarySubscribeHint')" @click="previewLibraryEntry(entry)"><Download :size="14" />{{ t('libraryUpdate') }}</button>
                      <button v-else class="secondary-button" @click="openLibraryEntry(entry)"><Pencil :size="14" />{{ t('openTemplate') }}</button>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>

            </template>
            <Pager
              class="pager-sticky"
              :page="libraryPage"
              :pages="libraryTotalPages"
              :total="libraryEntries.length"
              :page-size="libraryPageSize"
              @prev="libraryPrevPage"
              @next="libraryNextPage"
              @update:page-size="setLibraryPageSize"
            />

            <template v-if="libraryFailures.length">
              <h2>{{ t('libraryFailedTitle') }}</h2>
              <div v-for="failure in libraryFailures" :key="failure.name" class="run-row">
                <strong>{{ failure.name }}</strong><span class="error-text">{{ failure.error }}</span>
              </div>
            </template>
          </template>
        </section>

        <!-- 3) Templates other people published to this instance. Not the
             "public templates" of QD's vocabulary, hence the separate name. -->
        <section class="task-section">
          <div class="toolbar">
            <h2>{{ t('publicTemplates') }}</h2>
            <button class="secondary-button" :aria-expanded="showPublicTemplates" @click="showPublicTemplates = !showPublicTemplates">{{ showPublicTemplates ? t('collapse') : t('expand') }}</button>
          </div>
          <template v-if="showPublicTemplates">
            <div v-if="sortedPublicTemplates.length === 0" class="muted">{{ t('noTemplates') }}</div>
            <template v-else>
            <div class="paged-anchor" ref="publicTemplatesAnchor"></div>
            <div class="table-wrap">
              <table class="templates-table">
                <thead><tr>
                  <th class="sortable" :aria-sort="sortAria(publicSort, 'name')">
                    <button type="button" @click="toggleSort(publicSort, 'name', true)">{{ t('name') }}<ChevronUp v-if="sortDir(publicSort, 'name') === 'asc'" :size="13" /><ChevronDown v-else-if="sortDir(publicSort, 'name') === 'desc'" :size="13" /><ArrowUpDown v-else :size="13" class="sort-idle" /></button>
                  </th>
                  <th>{{ t('description') }}</th>
                  <th class="sortable" :aria-sort="sortAria(publicSort, 'updated_at')">
                    <button type="button" @click="toggleSort(publicSort, 'updated_at')">{{ t('updatedAt') }}<ChevronUp v-if="sortDir(publicSort, 'updated_at') === 'asc'" :size="13" /><ChevronDown v-else-if="sortDir(publicSort, 'updated_at') === 'desc'" :size="13" /><ArrowUpDown v-else :size="13" class="sort-idle" /></button>
                  </th>
                  <th><span class="sr-only">{{ t('manage') }}</span></th>
                </tr></thead>
                <tbody>
                  <tr v-for="item in pagedPublicTemplates" :key="item.id">
                    <td class="task-name"><strong>{{ item.name }}</strong></td>
                    <td class="muted row-description">{{ item.description ?? '—' }}</td>
                    <td class="run-time">{{ formatRunTime(item.updated_at) }}</td>
                    <td class="row-actions"><button class="secondary-button" @click="copyTemplate(item.id)"><Copy :size="14" />{{ t('copy') }}</button></td>
                  </tr>
                </tbody>
              </table>
            </div>
            </template>
            <Pager
              class="pager-sticky"
              :page="publicTemplatesPage"
              :pages="publicTemplatesTotalPages"
              :total="sortedPublicTemplates.length"
              :page-size="publicTemplatesPageSize"
              @prev="publicTemplatesPrevPage"
              @next="publicTemplatesNextPage"
              @update:page-size="setPublicTemplatesPageSize"
            />
          </template>
        </section>
      </div>

      <!-- ===== PLUGINS ===== -->
      <div v-else-if="view === 'plugins'" class="page">
        <section class="page-heading"><div><h1>{{ t('pluginsTitle') }}</h1></div></section>
        <section class="task-section">
          <form class="modal inline-modal" @submit.prevent="savePlugin">
            <label>{{ t('pluginName') }}<input v-model="pluginForm.name" required /></label>
            <label>{{ t('command') }}<input v-model="pluginForm.command" required /></label>
            <button class="primary-button">{{ t('registerPlugin') }}</button>
          </form>
          <div class="run-row invoke-row">
            <label>{{ t('action') }}<input v-model="invokeForm.action" /></label>
            <label>{{ t('queryJson') }}<input v-model="invokeForm.query" /></label>
          </div>
          <div v-if="plugins.length === 0" class="muted">{{ t('noPlugins') }}</div>
          <div v-for="plugin in plugins" :key="plugin.id" class="run-row">
            <strong>{{ plugin.name }}</strong>
            <code>{{ plugin.command }}</code>
            <button v-if="plugin.enabled" class="secondary-button" @click="invokePlugin(plugin)">{{ t('invoke') }}</button>
            <button class="secondary-button" @click="togglePlugin(plugin)">{{ plugin.enabled ? t('disable') : t('enable') }}</button>
            <button class="icon-button" :title="t('deletePlugin')" @click="removePlugin(plugin.id, plugin.name)"><Trash2 :size="16" /></button>
          </div>
          <pre v-if="pluginResult" class="result-pre"><code>{{ pluginResult }}</code></pre>
        </section>
      </div>

      <!-- ===== NOTIFICATIONS ===== -->
      <div v-else-if="view === 'notifications'" class="page">
        <section class="page-heading"><div><h1>{{ t('notificationsTitle') }}</h1><p>{{ t('notificationsHint') }}</p></div></section>
        <section class="task-section">
          <form ref="channelFormEl" class="modal inline-modal" @submit.prevent="saveChannel">
            <label>{{ t('channelName') }}<input v-model="channelForm.name" required /></label>
            <label>{{ t('channelKind') }}
              <Dropdown v-model="channelForm.kind" :options="channelKindDropdownOptions" :disabled="channelForm.id != null" />
              <small v-if="channelForm.id != null" class="kv-hint">{{ t('channelKindLocked') }}</small>
            </label>
            <template v-if="channelForm.kind === 'webhook'">
              <label>{{ t('webhookUrl') }}<input v-model="channelForm.url" required type="url" placeholder="https://example.com/hook" /></label>
            </template>
            <template v-else-if="channelForm.kind === 'custom_http'">
              <label>{{ t('customHttpUrl') }}<input v-model="channelForm.url" required type="url" placeholder="https://example.com/hook" /></label>
              <label>{{ t('customHttpMethod') }}<input v-model="channelForm.customMethod" required /></label>
              <label>{{ t('customHttpHeaders') }}<textarea v-model="channelForm.customHeaders" rows="3" spellcheck="false" /></label>
              <label>{{ t('customHttpBodyTemplate') }}<textarea v-model="channelForm.customBody" rows="3" spellcheck="false" placeholder="{task} {event} {error}" /></label>
              <small class="kv-hint">{{ t('customHttpHint') }}</small>
            </template>
            <template v-else-if="channelForm.kind === 'email'">
              <label>{{ t('emailTo') }}<input v-model="channelForm.to" required type="email" /></label>
              <label>{{ t('emailSubject') }}<input v-model="channelForm.subject" /></label>
              <small class="kv-hint">{{ t('emailFromHint') }}</small>
            </template>
            <template v-else-if="channelForm.kind === 'bark'">
              <label>{{ t('barkUrl') }}<input v-model="channelForm.url" required type="url" placeholder="https://api.day.app/yourkey" /></label>
              <label>{{ t('barkSound') }}<input v-model="channelForm.sound" placeholder="minuet" /></label>
              <label>{{ t('barkGroup') }}<input v-model="channelForm.group" placeholder="qdrust" /></label>
            </template>
            <template v-else-if="channelForm.kind === 'serverchan'">
              <label>{{ t('serverchanKey') }}<input v-model="channelForm.sendkey" required placeholder="SCT..." /></label>
            </template>
            <template v-else-if="channelForm.kind === 'telegram'">
              <label>{{ t('telegramToken') }}<input v-model="channelForm.tgToken" required placeholder="123456:ABC-DEF..." /></label>
              <label>{{ t('telegramChatId') }}<input v-model="channelForm.tgChatId" required placeholder="123456789" /></label>
              <label>{{ t('telegramHost') }}<input v-model="channelForm.tgHost" placeholder="https://tg.example.com/" /></label>
            </template>
            <template v-else-if="channelForm.kind === 'dingtalk'">
              <label>{{ t('dingtalkToken') }}<input v-model="channelForm.dingToken" required /></label>
            </template>
            <template v-else-if="channelForm.kind === 'wxpusher'">
              <label>{{ t('wxpusherToken') }}<input v-model="channelForm.wxToken" required /></label>
              <label>{{ t('wxpusherUid') }}<input v-model="channelForm.wxUid" required /></label>
            </template>
            <template v-else-if="channelForm.kind === 'wxpusher_spt'">
              <label>{{ t('wxpusherSpt') }}<input v-model="channelForm.spt" required /></label>
            </template>
            <template v-else-if="channelForm.kind === 'wecom_app'">
              <label>{{ t('wecomCorpId') }}<input v-model="channelForm.corpId" required /></label>
              <label>{{ t('wecomAgentId') }}<input v-model="channelForm.agentId" required /></label>
              <label>{{ t('wecomSecret') }}<input v-model="channelForm.secret" required /></label>
              <label>{{ t('wecomToUser') }}<input v-model="channelForm.toUser" placeholder="@all" /></label>
            </template>
            <template v-else-if="channelForm.kind === 'wecom_webhook'">
              <label>{{ t('wecomWebhookKey') }}<input v-model="channelForm.wecomKey" required /></label>
            </template>
            <div class="inline-actions">
              <button class="primary-button">{{ channelForm.id != null ? t('save') : t('createChannel') }}</button>
              <button v-if="channelForm.id != null" type="button" class="secondary-button" @click="cancelChannelEdit">{{ t('cancel') }}</button>
            </div>
          </form>
          <div v-if="channels.length === 0" class="muted">{{ t('noChannels') }}</div>
          <div v-for="channel in channels" :key="channel.id" class="run-row">
            <strong>{{ channel.name }}</strong>
            <span class="chip">{{ channelKindLabel(channel.kind) }}</span>
            <button class="secondary-button" :disabled="testingChannelId === channel.id" @click="testChannel(channel)"><Send :size="15" />{{ testingChannelId === channel.id ? t('testingChannel') : t('testChannel') }}</button>
            <button class="secondary-button" @click="toggleChannel(channel)">{{ channel.enabled ? t('disable') : t('enable') }}</button>
            <button class="secondary-button" @click="openEditChannel(channel)"><Pencil :size="15" />{{ t('edit') }}</button>
            <button class="icon-button" :title="t('deleteChannel')" @click="removeChannel(channel)"><Trash2 :size="16" /></button>
          </div>
          <h2>{{ t('taskActions') }}</h2>
          <form class="modal inline-modal" @submit.prevent="saveAction">
            <div class="action-tasks field-wide">
              <div class="action-tasks-head">
                <span class="action-tasks-label">{{ t('notifyBatchTasks') }}</span>
                <span v-if="actionForm.taskIds.length" class="chip">{{ fmt('selectedCount', { n: actionForm.taskIds.length }) }}</span>
                <label class="search action-tasks-filter"><Search :size="15" /><input v-model="actionTaskFilter" type="search" :placeholder="t('notifyFilterTasks')" /></label>
                <button type="button" class="text-button" @click="toggleAllActionTasks">{{ allActionTasksChecked ? t('notifyClearSelection') : t('selectAll') }}</button>
              </div>
              <div class="check-list" role="group" :aria-label="t('notifyBatchTasks')">
                <label v-for="task in actionTasks" :key="task.id" class="check-row">
                  <input type="checkbox" :checked="actionForm.taskIds.includes(task.id)" @change="toggleActionTask(task.id)" />
                  <span class="check-row-name">{{ task.name }}</span>
                  <code v-if="task.url">{{ task.url }}</code>
                </label>
                <div v-if="actionTasks.length === 0" class="muted">{{ t('noTasksMatch') }}</div>
              </div>
            </div>
            <label>{{ t('channel') }}
              <Dropdown v-model="actionForm.channelId" :options="actionChannelDropdownOptions" />
            </label>
            <label>{{ t('event') }}
              <Dropdown v-model="actionForm.event" :options="eventDropdownOptions" />
            </label>
            <label>{{ t('notifyFailureThreshold') }}<input v-model="actionForm.failureThreshold" type="number" min="1" /></label>
            <label class="checkbox"><input v-model="actionForm.automaticOnly" type="checkbox" />{{ t('notifyAutomaticOnly') }}</label>
            <label>{{ t('notifyTitleTemplate') }}<textarea v-model="actionForm.titleTemplate" rows="2" spellcheck="false" placeholder="{task} {event}" /></label>
            <label>{{ t('notifyBodyTemplate') }}<textarea v-model="actionForm.bodyTemplate" rows="2" spellcheck="false" placeholder="{log} {error}" /></label>
            <small class="kv-hint field-wide">{{ t('notifyVarsHint') }}</small>
            <button class="primary-button">{{ t('addAction') }}</button>
          </form>
          <div v-if="actions.length === 0" class="muted">{{ t('noActions') }}</div>
          <div v-for="action in actions" :key="action.id" class="run-row">
            <template v-if="editingActionId === action.id">
              <!-- The row becomes its own editor: a binding is too small an
                   object to justify a second form further up the page. -->
              <form class="modal inline-modal action-edit" @submit.prevent="saveActionEdit">
                <label>{{ t('channel') }}
                  <Dropdown v-model="actionEdit.channelId" :options="actionChannelDropdownOptions" />
                </label>
                <label>{{ t('event') }}
                  <Dropdown v-model="actionEdit.event" :options="eventDropdownOptions" />
                </label>
                <label>{{ t('notifyFailureThreshold') }}<input v-model="actionEdit.failureThreshold" type="number" min="1" /></label>
                <label class="checkbox"><input v-model="actionEdit.automaticOnly" type="checkbox" />{{ t('notifyAutomaticOnly') }}</label>
                <label class="field-wide">{{ t('notifyTitleTemplate') }}<textarea v-model="actionEdit.titleTemplate" rows="2" spellcheck="false" placeholder="{task} {event}" /></label>
                <label class="field-wide">{{ t('notifyBodyTemplate') }}<textarea v-model="actionEdit.bodyTemplate" rows="2" spellcheck="false" placeholder="{log} {error}" /></label>
                <div class="inline-actions">
                  <button class="primary-button">{{ t('save') }}</button>
                  <button type="button" class="secondary-button" @click="cancelEditAction">{{ t('cancel') }}</button>
                </div>
              </form>
            </template>
            <template v-else>
              <strong>{{ action.event === 'success' ? t('eventSuccess') : action.event === 'failure' ? t('eventFailure') : t('eventAlways') }}</strong>
              <span>{{ t('channel') }}: {{ channelName(action.channel_id) }}</span>
              <span>{{ t('task') }}: {{ taskName(action.task_id) }}</span>
              <span v-if="action.failure_threshold > 1" class="chip" :title="t('notifyFailureThreshold')">&ge;{{ action.failure_threshold }}</span>
              <span v-if="action.automatic_only" class="chip">{{ t('notifyAutomaticOnlyShort') }}</span>
              <button class="secondary-button" @click="startEditAction(action)"><Pencil :size="15" />{{ t('edit') }}</button>
              <button class="icon-button" :title="t('deleteAction')" @click="removeAction(action.id)"><Trash2 :size="16" /></button>
            </template>
          </div>
        </section>
      </div>

      <!-- ===== PUSH ===== -->
      <div v-else-if="view === 'push'" class="page">
        <section class="page-heading"><div><h1>{{ t('pushTitle') }}</h1><p>{{ t('pushHint') }}</p></div></section>
        <section class="task-section">
          <h2>{{ t('myRequests') }}</h2>
          <form class="modal inline-modal" @submit.prevent="submitPush">
            <label>{{ t('pushTemplate') }}
              <Dropdown v-model="pushTemplateId" :options="pushTemplateDropdownOptions" />
            </label>
            <label>{{ t('pushNote') }}<textarea v-model="pushNote" rows="3" /></label>
            <button class="primary-button">{{ t('submitPush') }}</button>
          </form>
          <div v-if="myPushRequests.length === 0" class="muted">{{ t('noRequests') }}</div>
          <div v-for="r in myPushRequests" :key="r.id" class="run-row">
            <strong>{{ t('pushTemplate') }} #{{ r.template_id }}</strong>
            <span class="chip">{{ pushStatusLabel(r.status) }}</span>
            <span v-if="r.note" class="muted">{{ r.note }}</span>
          </div>
          <template v-if="isAdmin">
            <h2>{{ t('pending') }}</h2>
            <div v-if="pendingPushRequests.length === 0" class="muted">{{ t('noRequests') }}</div>
            <div v-for="r in pendingPushRequests" :key="r.id" class="run-row">
              <strong>{{ t('pushTemplate') }} #{{ r.template_id }}</strong>
              <span>{{ t('requester') }} #{{ r.owner_id }}</span>
              <span v-if="r.note" class="muted">{{ r.note }}</span>
              <button class="secondary-button" @click="decidePush(r.id, true)"><CheckCircle2 :size="15" />{{ t('approve') }}</button>
              <button class="secondary-button danger" @click="decidePush(r.id, false)"><XCircle :size="15" />{{ t('reject') }}</button>
            </div>
          </template>
        </section>
      </div>

      <!-- ===== ADMIN ===== -->
      <div v-else-if="view === 'admin'" class="page">
        <section class="page-heading"><div><h1>{{ t('adminTitle') }}</h1><p>{{ t('adminHint') }}</p></div></section>
        <section class="task-section">
          <h2>{{ t('users') }}</h2>
          <div v-for="user in adminUsers" :key="user.id" class="run-row">
            <strong>{{ user.username }}</strong>
            <span class="chip">{{ user.role === 'admin' ? t('roleAdmin') : t('roleUser') }}</span>
            <Dropdown v-if="user.id !== currentUser?.id" :model-value="user.role" :options="roleDropdownOptions" compact @change="(v) => changeUserRole(user, String(v))" />
            <span v-if="user.email">{{ user.email }}</span>
            <span :class="user.email_verified ? 'ok-text' : 'muted'">{{ user.email_verified ? t('verified') : t('unverified') }}</span>
            <span class="run-time">{{ formatRunTime(user.created_at) }}</span>
            <button v-if="user.id !== currentUser?.id" class="secondary-button" @click="toggleUser(user)">{{ user.disabled ? t('enableUser') : t('disableUser') }}</button>
            <button v-if="user.id !== currentUser?.id" class="secondary-button danger" @click="deleteUser(user)"><Trash2 :size="14" />{{ t('deleteUser') }}</button>
          </div>

          <h2>{{ t('siteSettings') }}</h2>
          <form class="modal inline-modal" @submit.prevent="saveAdminSettings">
            <div class="settings-group">
              <label class="checkbox"><input v-model="settingsForm.requireEmail" type="checkbox" />{{ t('requireEmailVerify') }}</label>
              <label class="checkbox">
                <input :checked="settingsForm.allowPrivateNetwork" type="checkbox" @change="onRiskToggle('allowPrivateNetwork', $event)" />
                {{ t('allowPrivateNetwork') }}
                <span v-if="settingsForm.allowPrivateNetwork" class="chip chip-warn">{{ t('highRisk') }}</span>
              </label>
              <label class="checkbox">
                <input :checked="settingsForm.allowInvalidCertificates" type="checkbox" @change="onRiskToggle('allowInvalidCertificates', $event)" />
                {{ t('allowInvalidCertificates') }}
                <span v-if="settingsForm.allowInvalidCertificates" class="chip chip-warn">{{ t('highRisk') }}</span>
              </label>
            </div>
            <div class="settings-group settings-group-pair">
              <label>{{ t('gaKey') }}<input v-model="settingsForm.gaKey" placeholder="G-XXXXXXX" /></label>
              <label>{{ t('retentionDays') }}<input v-model.number="settingsForm.retentionDays" type="number" min="0" /></label>
            </div>
            <div class="inline-actions">
              <button class="primary-button">{{ t('saveSettings') }}</button>
              <button class="secondary-button" type="button" @click="cleanupLogs">{{ t('cleanupLogs') }}</button>
            </div>
          </form>

          <h2>{{ t('backup') }}</h2>
          <div class="run-row">
            <button class="secondary-button" @click="downloadBackup">{{ t('exportBackup') }}</button>
            <label class="secondary-button file-button">{{ t('importRestore') }}
              <input type="file" accept="application/json" style="display:none" @change="onRestoreFile" />
            </label>
          </div>
        </section>
      </div>

      <!-- ===== SETTINGS ===== -->
      <div v-else class="page">
        <section class="page-heading"><div><h1>{{ t('settingsTitle') }}</h1></div></section>
        <section class="settings-grid">
          <div class="content-panel">
            <h2>{{ t('account') }}</h2>
            <dl class="settings-list">
              <div><dt>{{ t('username') }}</dt><dd>{{ currentUser?.username }}</dd></div>
              <div><dt>{{ t('role') }}</dt><dd>{{ currentUser?.role === 'admin' ? t('roleAdmin') : t('roleUser') }}</dd></div>
              <div><dt>{{ t('emailVerified') }}</dt><dd>{{ currentUser?.email_verified ? t('verified') : t('unverified') }}</dd></div>
            </dl>
            <form class="settings-form" @submit.prevent="changePassword">
              <h3>{{ t('changePassword') }}</h3>
              <label>{{ t('currentPassword') }}<input v-model="pwdForm.current" required type="password" /></label>
              <label>{{ t('newPassword') }}<input v-model="pwdForm.next" required minlength="12" type="password" /></label>
              <div class="inline-actions">
                <button class="primary-button">{{ t('save') }}</button>
                <button class="secondary-button" type="button" @click="rotateCsrf">{{ t('csrfRotate') }}</button>
              </div>
            </form>
          </div>
          <div class="content-panel">
            <h2>{{ t('service') }}</h2>
            <dl class="settings-list">
              <div><dt>{{ t('statusLabel') }}</dt><dd><span class="state-dot online" />{{ ready ? t('serviceOk') : t('connecting') }}</dd></div>
              <div><dt>{{ t('localeLabel') }}</dt><dd><button class="secondary-button" @click="toggleLocale">{{ locale === 'zh-CN' ? 'EN' : '中' }}</button></dd></div>
              <div><dt>{{ t('apiDocs') }}</dt><dd><a :href="apiPath('/api/v1/openapi.json')" target="_blank">{{ t('openapi') }}</a></dd></div>
            </dl>
          </div>
        </section>
      </div>
      </Transition>
    </main>

    <!-- ===== CREATE / EDIT TASK MODAL ===== -->
    <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
      <form class="modal" @submit.prevent="submitTask">
        <div class="modal-header">
          <div><h2>{{ taskForm.id ? t('editTask') : t('newTask') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="showCreate = false"><X :size="20" /></button>
        </div>
        <label>{{ t('taskName') }}<input v-model="taskForm.name" required maxlength="100" /></label>
        <label>{{ t('template') }}
          <Dropdown v-model="taskForm.templateId" :options="templateDropdownOptions" :placeholder="t('selectTemplate')" filterable :filter-placeholder="t('templateFilterPlaceholder')" :filter-empty-label="t('filterNoMatch')" @change="onTemplatePicked" />
        </label>
        <small v-if="taskForm.templateId == null && taskForm.id == null" class="kv-hint">{{ t('templateRequiredHint') }}</small>
        <div class="schedule-head">
          <span>{{ t('scheduleMode') }}</span>
          <div class="seg" role="group">
            <button
              type="button"
              :class="{ active: !taskForm.scheduleAdvanced }"
              :aria-pressed="!taskForm.scheduleAdvanced"
              @click="taskForm.scheduleAdvanced = false"
            >{{ t('scheduleVisual') }}</button>
            <button
              type="button"
              :class="{ active: taskForm.scheduleAdvanced }"
              :aria-pressed="taskForm.scheduleAdvanced"
              @click="taskForm.scheduleAdvanced = true"
            >{{ t('scheduleCronMode') }}</button>
          </div>
        </div>
        <div class="form-row schedule-fields">
          <template v-if="!taskForm.scheduleAdvanced">
            <label>{{ t('scheduleEveryDays') }}<input v-model="taskForm.scheduleDays" type="number" min="1" max="366" /></label>
            <label>{{ t('scheduleTime') }}<input v-model="taskForm.scheduleTime" type="time" step="1" required /></label>
          </template>
          <label v-else class="field-full">{{ t('cron') }}<input v-model="taskForm.cron" required placeholder="0 0 8 * * * *" /></label>
        </div>
        <div class="form-row schedule-fields">
          <label>{{ t('randomDelayMax') }}<input v-model="taskForm.randomDelay" type="number" min="0" max="604800" placeholder="0" /></label>
        </div>
        <small class="kv-hint">{{ taskForm.scheduleAdvanced ? t('scheduleCronHint') : t('scheduleHint') }}</small>
        <label>{{ t('group') }}<input v-model="taskForm.grp" list="grp-options" :placeholder="t('group')" /></label>
        <datalist id="grp-options"><option v-for="g in taskGroups" :key="g" :value="g" /></datalist>
        <div class="form-row form-row-4">
          <label :title="t('timeoutSecondsHint')">{{ t('timeoutSeconds') }}<input v-model="taskForm.timeoutSeconds" type="number" min="1" placeholder="30" /></label>
          <label :title="t('retryCountHint')">{{ t('retryCount') }}<input v-model="taskForm.retryCount" type="number" placeholder="0" /></label>
          <label :title="t('retryIntervalHint')">{{ t('retryInterval') }}<input v-model="taskForm.retryInterval" type="number" min="1" placeholder="60" /></label>
          <label :title="t('priorityHint')">{{ t('priority') }}<input v-model="taskForm.priority" type="number" placeholder="0" /></label>
        </div>
        <small class="kv-hint">{{ t('taskNumericHint') }}</small>
        <label :title="t('timezoneHint')">{{ t('timezone') }}
          <Dropdown v-model="taskForm.timezone" :options="timezoneDropdownOptions" />
        </label>
        <label class="kv-label">{{ t('variables') }}
          <span class="kv-rows">
            <span v-for="(row, i) in taskForm.variables" :key="i" class="kv-row">
              <span v-if="row.name === 'username' || row.name === 'password'" class="credential-label">{{ row.name === 'username' ? t('username') : t('password') }}</span>
              <input v-else v-model="row.name" :placeholder="t('variableName')" />
              <input v-model="row.value" :type="row.name === 'password' ? 'password' : 'text'" :placeholder="row.name === 'username' ? t('username') : row.name === 'password' ? t('password') : t('variableValue')" :autocomplete="row.name === 'password' ? 'current-password' : 'off'" />
              <button class="icon-button" type="button" :title="t('delete')" @click="removeVariableRow(i)"><X :size="14" /></button>
            </span>
            <button class="secondary-button kv-add" type="button" @click="addVariableRow"><Plus :size="14" />{{ t('addVariable') }}</button>
          </span>
          <small class="kv-hint">{{ taskForm.templateId ? t('templateVarsHint') : t('variablesHint') }}</small>
        </label>
        <label class="checkbox"><input v-model="taskForm.disabled" type="checkbox" />{{ t('createPaused') }}</label>
        <div v-if="testing || testResult" class="template-test">
          <div class="template-test-head">
            <strong>{{ t('templateTestResult') }}</strong>
            <span v-if="testing" class="muted">{{ t('templateTestRunning') }}</span>
            <span v-else-if="testResult?.log" class="muted">{{ testResult.log }}</span>
          </div>
          <ol v-if="testResult?.steps?.length" class="template-test-steps">
            <li v-for="step in testResult.steps" :key="step.index">
              <span class="chip" :class="{ 'chip-ok': step.status < 400 }">{{ step.status }}</span>
              <code>{{ step.url }}</code>
              <span class="muted">{{ step.body_size }} B</span>
            </li>
          </ol>
          <p v-else-if="testResult" class="muted">{{ t('templateTestNoSteps') }}</p>
        </div>
        <div class="modal-actions">
          <button v-if="taskForm.templateId != null" class="secondary-button" type="button" :disabled="testing" @click="runTemplateTest"><Play :size="14" />{{ testing ? t('templateTestRunning') : t('templateTest') }}</button>
          <button class="secondary-button" type="button" @click="showCreate = false">{{ t('cancel') }}</button>
          <button class="primary-button" type="submit">{{ taskForm.id ? t('saveTask') : t('createTask') }}</button>
        </div>
      </form>
    </div>

    <!-- ===== IMPORT / EDIT HAR MODAL ===== -->
    <div v-if="showImport" class="modal-backdrop modal-backdrop-wide" @click.self="closeHarEditor()">
      <div class="modal modal-har">
        <div class="modal-header">
          <div><h2>{{ libraryPreview ? t('libraryPreviewTitle') : t('importHarTitle') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="closeHarEditor"><X :size="20" /></button>
        </div>
        <!-- Opened from a library row: say plainly that nothing has been saved
             yet, because the old flow imported on click and this one does not. -->
        <p v-if="libraryPreview" class="muted section-hint">{{ t('libraryPreviewHint') }}</p>
        <div class="har-meta">
          <label>{{ t('templateName') }}<input v-model="importForm.name" required placeholder="my-template" /></label>
          <label>{{ t('description') }}<input v-model="importForm.description" /></label>
          <label class="secondary-button har-file-pick" :title="t('harChooseFile')">
            <FileUp :size="15" />{{ t('harChooseFile') }}
            <input type="file" accept=".har,.json,application/json" @change="onHarFile" />
          </label>
        </div>
        <HarEditor :model-value="harEditorDoc" @save="saveHar" @cancel="closeHarEditor" />
      </div>
    </div>

    <!-- ===== SUBSCRIPTION MODAL ===== -->
    <div v-if="showSubModal" class="modal-backdrop" @click.self="closeSubModal">
      <div class="modal">
        <div class="modal-header">
          <div><h2>{{ subEditing ? t('editSub') : t('addSub') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="closeSubModal"><X :size="20" /></button>
        </div>
        <form @submit.prevent="saveSubModal">
          <div class="har-meta">
            <label>{{ t('subName') }}<input v-model="subForm.name" required /></label>
            <label>{{ t('subUrl') }}<input v-model="subForm.url" required type="url" placeholder="https://github.com/qd-today/templates" /></label>
          </div>
          <div class="modal-actions">
            <button class="secondary-button" type="button" @click="closeSubModal">{{ t('close') }}</button>
            <button class="primary-button" type="submit">{{ t('save') }}</button>
          </div>
        </form>
      </div>
    </div>

    <!-- ===== RUN LOG MODAL ===== -->
    <!-- A list, not a form: the table is what the dialog is for and everything
         around it is chrome, so the table is the one thing allowed to scroll.
         It used to take the shared `.modal-har`, whose height cap no child could
         reach — so the *dialog* scrolled, and a 50-row page carried the filters,
         the column headers and the pager off the screen. Title, hint and filter
         row are one header block rather than three stacked ones: the hint
         explains the title above it rather than the list below it, and the
         shared `.section-hint` margins drew a gutter between lines that read as
         a single heading. -->
    <div v-if="showRunLog" class="modal-backdrop modal-backdrop-wide" @click.self="showRunLog = false">
      <div class="modal modal-runlog">
        <div class="modal-header">
          <div>
            <h2>{{ t('runLogTitle') }}</h2>
            <p class="muted run-log-hint">{{ t('runLogHint') }}</p>
          </div>
          <button class="icon-button" type="button" :title="t('close')" @click="showRunLog = false"><X :size="20" /></button>
        </div>
        <div class="toolbar">
          <div class="seg" role="group" :aria-label="t('status')">
            <button
              v-for="option in runLogStatusOptions"
              :key="option.value"
              type="button"
              :class="{ active: runLogStatus === option.value }"
              @click="setRunLogStatus(option.value)"
            >{{ option.label }}</button>
          </div>
          <label class="group-filter">
            <span>{{ t('runLogTask') }}</span>
            <Dropdown v-model="runLogTaskId" :options="runLogTaskDropdownOptions" compact />
          </label>
          <button class="icon-button run-log-refresh" :title="t('refresh')" @click="loadRunLog()"><RefreshCw :class="{ spin: runLogLoading }" :size="18" /></button>
        </div>
        <div v-if="runLogLoading && allRuns.length === 0" class="loading-state"><RefreshCw class="spin" :size="22" />{{ t('loading') }}</div>
        <div v-else-if="allRuns.length === 0" class="empty-state">
          <span><Activity :size="25" /></span>
          <h2>{{ t('runLogEmpty') }}</h2>
        </div>
        <template v-else>
        <div class="paged-anchor" ref="runLogAnchor"></div>
        <div class="table-wrap">
          <table class="runs-table">
            <thead><tr>
              <th>{{ t('time') }}</th><th>{{ t('task') }}</th><th>{{ t('status') }}</th><th class="run-log-col">{{ t('log') }}</th><th><span class="sr-only">{{ t('manage') }}</span></th>
            </tr></thead>
            <tbody>
              <tr v-for="run in allRuns" :key="run.id">
                <td class="col-time run-time">{{ formatRunTime(run.started_at ?? run.created_at, undefined, taskTimezone(run.task_id)) }}</td>
                <td><button class="text-button" @click="openRunLogHistory(run.task_id)">{{ runTaskName(run.task_id) }}</button></td>
                <td class="col-status"><strong :class="runStatusClass(run.status)">{{ runStatusLabel(run.status) }}</strong></td>
                <td class="run-log-col"><span class="run-log">{{ runLogText(run) }}</span></td>
                <td class="row-actions">
                  <button v-if="['pending','leased','running'].includes(run.status)" class="icon-button" :title="t('cancelRun')" @click="cancelRun(run)"><X :size="15" /></button>
                  <button class="icon-button" :title="t('deleteRun')" @click="removeRun(run)"><Trash2 :size="15" /></button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        </template>
        <Pager
          :page="runLogPageNo"
          :page-size="runLogPageSize"
          :busy="runLogLoading"
          :prev-disabled="!runLogCanGoBack"
          :next-disabled="!runLogHasMore"
          @prev="prevRunLogPage"
          @next="nextRunLogPage"
          @update:page-size="setRunLogPageSize"
        />
      </div>
    </div>

    <!-- ===== HIGH-RISK CONFIRMATION ===== -->
    <!-- The switch's own sentence, shown at the moment it is turned on. In the
         form it was a paragraph the eye skipped past on the way to the save
         button; here it is the thing being answered. 取消 puts the box back, so
         the dialog is the only way the value moves. -->
    <div v-if="riskPrompt" class="modal-backdrop" @click.self="cancelRisk">
      <div class="modal modal-risk">
        <div class="modal-header">
          <div class="risk-heading">
            <h2>{{ riskPrompt === 'allowPrivateNetwork' ? t('allowPrivateNetwork') : t('allowInvalidCertificates') }}</h2>
            <span class="chip chip-warn">{{ t('highRisk') }}</span>
          </div>
          <button class="icon-button" type="button" :title="t('close')" @click="cancelRisk"><X :size="20" /></button>
        </div>
        <p v-if="riskPrompt === 'allowPrivateNetwork'" class="risk-notice">{{ t('allowPrivateNetworkRisk') }}</p>
        <p v-else class="risk-notice">{{ t('allowInvalidCertificatesRisk') }}</p>
        <div class="modal-actions">
          <button class="secondary-button" type="button" @click="cancelRisk">{{ t('cancel') }}</button>
          <button class="primary-button" type="button" @click="acceptRisk">{{ t('highRiskConfirm') }}</button>
        </div>
      </div>
    </div>

    <!-- ===== HELP MODAL ===== -->
    <div v-if="showHelp" class="modal-backdrop" @click.self="showHelp = false">
      <div class="modal">
        <div class="modal-header">
          <div><h2>{{ t('helpTitle') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="showHelp = false"><X :size="20" /></button>
        </div>
        <p class="auth-hint">{{ t('helpBody') }}</p>
        <div class="modal-actions"><button class="secondary-button" type="button" @click="showHelp = false">{{ t('close') }}</button></div>
      </div>
    </div>

    <!-- ===== TOASTS ===== -->
    <div class="toast-stack" aria-live="polite">
      <div v-for="toast in toasts" :key="toast.id" :class="['toast', toast.kind]">
        <Loader2 v-if="toast.kind === 'pending'" :size="16" class="toast-spin" />
        <CheckCircle2 v-else-if="toast.kind === 'success'" :size="16" />
        <XCircle v-else :size="16" />
        <div class="toast-body">
          <strong v-if="toast.title" class="toast-title">{{ toast.title }}</strong>
          <span>{{ toast.message }}</span>
          <span v-if="toast.meta" class="toast-meta">{{ toast.meta }}</span>
          <pre v-if="toast.detail" class="toast-detail">{{ toast.detail }}</pre>
          <button v-if="toast.taskId != null" class="text-button toast-action" @click="openRunHistoryFromToast(toast)">{{ t('viewRunHistory') }}</button>
        </div>
        <button class="icon-button" :title="t('close')" @click="dismissToast(toast.id)"><X :size="14" /></button>
      </div>
    </div>
  </div>
</template>
