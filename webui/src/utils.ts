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

// ---------- 任务表单：模板下拉 ----------

/**
 * Order the "bind to template" dropdown so the templates no task uses yet come
 * first, each bucket keeping the order it arrived in.
 *
 * A template that already backs a task is rarely the one being looked for when
 * adding one, and in a library of hundreds the unused ones cannot be found by
 * scrolling — they are the reason someone opens this list at all. Reading the
 * ids in use rather than the tasks themselves keeps the rule testable without a
 * component harness, which this repo has none of.
 */
export function unboundTemplatesFirst<T extends { id: number }>(
  templates: T[],
  usedIds: Iterable<number>
): T[] {
  const used = new Set(usedIds);
  return [
    ...templates.filter((template) => !used.has(template.id)),
    ...templates.filter((template) => used.has(template.id)),
  ];
}

/** Newest first, the order the templates table shows by default.
 *
 *  The table sorts by `updated_at` in the component; the dropdown reads the
 *  same array in the order the server sent it, which is creation-id order. That
 *  made the two disagree — the list said one thing, the list you pick from said
 *  another — so both now ask for the same order. A copy is sorted, not the
 *  caller's array: this feeds a `computed` that must not mutate its source. */
export function newestTemplatesFirst<T extends { updated_at: number }>(templates: T[]): T[] {
  return [...templates].sort((a, b) => b.updated_at - a.updated_at);
}

/**
 * The create-task dropdown's order: templates no task uses yet first (issue
 * #25), newest first inside each half.
 *
 *  Both halves matter. "Unused first" is what someone adding a task is looking
 *  for; "newest first" is what every other list in the app means by its order,
 *  and without it the unused ones arrive oldest-first (the server pages them by
 *  creation id) while the table right above shows the newest. A user with one
 *  bound template and a hundred unused ones sees the same first row either way
 *  only if the split is invisible — with it, the first row is the one just
 *  imported, which is the one they came for.
 */
export function orderTemplatesForNewTask<T extends { id: number; updated_at: number }>(
  templates: T[],
  usedIds: Iterable<number>
): T[] {
  return unboundTemplatesFirst(newestTemplatesFirst(templates), usedIds);
}

// ---------- QD 模板 → HAR 文档 ----------

/** 编辑器在所有输入之前的起点：一个空的 HAR 文档。 */
export function emptyHarDoc(): object {
  return { log: { version: "1.2", creator: { name: "qdrust", version: "1" }, entries: [] as unknown[] } };
}

/**
 * QD 旧版模板数组（`[{comment, request:{method,url,headers,cookies,data,mimeType}, rule:{…}}]`）
 * 转 QD HAR 文档，与 QD 前端 `utils.tpl2har` 行为一致：
 * `request.data` → `postData.text`、`mimeType` → `postData.mimeType`、`rule.*` 平铺到条目上，
 * headers/cookies/条目一律 `checked: true`。
 */
