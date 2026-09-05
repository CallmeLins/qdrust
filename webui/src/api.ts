import type { components } from "./generated/api";

export type Task = components["schemas"]["Task"];
export type CreateTask = components["schemas"]["CreateTask"];
export type UpdateTask = components["schemas"]["UpdateTask"];
export type ApiError = components["schemas"]["ApiError"];
export type Run = components["schemas"]["Run"];
export type RunStep = {
  id: number;
  run_id: number;
  step_index: number;
  name: string;
  status: string;
  http_status: number | null;
  body_size: number;
  error: string | null;
  started_at: number;
  finished_at: number;
};
export type User = components["schemas"]["User"];
export type Template = components["schemas"]["Template"];
export type Plugin = components["schemas"]["Plugin"];
export type NotificationChannel = components["schemas"]["NotificationChannel"];
export type NotificationAction = components["schemas"]["NotificationAction"];
export type TemplateSubscription = components["schemas"]["TemplateSubscription"];
export type SubscriptionSync = components["schemas"]["SubscriptionSync"];
export type PushRequest = components["schemas"]["PushRequest"];
export type SiteSetting = components["schemas"]["SiteSetting"];
export type BatchTaskResult = components["schemas"]["BatchTaskResult"];
export type QdHarValidation = components["schemas"]["QdHarValidation"];

/** Paginated response returned by GET /api/v1/templates */
export interface TemplatePage {
  items: Template[];
  has_more: boolean;
  next_cursor: number | null;
}

/** Live run-step WebSocket event */
export interface LiveRunEvent {
  run_id: number;
  type: string;
  status?: string | null;
  step?: RunStep | null;
  error?: string | null;
}

function csrfToken(): string | undefined {
  return document.cookie.split("; ").find((value) => value.startsWith("qd_csrf="))?.split("=")[1];
}

// The UI is built with a RELATIVE base (or an explicit VITE_BASE_PATH), so the
// actual URL prefix is only known at runtime. Derive it from the current page
// path so ONE image works at the bare root "/" or any reverse-proxied
// sub-directory such as /qd — no rebuild needed.
//
// The SPA has no vue-router: the backend serves index.html for the root and the
// email deep-link paths (`/verify-email`, `/reset-password`, see App.vue) under
// any prefix. Everything else that loads the app is one of those, so stripping
// the trailing "/" and an optional auth deep-link segment leaves exactly the
// reverse-proxy prefix.
const PAGE_TAIL = ["/verify-email", "/reset-password"];

/** Pure helper: turn a full page pathname into the reverse-proxy prefix.
 *  e.g. "/" -> "", "/qd/" -> "/qd", "/qd/verify?..." -> "/qd", "" -> "". */
export function prefixFromPathname(pathname: string): string {
  let path = pathname.replace(/\/+$/, "");
  for (const tail of PAGE_TAIL) {
    if (path.endsWith(tail)) {
      path = path.slice(0, -tail.length).replace(/\/+$/, "");
      break;
    }
  }
  // Keep the leading slash of a real prefix ("/qd"), but never bare "/".
  return path === "/" ? "" : path;
}

/** Derive the current reverse-proxy prefix from window.location, e.g. "" for a
 *  bare-root deployment or "/qd" when served under /qd. Returns "" when no
 *  browser context is available (e.g. unit tests / non-DOM). */
export function detectUrlPrefix(): string {
  if (typeof window === "undefined") return "";
  return prefixFromPathname(window.location.pathname);
}

/** Prefix an API path with the reverse-proxy base. Liveness/readiness probes
 *  are intentionally left at the bare root (the backend keeps them un-nested).
 *  `prefix` is injectable for tests and defaults to the runtime-derived one. */
