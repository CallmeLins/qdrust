import type { components } from "./generated/api";

export type Task = components["schemas"]["Task"];
export type CreateTask = components["schemas"]["CreateTask"];
export type UpdateTask = components["schemas"]["UpdateTask"];
export type ApiError = components["schemas"]["ApiError"];
export type Run = components["schemas"]["Run"];
export type RunStep = { id: number; run_id: number; step_index: number; name: string; status: string; http_status: number | null; body_size: number; error: string | null; started_at: number; finished_at: number };
export type User = components["schemas"]["User"];
export type Template = components["schemas"]["Template"];
export type Note = components["schemas"]["Note"];
export type Plugin = components["schemas"]["Plugin"];
export type NotificationChannel = components["schemas"]["NotificationChannel"];
export type NotificationAction = components["schemas"]["NotificationAction"];
export type TemplateSubscription = components["schemas"]["TemplateSubscription"];
export type SubscriptionSync = components["schemas"]["SubscriptionSync"];
export type PushRequest = components["schemas"]["PushRequest"];
export type SiteSetting = components["schemas"]["SiteSetting"];

function csrfToken(): string | undefined {
  return document.cookie.split("; ").find((value) => value.startsWith("qd_csrf="))?.split("=")[1];
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
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
  session: () => request<{ user: User; expires_at: number }>("/api/v1/auth/session"),
  bootstrap: (username: string, password: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/bootstrap", { method: "POST", body: JSON.stringify({ username, password }) }),
  register: (username: string, password: string, email?: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/register", { method: "POST", body: JSON.stringify({ username, password, ...(email ? { email } : {}) }) }),
  login: (username: string, password: string) => request<{ user: User; expires_at: number }>("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username, password }) }),
  logout: () => request<void>("/api/v1/auth/logout", { method: "POST" }),
  templates: () => request<Template[]>("/api/v1/templates"),
  publicTemplates: () => request<Template[]>("/api/v1/public-templates"),
  publishTemplate: (id: number) => request<void>(`/api/v1/templates/${id}/publish`, { method: "POST" }),
  unpublishTemplate: (id: number) => request<void>(`/api/v1/templates/${id}/publish`, { method: "DELETE" }),
  copyPublicTemplate: (id: number) => request<Template>(`/api/v1/public-templates/${id}/copy`, { method: "POST" }),
  deleteTemplate: (id: number) => request<void>(`/api/v1/templates/${id}`, { method: "DELETE" }),
  importQdHar: (name: string, description: string, har: unknown) => request<Template>("/api/v1/templates/import-qd-har", { method: "POST", body: JSON.stringify({ name, description: description || null, har }) }),
  updateQdHar: (id: number, name: string, description: string, har: unknown) => request<Template>(`/api/v1/templates/${id}/qd-har`, { method: "PUT", body: JSON.stringify({ name, description: description || null, har }) }),
  notes: () => request<Note[]>("/api/v1/notes"),
  createNote: (title: string, content: string) => request<Note>("/api/v1/notes", { method: "POST", body: JSON.stringify({ title, content }) }),
  updateNote: (id: number, title: string, content: string) => request<Note>(`/api/v1/notes/${id}`, { method: "PUT", body: JSON.stringify({ title, content }) }),
  deleteNote: (id: number) => request<void>(`/api/v1/notes/${id}`, { method: "DELETE" }),
  plugins: () => request<Plugin[]>("/api/v1/plugins"),
  createPlugin: (name:string,command:string) => request<Plugin>("/api/v1/plugins", {method:"POST",body:JSON.stringify({name,command,config:{},enabled:true})}),
  updatePlugin: (id:number,enabled:boolean) => request<Plugin>(`/api/v1/plugins/${id}`, {method:"PUT",body:JSON.stringify({enabled})}),
  deletePlugin: (id:number) => request<void>(`/api/v1/plugins/${id}`, {method:"DELETE"}),
  invokePlugin:(id:number,action:string,query:Record<string,string>)=>request<unknown>(`/api/v1/plugins/${id}/invoke`,{method:"POST",body:JSON.stringify({action,query})}),
  notificationChannels:()=>request<NotificationChannel[]>("/api/v1/notification-channels"),
  createWebhook:(name:string,url:string)=>request<NotificationChannel>("/api/v1/notification-channels",{method:"POST",body:JSON.stringify({name,kind:"webhook",config:{url},enabled:true})}),
  updateNotificationChannel:(id:number,enabled:boolean)=>request<NotificationChannel>(`/api/v1/notification-channels/${id}`,{method:"PUT",body:JSON.stringify({enabled})}),
  deleteNotificationChannel:(id:number)=>request<void>(`/api/v1/notification-channels/${id}`,{method:"DELETE"}),
  notificationActions:(taskId:number)=>request<NotificationAction[]>(`/api/v1/tasks/${taskId}/notification-actions`),
  createNotificationAction:(taskId:number,channelId:number,event:string)=>request<NotificationAction>(`/api/v1/tasks/${taskId}/notification-actions`,{method:"POST",body:JSON.stringify({channel_id:channelId,event})}),
  deleteNotificationAction:(id:number)=>request<void>(`/api/v1/notification-actions/${id}`,{method:"DELETE"}),
  tasks: () => request<Task[]>("/api/v1/tasks"),
  createTask: (input: CreateTask) => request<Task>("/api/v1/tasks", { method: "POST", body: JSON.stringify(input) }),
  updateTask: (id: number, input: UpdateTask) => request<Task>(`/api/v1/tasks/${id}`, { method: "PUT", body: JSON.stringify(input) }),
  deleteTask: (id: number) => request<void>(`/api/v1/tasks/${id}`, { method: "DELETE" }),
  runTask: (id: number) => request<unknown>(`/api/v1/tasks/${id}/run`, { method: "POST" }),
  cancelRun: (id: number) => request<void>(`/api/v1/runs/${id}/cancel`, { method: "POST" }),
  taskRuns: (id: number) => request<Run[]>(`/api/v1/tasks/${id}/runs`),
  runSteps: (id: number) => request<RunStep[]>(`/api/v1/runs/${id}/steps`),
  ready: () => request<{ status: string }>("/ready"),
  // P1 features
  rotateCsrf: () => request<{ csrf_token: string }>("/api/v1/auth/csrf/rotate", { method: "POST" }),
  verifyEmail: (token: string) => request<{ ok: boolean }>("/api/v1/auth/verify-email", { method: "POST", body: JSON.stringify({ token }) }),
  resendVerification: () => request<{ sent: boolean }>("/api/v1/auth/resend-verification", { method: "POST" }),
  subscriptions: () => request<TemplateSubscription[]>("/api/v1/subscriptions"),
  createSubscription: (name: string, url: string) => request<TemplateSubscription>("/api/v1/subscriptions", { method: "POST", body: JSON.stringify({ name, url }) }),
  updateSubscription: (id: number, enabled: boolean) => request<TemplateSubscription>(`/api/v1/subscriptions/${id}`, { method: "PUT", body: JSON.stringify({ enabled }) }),
  deleteSubscription: (id: number) => request<void>(`/api/v1/subscriptions/${id}`, { method: "DELETE" }),
  syncSubscription: (id: number) => request<{ status: string }>(`/api/v1/subscriptions/${id}/sync`, { method: "POST" }),
  subscriptionSyncs: (id: number) => request<SubscriptionSync[]>(`/api/v1/subscriptions/${id}/syncs`),
  myPushRequests: () => request<PushRequest[]>("/api/v1/push-requests"),
  createPushRequest: (templateId: number, note: string) => request<PushRequest>("/api/v1/push-requests", { method: "POST", body: JSON.stringify({ template_id: templateId, note: note || null }) }),
  adminPushRequests: (status?: string) => request<PushRequest[]>(`/api/v1/admin/push-requests${status ? `?status=${status}` : ""}`),
  decidePushRequest: (id: number, approve: boolean) => request<PushRequest>(`/api/v1/admin/push-requests/${id}/decision`, { method: "POST", body: JSON.stringify({ approve }) }),
  adminUsers: () => request<User[]>("/api/v1/admin/users"),
  adminUpdateUser: (id: number, input: { disabled?: boolean; role?: string }) => request<User>(`/api/v1/admin/users/${id}`, { method: "PATCH", body: JSON.stringify(input) }),
  adminSettings: () => request<SiteSetting[]>("/api/v1/admin/settings"),
  adminSetSetting: (key: string, value: unknown) => request<SiteSetting>(`/api/v1/admin/settings/${key}`, { method: "PUT", body: JSON.stringify({ value }) }),
  adminBackup: () => request<unknown>("/api/v1/admin/backup"),
  adminRestore: (backup: unknown) => request<{ ok: boolean }>("/api/v1/admin/restore", { method: "POST", body: JSON.stringify(backup) }),
  adminClearLogs: (olderThanDays: number) => request<{ deleted: number }>("/api/v1/admin/logs", { method: "DELETE", body: JSON.stringify({ older_than_days: olderThanDays }) }),
};