export function qdTplToHar(tpl: unknown[]): object {
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

/**
 * 把拿到的模板数据归一化成编辑器能读的 HAR 文档。
 *
 * 两种形状都要认：标准 HAR（`{log:{entries}}`）和 QD 导出的请求数组
 * （见 `qdTplToHar`）。订阅源（qd-today/templates 及其兼容库）发布的正是后者，
 * 而 `GET …/library/preview` 按契约返回**上游原文**、`Template.qd_har` 也是库里
 * 存什么回什么，所以转换在客户端做——本地文件导入、模板库预览、打开已有模板
 * 三条路因此共用同一条转换。
 *
 * 两条路都漏掉这个转换时，编辑器只会拿到一个数组：它认不出 `log`，退回空文档，
 * 于是"点导入 → 编辑器全空 → 保存被后端拒绝"。
 *
 * 两种形状都不像时返回 null，交给调用方报错。
 */
export function harDocumentFrom(parsed: unknown): object | null {
  if (Array.isArray(parsed)) return qdTplToHar(parsed);
  if (parsed && typeof parsed === "object") {
    const log = (parsed as Record<string, unknown>).log;
    if (log && typeof log === "object" && !Array.isArray(log)) {
      // 标准 HAR 文档；后端执行要求 version 1.2，导入时统一归一化
      return { log: { ...(log as Record<string, unknown>), version: "1.2" } };
    }
  }
  return null;
}

// ---------- external IdP login policy (docs/design/EXTERNAL_IDP_PLAN.md Phase 3) ----------

/** Public auth policy as delivered by GET /api/v1/auth/config. */
export interface AuthPolicy {
  auth_mode: "local" | "hybrid" | "oidc";
  local_login_enabled: boolean;
  oidc_enabled: boolean;
  oidc_provider_name: string;
  header_auth_enabled: boolean;
  oidc_logout_url: string;
  oidc_post_logout_redirect_uri: string;
}

/** True when the deployment offers SSO through OIDC (an SSO button should be
 *  shown on the sign-in page). */
export function ssoAvailable(policy: AuthPolicy | null): boolean {
  return !!policy?.oidc_enabled;
}

/** The IdP end-session URL to bounce the browser to after local logout, or ""
 *  when the deployment does not configure OIDC single logout. Appends the
 *  optional `post_logout_redirect_uri` so the user returns to this app. */
export function oidcLogoutUrl(policy: AuthPolicy | null): string {
  const base = policy?.oidc_logout_url?.trim();
  if (!base) return "";
  const post = policy?.oidc_post_logout_redirect_uri?.trim();
  if (!post) return base;
  const sep = base.includes("?") ? "&" : "?";
  return `${base}${sep}post_logout_redirect_uri=${encodeURIComponent(post)}`;
}

/** The subset of `Storage` the logout-return marker needs; narrowed so tests can
 *  pass a plain in-memory stub instead of a real `Storage`. */
export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** sessionStorage key for the "we just left for the IdP end-session endpoint"
 *  marker. */
export const OIDC_LOGOUT_RETURN_KEY = "qdrust-oidc-logout-return";

/** How long a return to this app still counts as coming back from an IdP logout.
 *  Without a bound, an abandoned logout (the user hits Back at the IdP, or the
 *  redirect never happens) would leave the marker set forever and a much later
 *  visit would be hijacked into an unexpected SSO redirect. */
export const OIDC_LOGOUT_RETURN_TTL_MS = 300_000;

/** Record that we are leaving for the IdP end-session endpoint and expect to
 *  come back logged out. Stored as a timestamp rather than a flag so
 *  `consumeLogoutReturn` can age it out. */
export function markLogoutReturn(storage: StorageLike, now: number = Date.now()): void {
  storage.setItem(OIDC_LOGOUT_RETURN_KEY, String(now));
}

/** Read-and-clear the logout-return marker. Returns true only when it was set
 *  recently (within `OIDC_LOGOUT_RETURN_TTL_MS`). The marker is consumed either
 *  way, so one logout can trigger at most one automatic SSO start. */
export function consumeLogoutReturn(storage: StorageLike, now: number = Date.now()): boolean {
  const raw = storage.getItem(OIDC_LOGOUT_RETURN_KEY);
  if (raw == null) return false;
  storage.removeItem(OIDC_LOGOUT_RETURN_KEY);
  const stamp = Number(raw);
  if (!Number.isFinite(stamp)) return false;
  const age = now - stamp;
  return age >= 0 && age < OIDC_LOGOUT_RETURN_TTL_MS;
}

/** True when the username/password entry points are enabled on the server and
 *  the user is not forced onto a credential-less path. When false the local
 *  form must be hidden (the server also returns 403 `local_login_disabled`). */
export function localLoginAvailable(policy: AuthPolicy | null): boolean {
  if (!policy) return true; // policy unknown (pre-render) -> show local form by default
  return policy.local_login_enabled;
}

/** In pure-OIDC deployments (`auth_mode === "oidc"`) the local sign-in form is
 *  not shown at all; the page should default to the SSO affordance. */
export function ssoOnly(policy: AuthPolicy | null): boolean {
  return policy?.auth_mode === "oidc";
}
