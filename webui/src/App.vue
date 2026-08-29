<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import {
  Activity, Bell, CalendarClock, Check, CheckCircle2, ChevronDown, CircleHelp, FileJson2,
  LayoutDashboard, ListChecks, Mail, Menu, Pencil, Play, Plus, RefreshCw, Search, Send,
  Settings, Trash2, Users, X, XCircle, Zap,
} from "@lucide/vue";
import { api, type CreateTask, type Task, type Run, type RunStep, type User, type Template, type Note, type Plugin, type NotificationChannel, type NotificationAction, type TemplateSubscription, type SubscriptionSync, type PushRequest, type SiteSetting, type LiveRunEvent } from "./api";
import { formatRunTime } from "./utils";
import { locale, t, toggleLocale } from "./i18n";

// ---------- toast ----------
const toasts = ref<{ id: number; message: string; kind: "success" | "error" }[]>([]);
let toastSeq = 0;
function notify(message: string, kind: "success" | "error" = "success") {
  const id = ++toastSeq;
  toasts.value.push({ id, message, kind });
  setTimeout(() => {
    toasts.value = toasts.value.filter((x) => x.id !== id);
  }, 4000);
}
function fmt(key: Parameters<typeof t>[0], params?: Record<string, string | number>): string {
  let s = t(key);
  if (params) for (const [k, v] of Object.entries(params)) s = s.replace(`{${k}}`, String(v));
  return s;
}
function fmtDuration(seconds?: number | null): string {
  if (seconds == null) return "–";
  return seconds >= 60 ? `${(seconds / 60).toFixed(1)}m` : `${Math.round(seconds * 10) / 10}s`;
}

// ---------- app / auth state ----------
const ready = ref(false);
const authenticated = ref(false);
const currentUser = ref<User | null>(null);
const authMode = ref<"login" | "bootstrap" | "register" | "forgot" | "reset">("login");
const authForm = reactive({ username: "", password: "", email: "", token: "", newPassword: "" });
const authNotice = ref("");
const verifyResult = ref<"ok" | "fail" | null>(null);
const forgotResult = ref<{ sent: boolean; token?: string } | null>(null);
const view = ref<"tasks" | "templates" | "notes" | "plugins" | "notifications" | "runs" | "subscriptions" | "push" | "admin" | "settings">("tasks");
const menuOpen = ref(false);
const showCreate = ref(false);
const showImport = ref(false);
const showHelp = ref(false);

const currentViewName = computed(() => ({
  tasks: t("tasks"), templates: t("templates"), notes: t("notesTitle"), plugins: t("pluginsTitle"),
  notifications: t("notificationsTitle"), runs: t("runsTitle"), subscriptions: t("subscriptionsTitle"),
  push: t("pushTitle"), admin: t("adminTitle"), settings: t("settingsTitle"),
}[view.value]));

// ---------- tasks ----------
const tasks = ref<Task[]>([]);
const taskGroups = ref<string[]>([]);
const loading = ref(false);
const search = ref("");
const groupFilter = ref("");
const selected = reactive(new Set<number>());
const expandedTask = ref<number | null>(null);
const runsByTask = ref<Record<number, Run[]>>({});
const stepsMap = reactive(new Map<number, RunStep[]>());
const expandedRun = ref<number | null>(null);
const liveRunId = ref<number | null>(null);
let liveWs: WebSocket | null = null;
const expandedSteps = computed(() => (expandedRun.value ? stepsMap.get(expandedRun.value) ?? [] : []));

interface TaskForm {
  id: number | null;
  name: string;
  cron: string;
  method: string;
  url: string;
  headersText: string;
  body: string;
  disabled: boolean;
  grp: string;
  templateId: number | null;
}
const blankTaskForm = (): TaskForm => ({ id: null, name: "", cron: "0 * * * * * *", method: "GET", url: "", headersText: "{}", body: "", disabled: false, grp: "", templateId: null });
const taskForm = reactive<TaskForm>(blankTaskForm());
const templatesForSelect = computed(() => templates.value);

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
  const payload: CreateTask = {
    name: taskForm.name,
    cron: taskForm.cron,
    method: taskForm.method,
    url: taskForm.url,
    headers,
    body: taskForm.body || null,
    disabled: taskForm.disabled,
    grp: taskForm.grp || null,
    template_id: taskForm.templateId,
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
}
function openEditTask(task: Task) {
  Object.assign(taskForm, {
    id: task.id,
    name: task.name,
    cron: task.cron,
    method: task.method,
    url: task.url,
    headersText: task.headers && typeof task.headers === "object" && !Array.isArray(task.headers) ? JSON.stringify(task.headers, null, 2) : "{}",
    body: task.body ?? "",
    disabled: task.disabled,
    grp: task.grp ?? "",
    templateId: task.template_id ?? null,
  });
  showCreate.value = true;
}

