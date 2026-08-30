<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import {
  Activity, ArrowLeft, ArrowRight, Bell, CalendarClock, Check, CheckCircle2, ChevronDown, CircleHelp, Copy, FileJson2, FileUp,
  LayoutDashboard, Loader2, Mail, Menu, Monitor, Moon, Pencil, Play, Plus, RefreshCw, Search, Send,
  Settings, Sun, Trash2, Undo2, Upload, Users, X, XCircle, Zap,
} from "@lucide/vue";
import { api, type CreateTask, type Task, type Run, type RunStep, type User, type Template, type Plugin, type NotificationChannel, type NotificationAction, type TemplateSubscription, type SubscriptionSync, type PushRequest, type SiteSetting } from "./api";
import HarEditor from "./HarEditor.vue";
import { formatRunTime } from "./utils";
import { locale, t, toggleLocale } from "./i18n";

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
function fmt(key: Parameters<typeof t>[0], params?: Record<string, string | number>): string {
  let s = t(key);
  if (params) for (const [k, v] of Object.entries(params)) s = s.replace(`{${k}}`, String(v));
  return s;
}

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
const verifyResult = ref<"ok" | "fail" | null>(null);
const forgotResult = ref<{ sent: boolean; token?: string } | null>(null);
const view = ref<"tasks" | "taskRuns" | "templates" | "plugins" | "notifications" | "subscriptions" | "push" | "admin" | "settings">("tasks");
const menuOpen = ref(false);
const showCreate = ref(false);
const showImport = ref(false);
const showHelp = ref(false);

const currentViewName = computed(() => ({
  tasks: t("tasks"), taskRuns: t("runHistory"), templates: t("templates"), plugins: t("pluginsTitle"),
  notifications: t("notificationsTitle"), subscriptions: t("subscriptionsTitle"),
  push: t("pushTitle"), admin: t("adminTitle"), settings: t("settingsTitle"),
}[view.value]));

// ---------- tasks ----------
const tasks = ref<Task[]>([]);
const taskGroups = ref<string[]>([]);
const loading = ref(false);
const search = ref("");
const groupFilter = ref("");
const selected = reactive(new Set<number>());
const runsByTask = ref<Record<number, Run[]>>({});
const runHistoryTask = ref<Task | null>(null);