export function apiPath(path: string, prefix: string = detectUrlPrefix()): string {
  if (!prefix || path.startsWith("/health") || path.startsWith("/ready")) {
    return path;
  }
  return prefix + path;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(apiPath(path), {
    ...init,
    credentials: "same-origin",
    headers: { "content-type": "application/json", ...(csrfToken() ? { "x-csrf-token": decodeURIComponent(csrfToken()!) } : {}), ...init?.headers }
  });
  if (!response.ok) {
    const payload = await response.json().catch(() => null) as ApiError | null;
    throw new Error(payload?.message ?? response.statusText ?? "请求失败");
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

export const api = {
  // ---- auth ----
  session: () => request<{ user: User; expires_at: number }>("/api/v1/auth/session"),
  bootstrap: (username: string, password: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/bootstrap", { method: "POST", body: JSON.stringify({ username, password }) }),
  register: (username: string, password: string, email?: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/register", { method: "POST", body: JSON.stringify({ username, password, ...(email ? { email } : {}) }) }),
  login: (username: string, password: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username, password }) }),
  logout: () => request<void>("/api/v1/auth/logout", { method: "POST" }),
  changePassword: (currentPassword: string, newPassword: string) => request<{ ok: boolean }>("/api/v1/auth/password", { method: "POST", body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }) }),
  forgotPassword: (username: string) => request<{ sent: boolean; reset_token?: string; reset_url?: string }>("/api/v1/auth/forgot-password", { method: "POST", body: JSON.stringify({ username }) }),
  resetPassword: (token: string, newPassword: string) => request<{ ok: boolean }>("/api/v1/auth/reset-password", { method: "POST", body: JSON.stringify({ token, new_password: newPassword }) }),
  verifyEmail: (token: string) => request<{ ok: boolean }>("/api/v1/auth/verify-email", { method: "POST", body: JSON.stringify({ token }) }),
  resendVerification: () => request<{ sent: boolean; verify_token?: string; expires_at?: number }>("/api/v1/auth/resend-verification", { method: "POST" }),
  rotateCsrf: () => request<{ csrf_token: string }>("/api/v1/auth/csrf/rotate", { method: "POST" }),

  // ---- templates ----
  /** Returns the item array from the paginated list response. */
  templates: async (q?: string, grp?: string, limit = 100) => {
    const params = new URLSearchParams();
    if (q) params.set("q", q);
    if (grp) params.set("grp", grp);
    params.set("limit", String(limit));
    const page = await request<TemplatePage>(`/api/v1/templates?${params.toString()}`);
    return page.items;
  },
  templateGroups: () => request<string[]>("/api/v1/task-groups"),
  publicTemplates: () => request<Template[]>("/api/v1/public-templates"),
  publishTemplate: (id: number) => request<void>(`/api/v1/templates/${id}/publish`, { method: "POST" }),
  unpublishTemplate: (id: number) => request<void>(`/api/v1/templates/${id}/publish`, { method: "DELETE" }),
  copyPublicTemplate: (id: number) => request<Template>(`/api/v1/public-templates/${id}/copy`, { method: "POST" }),
  deleteTemplate: (id: number) => request<void>(`/api/v1/templates/${id}`, { method: "DELETE" }),
  importQdHar: (name: string, description: string, har: unknown) => request<Template>("/api/v1/templates/import-qd-har", { method: "POST", body: JSON.stringify({ name, description: description || null, har }) }),
  updateQdHar: (id: number, name: string, description: string, har: unknown) => request<Template>(`/api/v1/templates/${id}/qd-har`, { method: "PUT", body: JSON.stringify({ name, description: description || null, har }) }),
  validateQdHar: (har: unknown) => request<QdHarValidation>("/api/v1/templates/validate-qd-har", { method: "POST", body: JSON.stringify({ har }) }),

  // ---- plugins ----
  plugins: () => request<Plugin[]>("/api/v1/plugins"),
  createPlugin: (name: string, command: string) => request<Plugin>("/api/v1/plugins", { method: "POST", body: JSON.stringify({ name, command, config: {}, enabled: true }) }),
  updatePlugin: (id: number, enabled: boolean) => request<Plugin>(`/api/v1/plugins/${id}`, { method: "PUT", body: JSON.stringify({ enabled }) }),
  deletePlugin: (id: number) => request<void>(`/api/v1/plugins/${id}`, { method: "DELETE" }),
  invokePlugin: (id: number, action: string, query: Record<string, string>) => request<unknown>(`/api/v1/plugins/${id}/invoke`, { method: "POST", body: JSON.stringify({ action, query }) }),

  // ---- notifications ----
  notificationChannels: () => request<NotificationChannel[]>("/api/v1/notification-channels"),
  createNotificationChannel: (name: string, kind: NotificationChannel["kind"], config: Record<string, unknown>) => request<NotificationChannel>("/api/v1/notification-channels", { method: "POST", body: JSON.stringify({ name, kind, config, enabled: true }) }),
  updateNotificationChannel: (id: number, enabled: boolean) => request<NotificationChannel>(`/api/v1/notification-channels/${id}`, { method: "PUT", body: JSON.stringify({ enabled }) }),
  deleteNotificationChannel: (id: number) => request<void>(`/api/v1/notification-channels/${id}`, { method: "DELETE" }),
  notificationActions: (taskId: number) => request<NotificationAction[]>(`/api/v1/tasks/${taskId}/notification-actions`),
  createNotificationAction: (taskId: number, channelId: number, event: string) => request<NotificationAction>(`/api/v1/tasks/${taskId}/notification-actions`, { method: "POST", body: JSON.stringify({ channel_id: channelId, event }) }),
  deleteNotificationAction: (id: number) => request<void>(`/api/v1/notification-actions/${id}`, { method: "DELETE" }),

  // ---- tasks ----
  tasks: () => request<Task[]>("/api/v1/tasks"),
  createTask: (input: CreateTask) => request<Task>("/api/v1/tasks", { method: "POST", body: JSON.stringify(input) }),
  updateTask: (id: number, input: UpdateTask) => request<Task>(`/api/v1/tasks/${id}`, { method: "PUT", body: JSON.stringify(input) }),
  deleteTask: (id: number) => request<void>(`/api/v1/tasks/${id}`, { method: "DELETE" }),
  runTask: (id: number) => request<Run>(`/api/v1/tasks/${id}/run`, { method: "POST" }),
  cancelRun: (id: number) => request<void>(`/api/v1/runs/${id}/cancel`, { method: "POST" }),
  taskRuns: (id: number) => request<Run[]>(`/api/v1/tasks/${id}/runs`),
  deleteTaskRuns: (id: number) => request<{ deleted?: number }>(`/api/v1/tasks/${id}/runs`, { method: "DELETE" }),
  deleteRun: (id: number) => request<void>(`/api/v1/runs/${id}`, { method: "DELETE" }),
  runSteps: (id: number) => request<RunStep[]>(`/api/v1/runs/${id}/steps`),
  batchTasks: (ids: number[], action: "enable" | "disable" | "delete" | "run") => request<BatchTaskResult>("/api/v1/tasks/batch", { method: "POST", body: JSON.stringify({ ids, action }) }),
  taskGroups: () => request<string[]>("/api/v1/task-groups"),

  // ---- subscriptions ----
  subscriptions: () => request<TemplateSubscription[]>("/api/v1/subscriptions"),
  createSubscription: (name: string, url: string) => request<TemplateSubscription>("/api/v1/subscriptions", { method: "POST", body: JSON.stringify({ name, url }) }),
  updateSubscription: (id: number, enabled: boolean) => request<TemplateSubscription>(`/api/v1/subscriptions/${id}`, { method: "PUT", body: JSON.stringify({ enabled }) }),
  deleteSubscription: (id: number) => request<void>(`/api/v1/subscriptions/${id}`, { method: "DELETE" }),
  syncSubscription: (id: number) => request<{ status: string }>(`/api/v1/subscriptions/${id}/sync`, { method: "POST" }),
  subscriptionSyncs: (id: number) => request<SubscriptionSync[]>(`/api/v1/subscriptions/${id}/syncs`),

  // ---- push requests ----
  myPushRequests: () => request<PushRequest[]>("/api/v1/push-requests"),
  createPushRequest: (templateId: number, note: string) => request<PushRequest>("/api/v1/push-requests", { method: "POST", body: JSON.stringify({ template_id: templateId, note: note || null }) }),
  adminPushRequests: (status?: string) => request<PushRequest[]>(`/api/v1/admin/push-requests${status ? `?status=${status}` : ""}`),
  decidePushRequest: (id: number, approve: boolean) => request<PushRequest>(`/api/v1/admin/push-requests/${id}/decision`, { method: "POST", body: JSON.stringify({ approve }) }),

  // ---- admin ----
  adminUsers: () => request<User[]>("/api/v1/admin/users"),
  adminUpdateUser: (id: number, input: { disabled?: boolean; role?: string }) => request<User>(`/api/v1/admin/users/${id}`, { method: "PATCH", body: JSON.stringify(input) }),
  adminDeleteUser: (id: number) => request<{ ok: boolean }>(`/api/v1/admin/users/${id}`, { method: "DELETE" }),
  adminSettings: () => request<SiteSetting[]>("/api/v1/admin/settings"),
  adminSetSetting: (key: string, value: unknown) => request<SiteSetting>(`/api/v1/admin/settings/${key}`, { method: "PUT", body: JSON.stringify({ value }) }),
  adminBackup: () => request<unknown>("/api/v1/admin/backup"),
  adminRestore: (backup: unknown) => request<{ ok: boolean }>("/api/v1/admin/restore", { method: "POST", body: JSON.stringify(backup) }),
  adminClearLogs: (olderThanDays: number) => request<{ deleted: number }>("/api/v1/admin/logs", { method: "DELETE", body: JSON.stringify({ older_than_days: olderThanDays }) }),

  ready: () => request<{ status: string }>("/ready"),
};