async function toggleTask(task: Task) {
  try {
    await api.updateTask(task.id, { disabled: !task.disabled });
    await loadTasks();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function runNow(task: Task) {
  try {
    await api.runTask(task.id);
    notify(t("runNow"));
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
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
async function toggleTaskRuns(task: Task) {
  if (expandedTask.value === task.id) {
    expandedTask.value = null;
    return;
  }
  expandedTask.value = task.id;
  await loadTaskRuns(task.id);
}
function closeLive() {
  if (liveWs) { liveWs.close(); liveWs = null; }
  liveRunId.value = null;
}
async function toggleRunSteps(run: Run) {
  if (expandedRun.value === run.id) {
    expandedRun.value = null;
    closeLive();
    return;
  }
  expandedRun.value = run.id;
  try {
    stepsMap.set(run.id, await api.runSteps(run.id));
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
  if (["pending", "leased", "running"].includes(run.status)) openLive(run.id);
}
function openLive(runId: number) {
  closeLive();
  liveRunId.value = runId;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/api/v1/runs/${runId}/steps/live`);
  liveWs = ws;
  ws.onmessage = (event) => {
    try {
      const msg = JSON.parse(event.data) as LiveRunEvent & { steps?: RunStep[] };
      if (msg.type === "snapshot" && msg.steps) {
        stepsMap.set(runId, msg.steps);
      } else if (msg.step) {
        const existing = stepsMap.get(runId) ?? [];
        const idx = existing.findIndex((s) => s.step_index === msg.step!.step_index);
        if (idx >= 0) existing[idx] = msg.step;
        else existing.push(msg.step);
        stepsMap.set(runId, [...existing]);
      } else if (msg.status) {
        for (const list of [runsByTask.value, allRuns.value]) {
          const target = (list as Run[]).find((r) => r.id === runId);
          if (target) target.status = msg.status;
        }
      } else if (msg.error) {
        notify(msg.error, "error");
      }
    } catch { /* ignore malformed frames */ }
  };
  ws.onclose = () => { if (liveRunId.value === runId) liveRunId.value = null; liveWs = null; };
  ws.onerror = () => { notify(t("genericError"), "error"); ws.close(); };
}
async function cancelRun(run: Run) {
  try {
    await api.cancelRun(run.id);
    if (expandedTask.value != null) await loadTaskRuns(expandedTask.value);
    if (view.value === "runs") await loadAllRuns();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function runStatusClass(status: string): string {
  if (status === "succeeded") return "run-ok";
  if (status === "failed") return "run-bad";
  if (status === "cancelled") return "run-cancelled";
  return "run-active";
}

// ---------- runs (global view) ----------
const allRuns = ref<Run[]>([]);
async function loadAllRuns() {
  try {
    const list = await api.tasks();
    const groups = await Promise.all(list.map((task) => api.taskRuns(task.id).catch(() => [] as Run[])));
    allRuns.value = groups.flat().sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0)).slice(0, 200);
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}

// ---------- templates ----------
const templates = ref<Template[]>([]);
const publicTemplates = ref<Template[]>([]);
const templateSearch = ref("");
const editingTemplateId = ref<number | null>(null);
const importForm = reactive({ name: "", description: "", har: "" });
const validating = ref(false);
const filteredTemplates = computed(() => {
  const term = templateSearch.value.trim().toLowerCase();
  return term ? templates.value.filter((x) => `${x.name} ${x.description ?? ""}`.toLowerCase().includes(term)) : templates.value;
});
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
}
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
  Object.assign(importForm, template
    ? { name: template.name, description: template.description ?? "", har: template.qd_har ? JSON.stringify(template.qd_har, null, 2) : "" }
    : { name: "", description: "", har: "" });
  showImport.value = true;
}
async function validateHar() {
  validating.value = true;
  try {
    const har = JSON.parse(importForm.har);
    const result = await api.validateQdHar(har);
    notify(`${t("validateOk")}（${result.entries} entries / ${result.requests} requests）`);
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  } finally { validating.value = false; }
}
async function saveImport() {
  try {
    const har = JSON.parse(importForm.har);
    if (editingTemplateId.value) await api.updateQdHar(editingTemplateId.value, importForm.name, importForm.description, har);
    else await api.importQdHar(importForm.name, importForm.description, har);
    notify(t("importDone"));
    showImport.value = false;
    editingTemplateId.value = null;
    Object.assign(importForm, { name: "", description: "", har: "" });
    await openTemplates();
  } catch (cause) {
    notify(cause instanceof Error ? cause.message : t("genericError"), "error");
  }
}

// ---------- notes ----------
const notes = ref<Note[]>([]);
const noteForm = reactive({ id: 0, title: "", content: "" });
async function openNotes() {
  view.value = "notes";
  try { notes.value = await api.notes(); } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
function editNote(note?: Note) {
  Object.assign(noteForm, note ? { id: note.id, title: note.title, content: note.content } : { id: 0, title: "", content: "" });
}
async function saveNote() {
  try {
    if (noteForm.id) await api.updateNote(noteForm.id, noteForm.title, noteForm.content);
    else await api.createNote(noteForm.title, noteForm.content);
    editNote();
    await openNotes();
  } catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
}
async function removeNote(id: number, title: string) {
  if (!window.confirm(fmt("deleteNoteConfirm", { title }))) return;
  try { await api.deleteNote(id); await openNotes(); }
  catch (cause) { notify(cause instanceof Error ? cause.message : t("genericError"), "error"); }
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
const channelForm = reactive({ name: "", kind: "webhook" as "webhook" | "email", url: "", to: "", subject: "" });
const actionForm = reactive({ taskId: 0, channelId: 0, event: "failure" });
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
    const config = channelForm.kind === "webhook" ? { url: channelForm.url } : { to: channelForm.to, ...(channelForm.subject ? { subject: channelForm.subject } : {}) };
    await api.createNotificationChannel(channelForm.name, channelForm.kind, config);
    Object.assign(channelForm, { name: "", kind: "webhook", url: "", to: "", subject: "" });
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
  allRuns.value = [];
  runsByTask.value = {};
  closeLive();
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
          <button type="button" class="secondary-button" @click="authMode = 'reset'">{{ t('resetTitle') }} →</button>
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
    </form>
  </main>

  <!-- ============ APP ============ -->
  <div v-else class="app-shell">
    <aside :class="['sidebar', { open: menuOpen }]">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <nav aria-label="主导航">
        <a :class="['nav-link', { active: view === 'tasks' }]" href="#" @click.prevent="view='tasks'"><LayoutDashboard :size="18" />{{ t('tasks') }}</a>
        <a :class="['nav-link', { active: view === 'templates' }]" href="#" @click.prevent="openTemplates"><FileJson2 :size="18" />{{ t('templates') }}</a>
        <a :class="['nav-link', { active: view === 'notes' }]" href="#" @click.prevent="openNotes"><ListChecks :size="18" />{{ t('notes') }}</a>
        <a :class="['nav-link', { active: view === 'plugins' }]" href="#" @click.prevent="openPlugins"><Settings :size="18" />{{ t('plugins') }}</a>
        <a :class="['nav-link', { active: view === 'notifications' }]" href="#" @click.prevent="openNotifications"><BellIcon :size="18" />{{ t('notifications') }}</a>
        <a :class="['nav-link', { active: view === 'runs' }]" href="#" @click.prevent="loadAllRuns; view='runs'"><Activity :size="18" />{{ t('runs') }}</a>
        <a :class="['nav-link', { active: view === 'subscriptions' }]" href="#" @click.prevent="openSubscriptions"><RefreshCw :size="18" />{{ t('subscriptions') }}</a>
        <a :class="['nav-link', { active: view === 'push' }]" href="#" @click.prevent="openPush"><SendIcon :size="18" />{{ t('push') }}</a>
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
        <button class="icon-button mobile-menu" title="☰" @click="menuOpen = true"><Menu :size="20" /></button>
        <div class="breadcrumb">{{ t('workspace') }} <span>/</span> {{ currentViewName }}</div>
        <div class="topbar-right">
          <span v-if="currentUser?.email && !currentUser.email_verified" class="verify-hint" :title="t('emailVerifyBanner')">
            <Mail :size="14" />{{ t('emailVerifyBanner') }}
            <button class="text-button" @click="resendVerification">{{ t('resendVerify') }}</button>
          </span>
          <button class="icon-button" :title="locale" @click="toggleLocale">{{ locale === 'zh-CN' ? 'EN' : '中' }}</button>
          <span class="account-name">{{ currentUser?.username }} · {{ currentUser?.role === 'admin' ? t('roleAdmin') : t('roleUser') }}</span>
          <button class="avatar" :title="t('logout')" @click="logout()">{{ currentUser?.username.slice(0, 2).toUpperCase() }}</button>
        </div>
      </header>

      <!-- ===== TASKS ===== -->
      <div v-if="view === 'tasks'" class="page">
        <section class="page-heading">
          <div><p class="eyebrow">AUTOMATION</p><h1>{{ t('tasks') }}</h1><p>{{ t('createFirst') }}</p></div>
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
            <button class="icon-button" title="×" @click="selected.clear()"><X :size="16" /></button>
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
                    <td><button :class="['status-pill', { paused: task.disabled }]" @click="toggleTask(task)"><span />{{ task.disabled ? t('paused') : t('running') }}</button></td>
                    <td>{{ task.grp ?? '–' }}</td>
                    <td class="row-actions">
                      <button class="icon-button" :title="t('runNow')" @click="runNow(task)"><Play :size="17" /></button>
                      <button class="icon-button" :title="t('runHistory')" @click="toggleTaskRuns(task)"><Activity :size="17" /></button>
                      <button class="icon-button" :title="t('editTask')" @click="openEditTask(task)"><Pencil :size="17" /></button>
                      <button class="icon-button" :title="t('deleteTask')" @click="removeTask(task)"><Trash2 :size="17" /></button>
                    </td>
                  </tr>
                  <tr v-if="expandedTask === task.id" class="run-detail">
                    <td colspan="7">
                      <div v-if="!runsByTask[task.id] || runsByTask[task.id].length === 0" class="muted">{{ t('noRuns') }}</div>
                      <div v-for="run in (runsByTask[task.id] ?? []).slice(0, 10)" :key="run.id" class="run-row">
                        <span class="run-id">#{{ run.id }}</span>
                        <strong :class="runStatusClass(run.status)">{{ run.status }}</strong>
                        <span v-if="run.http_status">HTTP {{ run.http_status }}</span>
                        <span v-if="run.error" class="error-text">{{ run.error }}</span>
                        <span class="run-time">{{ formatRunTime(run.started_at ?? run.created_at) }}</span>
                        <button v-if="['pending','leased','running'].includes(run.status)" class="icon-button" :title="t('cancelRun')" @click="cancelRun(run)"><X :size="15" /></button>
                        <button class="secondary-button" @click="toggleRunSteps(run)">{{ expandedRun === run.id ? t('close') : t('viewSteps') }} <span v-if="stepsMap.get(run.id)?.length">({{ stepsMap.get(run.id)!.length }})</span></button>
                      </div>
                    </td>
                  </tr>
                  <tr v-if="expandedTask === task.id && expandedRun && expandedSteps.length > 0" class="steps-detail">
                    <td colspan="7">
                      <div class="steps-panel">
                        <div class="steps-head">
                          <span>#{{ expandedRun }} · {{ t('runStatus') }}</span>
                          <span v-if="liveRunId === expandedRun" class="live-badge"><span class="live-dot" />live</span>
                        </div>
                        <table v-if="expandedSteps.length > 0" class="steps-table">
                          <thead><tr><th>#</th><th>{{ t('name') }}</th><th>{{ t('status') }}</th><th>{{ t('httpStatus') }}</th><th>bytes</th><th>{{ t('duration') }}</th><th>{{ t('error') }}</th></tr></thead>
                          <tbody>
                            <tr v-for="step in expandedSteps" :key="step.id">
                              <td>{{ step.step_index }}</td>
                              <td><code>{{ step.name }}</code></td>
                              <td><strong :class="runStatusClass(step.status)">{{ step.status }}</strong></td>
                              <td>{{ step.http_status ?? '–' }}</td>
                              <td>{{ step.body_size ?? 0 }}</td>
                              <td>{{ fmtDuration(step.finished_at && step.started_at ? step.finished_at - step.started_at : null) }}</td>
                              <td v-if="step.error" class="error-text">{{ step.error }}</td>
                              <td v-else>–</td>
                            </tr>
                          </tbody>
                        </table>
                        <div v-else class="muted">{{ t('loading') }}</div>
                      </div>
                    </td>
                  </tr>
                </template>
              </tbody>
            </table>
          </div>
        </section>
      </div>

      <!-- ===== TEMPLATES ===== -->
      <div v-else-if="view === 'templates'" class="page">
        <section class="page-heading">
          <div><p class="eyebrow">TEMPLATES</p><h1>{{ t('templates') }}</h1><p>{{ t('templateHint') }}</p></div>
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
            <button v-if="item.source_format === 'qd_har'" class="secondary-button" @click="openImportModal(item)"><Pencil :size="14" />{{ t('editTemplate') }}</button>
            <button class="secondary-button" @click="publishTemplate(item.id)">{{ t('publish') }}</button>
            <button class="secondary-button" @click="unpublishTemplate(item.id)">{{ t('unpublish') }}</button>
            <button class="icon-button" :title="t('deleteTemplate')" @click="removeTemplate(item.id, item.name)"><Trash2 :size="16" /></button>
          </div>
          <h2>{{ t('publicTemplates') }}</h2>
          <div v-if="publicTemplates.length === 0" class="muted">{{ t('noTemplates') }}</div>
          <div v-for="item in publicTemplates" :key="item.id" class="run-row">
            <strong>{{ item.name }}</strong>
            <span>{{ item.source_format }}</span>
            <span v-if="item.description" class="muted">{{ item.description }}</span>
            <button class="secondary-button" @click="copyTemplate(item.id)">{{ t('copy') }}</button>
          </div>
        </section>
      </div>

      <!-- ===== NOTES ===== -->
      <div v-else-if="view === 'notes'" class="page">
        <section class="page-heading"><div><p class="eyebrow">NOTES</p><h1>{{ t('notesTitle') }}</h1><p>{{ t('notesHint') }}</p></div></section>
        <section class="task-section">
          <form class="modal inline-modal" @submit.prevent="saveNote">
            <label>{{ t('noteTitle') }}<input v-model="noteForm.title" required /></label>
            <label>{{ t('noteContent') }}<textarea v-model="noteForm.content" rows="8" /></label>
            <button class="primary-button">{{ noteForm.id ? t('saveNote') : t('newNote') }}</button>
          </form>
          <div v-if="notes.length === 0" class="muted">{{ t('noNotes') }}</div>
          <div v-for="note in notes" :key="note.id" class="run-row">
            <strong>{{ note.title }}</strong>
            <span class="muted">{{ note.content.slice(0, 60) }}</span>
            <button class="secondary-button" @click="editNote(note)">{{ t('edit') }}</button>
            <button class="icon-button" :title="t('deleteNote')" @click="removeNote(note.id, note.title)"><Trash2 :size="16" /></button>
          </div>
        </section>
      </div>

      <!-- ===== PLUGINS ===== -->
      <div v-else-if="view === 'plugins'" class="page">
        <section class="page-heading"><div><p class="eyebrow">PLUGINS</p><h1>{{ t('pluginsTitle') }}</h1></div></section>
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
        <section class="page-heading"><div><p class="eyebrow">NOTIFICATIONS</p><h1>{{ t('notificationsTitle') }}</h1><p>{{ t('notificationsHint') }}</p></div></section>
        <section class="task-section">
          <form class="modal inline-modal" @submit.prevent="saveChannel">
            <label>{{ t('channelName') }}<input v-model="channelForm.name" required /></label>
            <label>{{ t('channelKind') }}
              <select v-model="channelForm.kind">
                <option value="webhook">{{ t('webhookKind') }}</option>
                <option value="email">{{ t('emailKind') }}</option>
              </select>
            </label>
            <template v-if="channelForm.kind === 'webhook'">
              <label>{{ t('webhookUrl') }}<input v-model="channelForm.url" required type="url" placeholder="https://example.com/hook" /></label>
            </template>
            <template v-else>
              <label>{{ t('emailTo') }}<input v-model="channelForm.to" required type="email" /></label>
              <label>{{ t('emailSubject') }}<input v-model="channelForm.subject" /></label>
            </template>
            <button class="primary-button">{{ channelForm.kind === 'webhook' ? t('createWebhook') : t('createEmailChannel') }}</button>
          </form>
          <div v-if="channels.length === 0" class="muted">{{ t('noChannels') }}</div>
          <div v-for="channel in channels" :key="channel.id" class="run-row">
            <strong>{{ channel.name }}</strong>
            <span class="chip">{{ channel.kind === 'webhook' ? t('webhookKind') : t('emailKind') }}</span>
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

      <!-- ===== RUNS (global) ===== -->
      <div v-else-if="view === 'runs'" class="page">
        <section class="page-heading">
          <div><p class="eyebrow">RUN HISTORY</p><h1>{{ t('runsTitle') }}</h1><p>{{ t('runsHint') }}</p></div>
          <button class="secondary-button" @click="loadAllRuns"><RefreshCw :size="16" />{{ t('refresh') }}</button>
        </section>
        <section class="task-section">
          <div v-if="allRuns.length === 0" class="empty-state"><Activity :size="24" /><h2>{{ t('noRuns') }}</h2></div>
          <div v-else class="run-list">
            <div v-for="run in allRuns" :key="run.id" class="run-row clickable" @click="toggleRunSteps(run)">
              <span class="run-id">#{{ run.id }}</span>
              <strong>{{ taskName(run.task_id) }}</strong>
              <strong :class="runStatusClass(run.status)">{{ run.status }}</strong>
              <span v-if="run.http_status">{{ t('httpStatus') }} {{ run.http_status }}</span>
              <span v-if="run.error" class="error-text">{{ run.error }}</span>
              <span class="run-time">{{ formatRunTime(run.started_at ?? run.created_at) }}</span>
              <span v-if="liveRunId === run.id" class="live-badge"><span class="live-dot" />live</span>
              <button v-if="['pending','leased','running'].includes(run.status)" class="icon-button" :title="t('cancelRun')" @click.stop="cancelRun(run)"><X :size="15" /></button>
            </div>
            <div v-if="expandedRun && expandedSteps.length > 0" class="steps-panel">
              <div class="steps-head"><span>#{{ expandedRun }} · {{ t('runStatus') }}</span><span v-if="liveRunId === expandedRun" class="live-badge"><span class="live-dot" />live</span></div>
              <table v-if="expandedSteps.length" class="steps-table">
                <thead><tr><th>#</th><th>{{ t('name') }}</th><th>{{ t('status') }}</th><th>{{ t('httpStatus') }}</th><th>bytes</th><th>{{ t('duration') }}</th><th>{{ t('error') }}</th></tr></thead>
                <tbody>
                  <tr v-for="step in expandedSteps" :key="step.id">
                    <td>{{ step.step_index }}</td><td><code>{{ step.name }}</code></td>
                    <td><strong :class="runStatusClass(step.status)">{{ step.status }}</strong></td>
                    <td>{{ step.http_status ?? '–' }}</td><td>{{ step.body_size ?? 0 }}</td>
                    <td>{{ fmtDuration(step.finished_at && step.started_at ? step.finished_at - step.started_at : null) }}</td>
                    <td v-if="step.error" class="error-text">{{ step.error }}</td><td v-else>–</td>
                  </tr>
                </tbody>
              </table>
              <div v-else class="muted">{{ t('loading') }}</div>
            </div>
          </div>
        </section>
      </div>

      <!-- ===== SUBSCRIPTIONS ===== -->
      <div v-else-if="view === 'subscriptions'" class="page">
        <section class="page-heading"><div><p class="eyebrow">TEMPLATE SOURCES</p><h1>{{ t('subscriptionsTitle') }}</h1><p>{{ t('subHint') }}</p></div></section>
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
        <section class="page-heading"><div><p class="eyebrow">PUBLISH REVIEW</p><h1>{{ t('pushTitle') }}</h1><p>{{ t('pushHint') }}</p></div></section>
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
        <section class="page-heading"><div><p class="eyebrow">ADMINISTRATION</p><h1>{{ t('adminTitle') }}</h1><p>{{ t('adminHint') }}</p></div></section>
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
        <section class="page-heading"><div><p class="eyebrow">WORKSPACE</p><h1>{{ t('settingsTitle') }}</h1></div></section>
        <section class="settings-grid">
          <div class="content-panel">
            <p class="eyebrow">ACCOUNT</p><h2>{{ t('account') }}</h2>
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
            <p class="eyebrow">SERVICE</p><h2>{{ t('service') }}</h2>
            <dl class="settings-list">
              <div><dt>{{ t('statusLabel') }}</dt><dd><span class="state-dot online" />{{ ready ? t('serviceOk') : t('connecting') }}</dd></div>
              <div><dt>{{ t('localeLabel') }}</dt><dd><button class="secondary-button" @click="toggleLocale">{{ locale === 'zh-CN' ? 'EN' : '中' }}</button></dd></div>
              <div><dt>{{ t('apiDocs') }}</dt><dd><a href="/api/v1/openapi.json" target="_blank">{{ t('openapi') }}</a></dd></div>
            </dl>
          </div>
        </section>
      </div>
    </main>

    <!-- ===== CREATE / EDIT TASK MODAL ===== -->
    <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
      <form class="modal" @submit.prevent="submitTask">
        <div class="modal-header">
          <div><p class="eyebrow">{{ taskForm.id ? 'EDIT' : 'NEW' }} AUTOMATION</p><h2>{{ taskForm.id ? t('editTask') : t('newTask') }}</h2></div>
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
          <label>{{ t('cron') }}<input v-model="taskForm.cron" required /></label>
        </div>
        <label>{{ t('url') }}<input v-model="taskForm.url" required type="url" placeholder="https://example.com/api/health" /></label>
        <label>{{ t('group') }}<input v-model="taskForm.grp" list="grp-options" :placeholder="t('group')" /></label>
        <datalist id="grp-options"><option v-for="g in taskGroups" :key="g" :value="g" /></datalist>
        <label>{{ t('requestHeaders') }}<textarea v-model="taskForm.headersText" rows="4" spellcheck="false" /></label>
        <label>{{ t('body') }}<textarea v-model="taskForm.body" rows="3" spellcheck="false" /></label>
        <label class="checkbox"><input v-model="taskForm.disabled" type="checkbox" />{{ t('createPaused') }}</label>
        <div class="modal-actions">
          <button class="secondary-button" type="button" @click="showCreate = false">{{ t('cancel') }}</button>
          <button class="primary-button" type="submit">{{ taskForm.id ? t('saveTask') : t('createTask') }}</button>
        </div>
      </form>
    </div>

    <!-- ===== IMPORT MODAL ===== -->
    <div v-if="showImport" class="modal-backdrop" @click.self="showImport = false">
      <form class="modal modal-wide" @submit.prevent="saveImport">
        <div class="modal-header">
          <div><p class="eyebrow">QD HAR</p><h2>{{ t('importHarTitle') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="showImport = false"><X :size="20" /></button>
        </div>
        <label>{{ t('templateName') }}<input v-model="importForm.name" required /></label>
        <label>{{ t('description') }}<input v-model="importForm.description" /></label>
        <label>{{ t('harJson') }}<textarea v-model="importForm.har" required rows="14" spellcheck="false" /></label>
        <div class="modal-actions">
          <button class="secondary-button" type="button" @click="showImport = false">{{ t('cancel') }}</button>
          <button class="secondary-button" type="button" :disabled="validating" @click="validateHar">{{ validating ? t('loading') : t('check') }}</button>
          <button class="primary-button" type="submit">{{ editingTemplateId ? t('save') : t('import') }}</button>
        </div>
      </form>
    </div>

    <!-- ===== HELP MODAL ===== -->
    <div v-if="showHelp" class="modal-backdrop" @click.self="showHelp = false">
      <div class="modal">
        <div class="modal-header">
          <div><p class="eyebrow">HELP</p><h2>{{ t('helpTitle') }}</h2></div>
          <button class="icon-button" type="button" :title="t('close')" @click="showHelp = false"><X :size="20" /></button>
        </div>
        <p class="auth-hint">{{ t('helpBody') }}</p>
        <div class="modal-actions"><button class="secondary-button" type="button" @click="showHelp = false">{{ t('close') }}</button></div>
      </div>
    </div>

    <!-- ===== TOASTS ===== -->
    <div class="toast-stack" aria-live="polite">
      <div v-for="toast in toasts" :key="toast.id" :class="['toast', toast.kind]">
        <CheckCircle2 v-if="toast.kind === 'success'" :size="16" />
        <XCircle v-else :size="16" />
        <span>{{ toast.message }}</span>
        <button class="icon-button" @click="toasts = toasts.filter(x => x.id !== toast.id)"><X :size="14" /></button>
      </div>
    </div>
  </div>
</template>