interface TaskForm {
  id: number | null;
  name: string;
  cron: string;
  scheduleTime: string;
  scheduleDays: string;
  scheduleAdvanced: boolean;
  randomDelay: string;
  method: string;
  url: string;
  headersText: string;
  body: string;
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
const blankTaskForm = (): TaskForm => ({ id: null, name: "", cron: "", scheduleTime: "08:00:00", scheduleDays: "1", scheduleAdvanced: false, randomDelay: "", method: "GET", url: "", headersText: "{}", body: "", disabled: false, grp: "", templateId: null, timeoutSeconds: "", retryCount: "", retryInterval: "", priority: "", timezone: "", variables: [] });
const taskForm = reactive<TaskForm>(blankTaskForm());
const templatesForSelect = computed(() => templates.value);

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
/** 扫描模板 HAR 中所有 {{变量名}} 引用（qd find_variables 同思路），排除 __log__ 等内部变量 */
function harVariableNames(har: unknown): string[] {
  const names = new Set<string>();
  const walk = (value: unknown) => {
    if (typeof value === "string") {
      for (const match of value.matchAll(/\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}/g)) names.add(match[1]);
      // QD templates commonly apply filters/functions, e.g. {{username|urlencode}}
      // or {{rsa(password)}}. The simple identifier form above does not catch
      // those, so explicitly retain credential inputs wherever they occur in
      // a Jinja expression.
      if (/\{\{[\s\S]*?\busername\b[\s\S]*?\}\}/.test(value)) names.add("username");
      if (/\{\{[\s\S]*?\bpassword\b[\s\S]*?\}\}/.test(value)) names.add("password");
    } else if (Array.isArray(value)) value.forEach(walk);
    else if (value && typeof value === "object") Object.values(value).forEach(walk);
  };
  walk(har);
  // QD also exposes cookie values as task-level inputs. Cookie names are not
  // written as {{name}} in many exported HAR files, so discover empty or
  // templated cookie values explicitly and create an input for each one.
  const entries = (har as Record<string, unknown>)?.log && typeof har === "object"
    ? ((har as Record<string, unknown>).log as Record<string, unknown>)?.entries
    : Array.isArray(har) ? har : null;
  if (Array.isArray(entries)) for (const entry of entries) {
    const cookies = (entry as Record<string, unknown>)?.request && typeof (entry as Record<string, unknown>).request === "object"
      ? ((entry as Record<string, unknown>).request as Record<string, unknown>).cookies : null;
    if (Array.isArray(cookies)) for (const cookie of cookies) {
      if (!cookie || typeof cookie !== "object") continue;
      const item = cookie as Record<string, unknown>;
      const name = typeof item.name === "string" ? item.name.trim() : "";
      const value = typeof item.value === "string" ? item.value : "";
      if (name && (!value || /\{\{/.test(value))) names.add(name);
    }
  }
  return [...names].filter((name) => !name.startsWith("__"));
}

/** 收集模板的提取输出变量名（extract_variables），它们在运行时产生，不需要用户填写 */
function harExtractedNames(har: unknown): Set<string> {
  const names = new Set<string>();
  const entries = (har as Record<string, unknown>)?.log && typeof har === "object"
    ? ((har as Record<string, unknown>).log as Record<string, unknown>)?.entries
    : Array.isArray(har) ? har : null;
  if (!Array.isArray(entries)) return names;
  for (const entry of entries) {
    if (!entry || typeof entry !== "object") continue;
    const record = entry as Record<string, unknown>;
    const rule = record.rule && typeof record.rule === "object" ? record.rule as Record<string, unknown> : record;
    for (const extract of Array.isArray(rule.extract_variables) ? rule.extract_variables : []) {
      const name = (extract as Record<string, unknown>)?.name;
      if (typeof name === "string") names.add(name);
    }
  }
  return names;
}

const filteredTasks = computed(() => {
  const term = search.value.trim().toLowerCase();
  return tasks.value.filter((task) => {
    if (groupFilter.value && (task.grp ?? "") !== groupFilter.value) return false;
    if (!term) return true;
    return `${task.name} ${task.url}`.toLowerCase().includes(term);
  });
});
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
  let headers: Record<string, unknown> = {};
  if (taskForm.headersText.trim()) {
    try {
      const parsed = JSON.parse(taskForm.headersText);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) throw new Error("headers must be an object");
      headers = parsed;
    } catch (cause) {
      notify(cause instanceof Error ? `Headers: ${cause.message}` : t("genericError"), "error");
      return;
    }
  }
  if (taskForm.timeoutSeconds && !(Number(taskForm.timeoutSeconds) > 0)) return void notify("timeout must be a positive number", "error");
  if (taskForm.retryInterval && !(Number(taskForm.retryInterval) > 0)) return void notify("retry interval must be a positive number", "error");
  if (taskForm.priority && Number.isNaN(Number(taskForm.priority))) return void notify("priority must be a number", "error");
  const cron = taskForm.scheduleAdvanced ? taskForm.cron : buildCron();
  const payload: CreateTask = {
    name: taskForm.name,
    cron,
    method: taskForm.method,
    url: taskForm.url,
    headers,
    body: taskForm.body || null,
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
      await api.updateTask(taskForm.id, { ...payload, headers: Object.keys(headers).length ? headers : null });
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
  showCreate.value = true;
  if (!templates.value.length) {
    void api.templates(undefined, undefined, 200)
      .then((items) => { templates.value = items; })
      .catch((cause) => notify(cause instanceof Error ? cause.message : t("genericError"), "error"));
  }
}
function openEditTask(task: Task) {
  const visual = parseVisualCron(task.cron);
  Object.assign(taskForm, {
    id: task.id,
    name: task.name,
    cron: task.cron,
    scheduleTime: visual?.time ?? "08:00:00",
    scheduleDays: visual?.days ?? "1",
    scheduleAdvanced: !visual,
    randomDelay: task.random_delay_max_seconds != null && task.random_delay_max_seconds > 0 ? String(task.random_delay_max_seconds) : "",
    method: task.method,
    url: task.url,
    headersText: task.headers && typeof task.headers === "object" && !Array.isArray(task.headers) ? JSON.stringify(task.headers, null, 2) : "{}",
    body: task.body ?? "",
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
function selectAllVisible() {
  const ids = filteredTasks.value.map((x) => x.id);
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
async function cancelRun(run: Run) {
  try {
    await api.cancelRun(run.id);
    if (runHistoryTask.value != null) await loadTaskRuns(runHistoryTask.value.id);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeRun(run: Run) {
  if (!window.confirm(t("confirmDeleteRun"))) return;
  try {
    await api.deleteRun(run.id);
    if (runHistoryTask.value != null) await loadTaskRuns(runHistoryTask.value.id);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
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

// ---------- templates ----------
const templates = ref<Template[]>([]);
const publicTemplates = ref<Template[]>([]);
const templateSearch = ref("");
const editingTemplateId = ref<number | null>(null);
const importForm = reactive({ name: "", description: "" });
const harEditorDoc = ref<object | null>(null);
const filteredTemplates = computed(() => {
  const term = templateSearch.value.trim().toLowerCase();
  return term ? templates.value.filter((x) => `${x.name} ${x.description ?? ""}`.toLowerCase().includes(term)) : templates.value;
});
/** The public list is every published template, so it doubles as the publish-state index. */
const publishedTemplateIds = computed(() => new Set(publicTemplates.value.map((x) => x.id)));
async function openTemplates() {
  view.value = "templates";
  try {
    const [mine, pub] = await Promise.all([api.templates(undefined, undefined, 200), api.publicTemplates()]);
    templates.value = mine;
    publicTemplates.value = pub;
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function firstHarUrl(template: Template): string {
  const entries = (template.qd_har as any)?.log?.entries;
  if (!Array.isArray(entries)) return "";
  const entry = entries.find((e: any) => e.checked) ?? entries[0];
  return entry?.request?.url ?? "";
}
function onTemplatePicked() {
  const tmpl = templatesForSelect.value.find((x) => x.id === taskForm.templateId);
  if (!tmpl) return;
  if (!taskForm.name) taskForm.name = tmpl.name;
  if (!taskForm.url) taskForm.url = firstHarUrl(tmpl);
  // QD 式变量联动：从模板 HAR 提取 {{变量名}} 引用，自动生成填值行（保留已输入的同名值）。
  // extract_variables 的提取输出（如 points/error/__log__）在运行时产生，不出现在填值表单。
  const extracted = harExtractedNames(tmpl.qd_har);
  const names = harVariableNames(tmpl.qd_har).filter((name) => !extracted.has(name));
  if (names.length) {
    const existing = new Map(taskForm.variables.filter((row) => row.name.trim()).map((row) => [row.name.trim(), row.value]));
    taskForm.variables = names.map((name) => ({ name, value: existing.get(name) ?? "" }));
    notify(fmt("templateVarsFound", { n: names.length }));
  }
}
watch(() => taskForm.templateId, (id, previous) => {
  if (id != null && id !== previous) onTemplatePicked();
});
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
async function removeTemplate(id: number, name: string) {
  if (!window.confirm(fmt("deleteTemplateConfirm", { name }))) return;
  try { await api.deleteTemplate(id); await openTemplates(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function openImportModal(template?: Template) {
  editingTemplateId.value = template?.id ?? null;
  Object.assign(importForm, { name: template?.name ?? "", description: template?.description ?? "" });
  harEditorDoc.value = template?.qd_har ?? { log: { version: "1.2", creator: { name: "qdrust", version: "1" }, entries: [] as unknown[] } };
  showImport.value = true;
}

/**
 * QD 旧版模板数组（[{comment, request:{method,url,headers,cookies,data,mimeType}, rule:{...}}]）
 * 转 QD HAR 文档，与 QD 前端 utils.tpl2har 行为一致：
 * request.data → postData.text、mimeType → postData.mimeType、rule.* 平铺到条目上，
 * headers/cookies/条目一律 checked: true。
 */
function qdTplToHar(tpl: unknown[]): object {
  const entries = tpl.map((item) => {
    const raw = (item && typeof item === "object" && !Array.isArray(item) ? item : {}) as Record<string, unknown>;
    const req = (raw.request && typeof raw.request === "object" && !Array.isArray(raw.request) ? raw.request : {}) as Record<string, unknown>;
    const rule = (raw.rule && typeof raw.rule === "object" && !Array.isArray(raw.rule) ? raw.rule : {}) as Record<string, unknown>;
    const data = typeof req.data === "string" ? req.data : undefined;
    const mimeType = typeof req.mimeType === "string" ? req.mimeType : undefined;
    const entry: Record<string, unknown> = {
      checked: true,
      request: {
        method: typeof req.method === "string" && req.method.trim() ? req.method : "GET",
        url: typeof req.url === "string" ? req.url : "",
        headers: Array.isArray(req.headers)
          ? req.headers.map((h) => ({ name: String((h as Record<string, unknown>)?.name ?? ""), value: String((h as Record<string, unknown>)?.value ?? ""), checked: true }))
          : [],
        cookies: Array.isArray(req.cookies)
          ? req.cookies.map((c) => ({ name: String((c as Record<string, unknown>)?.name ?? ""), value: String((c as Record<string, unknown>)?.value ?? ""), checked: true }))
          : [],
        queryString: [],
        ...(data !== undefined || mimeType !== undefined ? { postData: { mimeType: mimeType ?? "", ...(data !== undefined ? { text: data } : {}) } } : {}),
      },
      success_asserts: Array.isArray(rule.success_asserts) ? rule.success_asserts : [],
      failed_asserts: Array.isArray(rule.failed_asserts) ? rule.failed_asserts : [],
      extract_variables: Array.isArray(rule.extract_variables) ? rule.extract_variables : [],
    };
    if (typeof raw.comment === "string" && raw.comment) entry.comment = raw.comment;
    return entry;
  });
  return { log: { version: "1.2", creator: { name: "binux", version: "QD" }, entries } };
}

/** 导入本地模板文件：兼容标准 HAR（{log:{entries}}）与 QD 导出的请求数组两种格式 */
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
  let doc: object | null = null;
  if (Array.isArray(parsed)) {
    doc = qdTplToHar(parsed);
  } else if (parsed && typeof parsed === "object" && (parsed as Record<string, unknown>).log) {
    // 标准 HAR 文档；后端执行要求 version 1.2，导入时统一归一化
    const log = ((parsed as Record<string, unknown>).log && typeof (parsed as Record<string, unknown>).log === "object"
      ? { ...((parsed as Record<string, unknown>).log as Record<string, unknown>) }
      : {});
    log.version = "1.2";
    doc = { log };
  }
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
  try {
    if (editingTemplateId.value) await api.updateQdHar(editingTemplateId.value, importForm.name, importForm.description, doc);
    else await api.importQdHar(importForm.name, importForm.description, doc);
    notify(t("importDone"));
    showImport.value = false;
    editingTemplateId.value = null;
    Object.assign(importForm, { name: "", description: "" });
    harEditorDoc.value = null;
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
const channelForm = reactive({
  name: "", kind: "webhook" as NotificationChannel["kind"],
  url: "", sound: "", group: "", sendkey: "", tgToken: "", tgChatId: "", tgHost: "",
  dingToken: "", wxToken: "", wxUid: "", spt: "", corpId: "", agentId: "",
  secret: "", toUser: "", wecomKey: "", to: "", subject: "",
});
const actionForm = reactive({ taskId: 0, channelId: 0, event: "failure" });
const channelKindLabels: Record<NotificationChannel["kind"], string> = {
  webhook: t("webhookKind"), email: t("emailKind"), bark: t("barkKind"),
  serverchan: t("serverchanKind"), telegram: t("telegramKind"), dingtalk: t("dingtalkKind"),
  wxpusher: t("wxpusherKind"), wxpusher_spt: t("wxpusherSptKind"),
  wecom_app: t("wecomAppKind"), wecom_webhook: t("wecomWebhookKind"),
};
function channelKindLabel(kind: string): string {
  return channelKindLabels[kind as NotificationChannel["kind"]] ?? kind;
}
function buildChannelConfig(): Record<string, unknown> {
  const f = channelForm;
  const trim = (value: string) => value.trim();
  const optional = (value: string) => trim(value) || undefined;
  switch (f.kind) {
    case "webhook": return { url: trim(f.url) };
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
    if (actionForm.taskId) actions.value = await api.notificationActions(actionForm.taskId);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function saveChannel() {
  try {
    await api.createNotificationChannel(channelForm.name, channelForm.kind, buildChannelConfig());
    Object.assign(channelForm, { name: "", kind: "webhook", url: "", sound: "", group: "", sendkey: "", tgToken: "", tgChatId: "", tgHost: "", dingToken: "", wxToken: "", wxUid: "", spt: "", corpId: "", agentId: "", secret: "", toUser: "", wecomKey: "", to: "", subject: "" });
    await openNotifications();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleChannel(channel: NotificationChannel) {
  try { await api.updateNotificationChannel(channel.id, !channel.enabled); await openNotifications(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeChannel(channel: NotificationChannel) {
  if (!window.confirm(fmt("deleteChannelConfirm", { name: channel.name }))) return;
  try { await api.deleteNotificationChannel(channel.id); await openNotifications(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function loadActions() {
  try { actions.value = actionForm.taskId ? await api.notificationActions(actionForm.taskId) : []; }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function saveAction() {
  try {
    await api.createNotificationAction(actionForm.taskId, actionForm.channelId, actionForm.event);
    await loadActions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeAction(id: number) {
  try { await api.deleteNotificationAction(id); await loadActions(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- subscriptions ----------
const subscriptions = ref<TemplateSubscription[]>([]);
const subSyncs = ref<SubscriptionSync[]>([]);
const subForm = reactive({ name: "", url: "" });
const syncingId = ref<number | null>(null);
async function openSubscriptions() {
  view.value = "subscriptions";
  try { [subscriptions.value, templates.value] = await Promise.all([api.subscriptions(), api.templates(undefined, undefined, 200)]); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function saveSubscription() {
  try {
    await api.createSubscription(subForm.name, subForm.url);
    Object.assign(subForm, { name: "", url: "" });
    await openSubscriptions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleSubscription(sub: TemplateSubscription) {
  try { await api.updateSubscription(sub.id, !sub.enabled); await openSubscriptions(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeSubscription(id: number) {
  if (!window.confirm(t("deleteSubConfirm"))) return;
  try { await api.deleteSubscription(id); await openSubscriptions(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function syncSubscription(id: number) {
  syncingId.value = id;
  try {
    await api.syncSubscription(id);
    subSyncs.value = await api.subscriptionSyncs(id);
    await openSubscriptions();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
  finally { syncingId.value = null; }
}
async function showSubSyncs(id: number) {
  try { subSyncs.value = await api.subscriptionSyncs(id); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- push ----------
const myPushRequests = ref<PushRequest[]>([]);
const pendingPushRequests = ref<PushRequest[]>([]);
const pushNote = ref("");
const pushTemplateId = ref(0);
const isAdmin = computed(() => currentUser.value?.role === "admin");
async function openPush() {
  view.value = "push";
  try {
    const [mine, tpls] = await Promise.all([api.myPushRequests(), api.templates(undefined, undefined, 200)]);
    myPushRequests.value = mine;
    templates.value = tpls;
    pendingPushRequests.value = isAdmin.value ? await api.adminPushRequests("pending").catch(() => []) : [];
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function submitPush() {
  try {
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
const settingsForm = reactive({ requireEmail: false, gaKey: "", retentionDays: 0 });
async function openAdmin() {
  view.value = "admin";
  try {
    [adminUsers.value, adminSettings.value] = await Promise.all([api.adminUsers(), api.adminSettings()]);
    const requireEmail = adminSettings.value.find((s) => s.key === "require_email_verification");
    const ga = adminSettings.value.find((s) => s.key === "ga_key");
    const retention = adminSettings.value.find((s) => s.key === "logs.retention_days");
    settingsForm.requireEmail = requireEmail?.value === true;
    settingsForm.gaKey = typeof ga?.value === "string" ? ga.value : "";
    settingsForm.retentionDays = typeof retention?.value === "number" ? retention.value : 0;
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function toggleUser(user: User) {
  try { await api.adminUpdateUser(user.id, { disabled: !user.disabled }); await openAdmin(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function changeUserRole(user: User, role: string) {
  try { await api.adminUpdateUser(user.id, { role }); await openAdmin(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function onRoleChange(user: User, event: Event) {
  changeUserRole(user, (event.target as HTMLSelectElement).value);
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
  if (!silent) notify(t("logout"));
  authMode.value = "login";
  Object.assign(authForm, { username: "", password: "" });
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
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      if (showCreate.value) showCreate.value = false;
      if (showImport.value) showImport.value = false;
      if (showHelp.value) showHelp.value = false;
      menuOpen.value = false;
    }
  });
  try {
    const session = await api.session();
    currentUser.value = session.user;
    authenticated.value = true;
    await Promise.all([loadTasks(), api.ready().then(() => { ready.value = true; })]);
  } catch {
    authenticated.value = false;
    ready.value = true;
  }
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
    <form class="auth-panel" @submit.prevent="authMode === 'forgot' ? submitForgot() : authenticate()">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <h1>{{ authMode === "login" ? t('loginTitle') : authMode === "bootstrap" ? t('bootstrapTitle') : authMode === "register" ? t('registerTitle') : authMode === "forgot" ? t('forgotTitle') : t('resetTitle') }}</h1>

      <template v-if="authMode === 'login' || authMode === 'bootstrap' || authMode === 'register'">
        <label>{{ t('username') }}<input v-model="authForm.username" required autocomplete="username" minlength="3" /></label>
        <label v-if="authMode === 'register'">{{ t('email') }}<input v-model="authForm.email" type="email" autocomplete="email" /></label>
        <label v-if="authMode === 'register'" class="auth-hint">{{ t('registerHint') }}</label>
        <label>{{ t('password') }}<input v-model="authForm.password" required minlength="12" type="password" :autocomplete="authMode === 'login' ? 'current-password' : 'new-password'" /></label>
        <label v-if="authMode === 'register'" class="auth-hint">{{ t('passwordMinHint') }}</label>
      </template>

      <template v-else-if="authMode === 'forgot'">
        <label>{{ t('username') }}<input v-model="authForm.username" required autocomplete="username" /></label>
        <p class="auth-hint">{{ t('forgotHint') }}</p>
        <div v-if="forgotResult?.token" class="dev-token">
          {{ t('forgotDevToken') }}<code>{{ forgotResult.token }}</code>
          <button type="button" class="secondary-button" @click="authMode = 'reset'">{{ t('resetTitle') }}<ArrowRight :size="15" /></button>
        </div>
      </template>

      <template v-else>
        <label>{{ t('username') }}<input :value="authForm.token" disabled /></label>
        <label>{{ t('resetNewPassword') }}<input v-model="authForm.newPassword" required minlength="12" type="password" /></label>
      </template>

      <div v-if="authNotice" class="auth-notice">{{ authNotice }}</div>
      <div v-if="verifyResult" class="auth-notice">{{ verifyResult === 'ok' ? t('verifyDone') : t('verifyFail') }}</div>

      <button class="primary-button" type="submit">
        {{ authMode === 'login' ? t('login') : authMode === 'bootstrap' ? t('createAdmin') : authMode === 'register' ? t('register') : authMode === 'forgot' ? t('forgotSubmit') : t('resetSubmit') }}
      </button>

      <button v-if="authMode === 'login'" class="secondary-button" type="button" @click="authMode='forgot'; authNotice=''">{{ t('forgotPassword') }}</button>
      <button v-if="authMode === 'forgot' || authMode === 'reset'" class="secondary-button" type="button" @click="authMode='login'; authNotice=''">{{ t('backToLogin') }}</button>
      <button v-if="authMode === 'login' || authMode === 'bootstrap' || authMode === 'register'" class="secondary-button" type="button" @click="authMode = authMode === 'login' ? 'bootstrap' : authMode === 'bootstrap' ? 'register' : 'login'; authNotice=''; verifyResult=null">
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
        <a :class="['nav-link', { active: view === 'subscriptions' }]" href="#" @click.prevent="openSubscriptions"><RefreshCw :size="18" />{{ t('subscriptions') }}</a>
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
          <button class="primary-button" @click="openCreateTask"><Plus :size="17" />{{ t('createTaskShort') }}</button>
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
              <select v-model="groupFilter">
                <option value="">{{ t('all') }}</option>
                <option v-for="g in taskGroups" :key="g" :value="g">{{ g }}</option>
              </select>
            </label>
            <button class="icon-button" :title="t('refresh')" @click="loadTasks"><RefreshCw :class="{ spin: loading }" :size="18" /></button>
          </div>

          <div v-if="selected.size > 0" class="batch-bar">
            <span>{{ fmt('selectedCount', { n: selected.size }) }}</span>
            <button class="secondary-button" @click="batchTasks('enable')">{{ t('batchEnable') }}</button>
            <button class="secondary-button" @click="batchTasks('disable')">{{ t('batchDisable') }}</button>
            <button class="secondary-button" @click="batchTasks('run')">{{ t('batchRun') }}</button>
            <button class="secondary-button danger" @click="batchTasks('delete')">{{ t('batchDelete') }}</button>
            <button class="icon-button" :title="t('close')" @click="selected.clear()"><X :size="16" /></button>
          </div>

          <div v-if="loading" class="loading-state"><RefreshCw class="spin" :size="22" />{{ t('loading') }}</div>
          <div v-else-if="filteredTasks.length === 0" class="empty-state">
            <span><CalendarClock :size="25" /></span>
            <h2>{{ search || groupFilter ? t('noTasksMatch') : t('createFirst') }}</h2>
            <button v-if="!search && !groupFilter" class="secondary-button" @click="openCreateTask"><Plus :size="16" />{{ t('createTaskShort') }}</button>
          </div>
          <div v-else class="table-wrap">
            <table>
              <thead><tr>
                <th class="col-check"><input type="checkbox" :checked="filteredTasks.length > 0 && filteredTasks.every(x => selected.has(x.id))" :title="t('selectAll')" @change="selectAllVisible" /></th>
                <th>{{ t('name') }}</th><th>{{ t('schedule') }}</th><th>{{ t('lastRunAt') }}</th><th>{{ t('status') }}</th><th>{{ t('group') }}</th><th><span class="sr-only">{{ t('more') }}</span></th>
              </tr></thead>
              <tbody>
                <template v-for="task in filteredTasks" :key="task.id">
                  <tr>
                    <td class="col-check"><input type="checkbox" :checked="selected.has(task.id)" @change="toggleSelect(task.id)" /></td>
                    <td><div class="task-name"><span :class="['method', task.method.toLowerCase()]">{{ task.method }}</span><div><strong>{{ task.name }}</strong><small>{{ task.url }}</small></div></div></td>
                    <td><code>{{ task.cron }}</code></td>
                    <td>{{ formatRunTime(task.last_run_at) }}</td>
                    <td><button :class="['status-pill', { paused: task.disabled, 'run-bad': taskStatusLabel(task) === '失败' }]" @click="toggleTask(task)"><span />{{ taskStatusLabel(task) }}</button></td>
                    <td>{{ task.grp ?? '–' }}</td>
                    <td class="row-actions">
                      <button class="icon-button" :title="t('runNow')" @click="runNow(task)"><Play :size="17" /></button>
                      <button class="icon-button" :title="t('runHistory')" @click="openRunHistory(task)"><Activity :size="17" /></button>
                      <button class="icon-button" :title="t('editTask')" @click="openEditTask(task)"><Pencil :size="17" /></button>
                      <button class="icon-button" :title="t('deleteTask')" @click="removeTask(task)"><Trash2 :size="17" /></button>
                    </td>
                  </tr>
                </template>
              </tbody>
            </table>
          </div>
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
                  <td class="run-time">{{ formatRunTime(run.started_at ?? run.created_at) }}</td>
                  <td><strong :class="runStatusClass(run.status)">{{ runStatusLabel(run.status) }}</strong></td>
                  <td><span class="run-log">{{ runLogText(run) }}</span></td>
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
          <button class="primary-button" @click="openImportModal()"><Plus :size="17" />{{ t('importHar') }}</button>
        </section>
        <section class="task-section">
          <div class="toolbar">
            <label class="search"><Search :size="17" /><input v-model="templateSearch" type="search" :placeholder="t('templateSearch')" /></label>
          </div>
          <h2>{{ t('myTemplates') }}</h2>
          <div v-if="filteredTemplates.length === 0" class="muted">{{ t('noTemplates') }}</div>
          <div v-for="item in filteredTemplates" :key="item.id" class="run-row">
            <strong>{{ item.name }}</strong>
            <span>{{ item.source_format }}</span>
            <span v-if="item.grp" class="chip">{{ item.grp }}</span>
            <span v-if="publishedTemplateIds.has(item.id)" class="chip chip-published">{{ t('published') }}</span>
            <span v-if="item.description" class="muted row-description">{{ item.description }}</span>
            <div class="row-actions">
              <button v-if="item.source_format === 'qd_har'" class="secondary-button" @click="openImportModal(item)"><Pencil :size="14" />{{ t('editTemplate') }}</button>
              <button v-if="publishedTemplateIds.has(item.id)" class="secondary-button" @click="unpublishTemplate(item.id)"><Undo2 :size="14" />{{ t('unpublish') }}</button>
              <button v-else class="secondary-button" @click="publishTemplate(item.id)"><Upload :size="14" />{{ t('publish') }}</button>
              <button class="icon-button danger" :title="t('deleteTemplate')" @click="removeTemplate(item.id, item.name)"><Trash2 :size="16" /></button>
            </div>
          </div>
          <h2>{{ t('publicTemplates') }}</h2>
          <div v-if="publicTemplates.length === 0" class="muted">{{ t('noTemplates') }}</div>
          <div v-for="item in publicTemplates" :key="item.id" class="run-row">
            <strong>{{ item.name }}</strong>
            <span>{{ item.source_format }}</span>
            <span v-if="item.description" class="muted row-description">{{ item.description }}</span>
            <div class="row-actions">
              <button class="secondary-button" @click="copyTemplate(item.id)"><Copy :size="14" />{{ t('copy') }}</button>
            </div>
          </div>
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
          <form class="modal inline-modal" @submit.prevent="saveChannel">
            <label>{{ t('channelName') }}<input v-model="channelForm.name" required /></label>
            <label>{{ t('channelKind') }}
              <select v-model="channelForm.kind">
                <option value="webhook">{{ t('webhookKind') }}</option>
                <option value="email">{{ t('emailKind') }}</option>
                <option value="bark">{{ t('barkKind') }}</option>
                <option value="serverchan">{{ t('serverchanKind') }}</option>
                <option value="telegram">{{ t('telegramKind') }}</option>
                <option value="dingtalk">{{ t('dingtalkKind') }}</option>
                <option value="wxpusher">{{ t('wxpusherKind') }}</option>
                <option value="wxpusher_spt">{{ t('wxpusherSptKind') }}</option>
                <option value="wecom_app">{{ t('wecomAppKind') }}</option>
                <option value="wecom_webhook">{{ t('wecomWebhookKind') }}</option>
              </select>
            </label>
            <template v-if="channelForm.kind === 'webhook'">
              <label>{{ t('webhookUrl') }}<input v-model="channelForm.url" required type="url" placeholder="https://example.com/hook" /></label>
            </template>
            <template v-else-if="channelForm.kind === 'email'">
              <label>{{ t('emailTo') }}<input v-model="channelForm.to" required type="email" /></label>
              <label>{{ t('emailSubject') }}<input v-model="channelForm.subject" /></label>
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
            <button class="primary-button">{{ t('createChannel') }}</button>
          </form>
          <div v-if="channels.length === 0" class="muted">{{ t('noChannels') }}</div>
          <div v-for="channel in channels" :key="channel.id" class="run-row">
            <strong>{{ channel.name }}</strong>
            <span class="chip">{{ channelKindLabel(channel.kind) }}</span>
            <button class="secondary-button" @click="toggleChannel(channel)">{{ channel.enabled ? t('disable') : t('enable') }}</button>
            <button class="icon-button" :title="t('deleteChannel')" @click="removeChannel(channel)"><Trash2 :size="16" /></button>
          </div>
          <h2>{{ t('taskActions') }}</h2>
          <form class="modal inline-modal" @submit.prevent="saveAction">
            <label>{{ t('task') }}
              <select v-model="actionForm.taskId" required @change="loadActions">
                <option :value="0" disabled>{{ t('chooseTask') }}</option>
                <option v-for="task in tasks" :key="task.id" :value="task.id">{{ task.name }}</option>
              </select>
            </label>
            <label>{{ t('channel') }}
              <select v-model="actionForm.channelId" required>
                <option :value="0" disabled>{{ t('chooseChannel') }}</option>
                <option v-for="channel in channels" :key="channel.id" :value="channel.id">{{ channel.name }}</option>
              </select>
            </label>
            <label>{{ t('event') }}
              <select v-model="actionForm.event">
                <option value="success">{{ t('eventSuccess') }}</option>
                <option value="failure">{{ t('eventFailure') }}</option>
                <option value="always">{{ t('eventAlways') }}</option>
              </select>
            </label>
            <button class="primary-button">{{ t('addAction') }}</button>
          </form>
          <div v-for="action in actions" :key="action.id" class="run-row">
            <strong>{{ action.event === 'success' ? t('eventSuccess') : action.event === 'failure' ? t('eventFailure') : t('eventAlways') }}</strong>
            <span>{{ t('channel') }}: {{ channelName(action.channel_id) }}</span>
            <span>{{ t('task') }}: {{ taskName(action.task_id) }}</span>
            <button class="icon-button" :title="t('deleteAction')" @click="removeAction(action.id)"><Trash2 :size="16" /></button>
          </div>
        </section>
      </div>

      <!-- ===== SUBSCRIPTIONS ===== -->
      <div v-else-if="view === 'subscriptions'" class="page">
        <section class="page-heading"><div><h1>{{ t('subscriptionsTitle') }}</h1><p>{{ t('subHint') }}</p></div></section>
        <section class="task-section">
          <form class="modal inline-modal" @submit.prevent="saveSubscription">
            <label>{{ t('subName') }}<input v-model="subForm.name" required /></label>
            <label>{{ t('subUrl') }}<input v-model="subForm.url" required type="url" placeholder="https://github.com/owner/repo" /></label>
            <button class="primary-button">{{ t('addSub') }}</button>
          </form>
          <div v-if="subscriptions.length === 0" class="muted">{{ t('noSubs') }}</div>
          <div v-for="sub in subscriptions" :key="sub.id" class="run-row">
            <strong>{{ sub.name }}</strong><span class="muted">{{ sub.url }}</span>
            <span v-if="sub.last_synced_at" class="run-time">{{ t('lastSync') }} {{ formatRunTime(sub.last_synced_at) }}</span>
            <span v-if="sub.last_error" class="error-text">{{ sub.last_error }}</span>
            <button class="secondary-button" :disabled="syncingId === sub.id" @click="syncSubscription(sub.id)">{{ syncingId === sub.id ? t('syncing') : t('sync') }}</button>
            <button class="secondary-button" @click="showSubSyncs(sub.id)">{{ t('syncs') }}</button>
            <button class="secondary-button" @click="toggleSubscription(sub)">{{ sub.enabled ? t('disableSub') : t('enableSub') }}</button>
            <button class="icon-button" :title="t('deleteSub')" @click="removeSubscription(sub.id)"><Trash2 :size="16" /></button>
          </div>
          <h2 v-if="subSyncs.length">{{ t('syncRecords') }}</h2>
          <div v-for="s in subSyncs" :key="s.id" class="run-row">
            <span class="run-id">#{{ s.id }}</span><strong :class="runStatusClass(s.status)">{{ s.status }}</strong>
            <span class="muted">{{ s.message }}</span><span class="run-time">{{ formatRunTime(s.created_at) }}</span>
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
              <select v-model="pushTemplateId" required>
                <option :value="0" disabled>{{ t('chooseTask') }}</option>
                <option v-for="tpl in templates" :key="tpl.id" :value="tpl.id">{{ tpl.name }}</option>
              </select>
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
            <select v-if="user.id !== currentUser?.id" class="role-select" :value="user.role" @change="onRoleChange(user, $event)">              <option value="user">{{ t('roleUser') }}</option>
              <option value="admin">{{ t('roleAdmin') }}</option>
            </select>
            <span v-if="user.email">{{ user.email }}</span>
            <span :class="user.email_verified ? 'ok-text' : 'muted'">{{ user.email_verified ? t('verified') : t('unverified') }}</span>
            <span class="run-time">{{ formatRunTime(user.created_at) }}</span>
            <button v-if="user.id !== currentUser?.id" class="secondary-button" @click="toggleUser(user)">{{ user.disabled ? t('enableUser') : t('disableUser') }}</button>
            <button v-if="user.id !== currentUser?.id" class="secondary-button danger" @click="deleteUser(user)"><Trash2 :size="14" />{{ t('deleteUser') }}</button>
          </div>

          <h2>{{ t('siteSettings') }}</h2>
          <form class="modal inline-modal" @submit.prevent="saveAdminSettings">
            <label class="checkbox"><input v-model="settingsForm.requireEmail" type="checkbox" />{{ t('requireEmailVerify') }}</label>
            <label>{{ t('gaKey') }}<input v-model="settingsForm.gaKey" placeholder="G-XXXXXXX" /></label>
            <label>{{ t('retentionDays') }}<input v-model.number="settingsForm.retentionDays" type="number" min="0" /></label>
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
              <div><dt>{{ t('apiDocs') }}</dt><dd><a href="/api/v1/openapi.json" target="_blank">{{ t('openapi') }}</a></dd></div>
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
          <select v-model="taskForm.templateId" @change="onTemplatePicked">
            <option :value="null">{{ t('noTemplatesToBind') }}</option>
            <option v-for="tpl in templatesForSelect" :key="tpl.id" :value="tpl.id">{{ tpl.name }}（{{ tpl.source_format }}）</option>
          </select>
        </label>
        <div class="form-row">
          <label>{{ t('method') }}<select v-model="taskForm.method"><option>GET</option><option>POST</option><option>PUT</option><option>PATCH</option><option>DELETE</option></select><ChevronDown :size="16" /></label>
          <label v-if="!taskForm.scheduleAdvanced">{{ t('scheduleEveryDays') }}<input v-model="taskForm.scheduleDays" type="number" min="1" max="366" /></label>
        </div>
        <div class="form-row">
          <label v-if="!taskForm.scheduleAdvanced">{{ t('scheduleTime') }}<input v-model="taskForm.scheduleTime" type="time" step="1" required /></label>
          <label v-if="taskForm.scheduleAdvanced">{{ t('cron') }}<input v-model="taskForm.cron" required placeholder="0 0 8 * * * *" /></label>
          <label>{{ t('randomDelayMax') }}<input v-model="taskForm.randomDelay" type="number" min="0" max="604800" placeholder="0" /></label>
        </div>
        <small class="kv-hint">
          <template v-if="!taskForm.scheduleAdvanced">{{ t('scheduleHint') }}</template>
          <a href="#" class="text-button" @click.prevent="taskForm.scheduleAdvanced = !taskForm.scheduleAdvanced">{{ taskForm.scheduleAdvanced ? t('scheduleTime') : t('scheduleAdvanced') }}</a>
        </small>
        <label>{{ t('url') }}<input v-model="taskForm.url" required type="url" placeholder="https://example.com/api/health" /></label>
        <label>{{ t('group') }}<input v-model="taskForm.grp" list="grp-options" :placeholder="t('group')" /></label>
        <datalist id="grp-options"><option v-for="g in taskGroups" :key="g" :value="g" /></datalist>
        <div class="form-row form-row-4">
          <label :title="t('timeoutSecondsHint')">{{ t('timeoutSeconds') }}<input v-model="taskForm.timeoutSeconds" type="number" min="1" placeholder="30" /></label>
          <label :title="t('retryCountHint')">{{ t('retryCount') }}<input v-model="taskForm.retryCount" type="number" placeholder="0" /></label>
          <label :title="t('retryIntervalHint')">{{ t('retryInterval') }}<input v-model="taskForm.retryInterval" type="number" min="1" placeholder="60" /></label>
          <label :title="t('priorityHint')">{{ t('priority') }}<input v-model="taskForm.priority" type="number" placeholder="0" /></label>
        </div>
        <small class="kv-hint">{{ t('taskNumericHint') }}</small>
        <label>{{ t('timezone') }}<input v-model="taskForm.timezone" list="tz-options" placeholder="UTC / Asia/Shanghai" /></label>
        <datalist id="tz-options">
          <option v-for="tz in ['UTC','Asia/Shanghai','Asia/Tokyo','Asia/Hong_Kong','Europe/London','Europe/Berlin','America/New_York','America/Los_Angeles','Australia/Sydney']" :key="tz" :value="tz" />
        </datalist>
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
        <label>{{ t('requestHeaders') }}<textarea v-model="taskForm.headersText" rows="4" spellcheck="false" /></label>
        <label>{{ t('body') }}<textarea v-model="taskForm.body" rows="3" spellcheck="false" /></label>
        <label class="checkbox"><input v-model="taskForm.disabled" type="checkbox" />{{ t('createPaused') }}</label>
        <div class="modal-actions">
          <button class="secondary-button" type="button" @click="showCreate = false">{{ t('cancel') }}</button>
          <button class="primary-button" type="submit">{{ taskForm.id ? t('saveTask') : t('createTask') }}</button>
        </div>
      </form>
    </div>

    <!-- ===== IMPORT / EDIT HAR MODAL ===== -->
    <div v-if="showImport" class="modal-backdrop modal-backdrop-wide" @click.self="showImport = false">
      <div class="modal modal-har">
        <div class="modal-header">
          <div><h2>{{ t('importHarTitle') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="showImport = false"><X :size="20" /></button>
        </div>
        <div class="har-meta">
          <label>{{ t('templateName') }}<input v-model="importForm.name" required placeholder="my-template" /></label>
          <label>{{ t('description') }}<input v-model="importForm.description" /></label>
          <label class="secondary-button har-file-pick" :title="t('harChooseFile')">
            <FileUp :size="15" />{{ t('harChooseFile') }}
            <input type="file" accept=".har,.json,application/json" @change="onHarFile" />
          </label>
        </div>
        <HarEditor :model-value="harEditorDoc" @save="saveHar" @cancel="showImport = false" />
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
