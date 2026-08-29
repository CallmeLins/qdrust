<script setup lang="ts">
/**
 * HarEditor.vue — 可视化 HAR 编辑器（QD 模板格式）。
 *
 * 输入一个完整 HAR 文档 { log: { version, creator, entries } }（props.modelValue，
 * null 表示尚无文档），内部维护深拷贝编辑状态；保存时 emit('save', 规范化后的文档)，
 * 取消时 emit('cancel')。组件不修改 props 传入的对象。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ChevronDown, ChevronUp, Copy, FileJson2, Trash2 } from "@lucide/vue";
import { t } from "./i18n";

// ---------------- 类型（QD HAR 数据形状） ----------------

type UnknownRecord = Record<string, unknown>;

interface HarHeader {
  name: string;
  value: string;
  checked: boolean;
}
interface HarCookie {
  name: string;
  value: string;
}
interface HarQueryParam {
  name: string;
  value: string;
}
interface HarPostParam {
  name: string;
  value: string;
  checked: boolean;
}
interface HarPostData {
  mimeType?: string;
  /** text 模式：原始请求体（可含 {{模板变量}}） */
  text?: string;
  /** params 模式：application/x-www-form-urlencoded 参数行 */
  params?: HarPostParam[];
}
/** 断言 / 提取变量的数据来源：content | status | header | header-xxx */
interface HarAssert {
  re: string;
  from: string;
}
interface HarExtract {
  name: string;
  re: string;
  from: string;
}
interface HarRequest {
  method: string;
  url: string;
  headers: HarHeader[];
  cookies: HarCookie[];
  /** 可选：原文档没有时编辑状态里也保持 undefined，保存时空/缺失则省略 */
  queryString?: HarQueryParam[];
  /** 可选：缺失时保存不输出 */
  postData?: HarPostData;
}
interface HarEntry {
  checked: boolean;
  request: HarRequest;
  success_asserts: HarAssert[];
  failed_asserts: HarAssert[];
  extract_variables: HarExtract[];
}
interface HarLog {
  version?: string;
  creator?: UnknownRecord;
  entries: HarEntry[];
}
interface HarDocument {
  log: HarLog;
}

const props = defineProps<{ modelValue: object | null }>();
const emit = defineEmits<{ (e: "save", har: object): void; (e: "cancel"): void }>();

// ---------------- 基础工具 ----------------

function asStr(v: unknown): string {
  if (typeof v === "string") return v;
  if (v == null) return "";
  return String(v);
}
function asBool(v: unknown): boolean {
  if (typeof v === "boolean") return v;
  return v === 1 || v === "1" || v === "true";
}
function asRecord(v: unknown): UnknownRecord | null {
  return typeof v === "object" && v !== null && !Array.isArray(v) ? (v as UnknownRecord) : null;
}
function normalizeList<T>(raw: unknown, map: (o: UnknownRecord) => T): T[] {
  return Array.isArray(raw) ? raw.map((item) => map(asRecord(item) ?? {})) : [];
}
function freshDoc(): HarDocument {
  return { log: { version: "1.2", creator: { name: "qdrust", version: "1.0" }, entries: [] } };
}
/** HAR 为纯 JSON 数据，用 JSON 往返深拷贝（structuredClone 对 Vue reactive proxy 会抛 DataCloneError） */
function deepClone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

// ---------------- 加载 / 入站规范化 ----------------

function normalizeEntry(raw: unknown): HarEntry | null {
  const r = asRecord(raw);
  if (!r) return null;
  const req = asRecord(r.request) ?? {};
  const request: HarRequest = {
    method: asStr(req.method) || "GET",
    url: asStr(req.url),
    headers: normalizeList<HarHeader>(req.headers, (o) => ({
      name: asStr(o.name),
      value: asStr(o.value),
      checked: asBool(o.checked ?? true),
    })),
    cookies: normalizeList<HarCookie>(req.cookies, (o) => ({
      name: asStr(o.name),
      value: asStr(o.value),
    })),
  };
  if (Array.isArray(req.queryString)) {
    request.queryString = normalizeList<HarQueryParam>(req.queryString, (o) => ({
      name: asStr(o.name),
      value: asStr(o.value),
    }));
  }
  const pd = asRecord(req.postData);
  if (pd) {
    const post: HarPostData = { mimeType: asStr(pd.mimeType) };
    if (Array.isArray(pd.params)) {
      post.params = normalizeList<HarPostParam>(pd.params, (o) => ({
        name: asStr(o.name),
        value: asStr(o.value),
        checked: asBool(o.checked ?? true),
      }));
    }
    if (typeof pd.text === "string") post.text = pd.text;
    request.postData = post;
  }
  return {
    checked: r.checked === undefined ? true : asBool(r.checked),
    request,
    success_asserts: normalizeList<HarAssert>(r.success_asserts, (o) => ({
      re: asStr(o.re),
      from: asStr(o.from),
    })),
    failed_asserts: normalizeList<HarAssert>(r.failed_asserts, (o) => ({
      re: asStr(o.re),
      from: asStr(o.from),
    })),
    extract_variables: normalizeList<HarExtract>(r.extract_variables, (o) => ({
      name: asStr(o.name),
      re: asStr(o.re),
      from: asStr(o.from),
    })),
  };
}

/** 解析任意输入为内部文档；非 {log:{...}} 形状返回 null。 */
function parseHarDocument(raw: unknown): HarDocument | null {
  const doc = asRecord(raw);
  if (!doc) return null;
  const log = asRecord(doc.log);
  if (!log) return null;
  const out: HarLog = {
    version: typeof log.version === "string" && log.version ? log.version : "1.2",
    entries: (Array.isArray(log.entries) ? log.entries : [])
      .map(normalizeEntry)
      .filter((e): e is HarEntry => e !== null),
  };
  const creator = asRecord(log.creator);
  if (creator) out.creator = creator;
  return { log: out };
}

// ---------------- 保存 / 出站规范化 ----------------

function normalizeAsserts(raw: HarAssert[]): HarAssert[] {
  return (Array.isArray(raw) ? raw : [])
    .map((a) => ({ re: asStr(a.re), from: asStr(a.from) }))
    .filter((a) => a.re !== "" || a.from !== "");
}
function normalizeExtracts(raw: HarExtract[]): HarExtract[] {
  return (Array.isArray(raw) ? raw : [])
    .map((x) => ({ name: asStr(x.name), re: asStr(x.re), from: asStr(x.from) }))
    .filter((x) => x.name !== "" || x.re !== "" || x.from !== "");
}
/** postData：缺失/全空则省略（不输出 null）；params 与 text 互不覆盖。 */
function normalizePostData(pd: HarPostData | undefined): HarPostData | undefined {
  if (!pd) return undefined;
  const out: HarPostData = {};
  const mime = typeof pd.mimeType === "string" ? pd.mimeType.trim() : "";
  if (mime) out.mimeType = mime;
  if (typeof pd.text === "string" && pd.text !== "") out.text = pd.text;
  const params = (Array.isArray(pd.params) ? pd.params : [])
    .map((p) => ({ name: asStr(p.name), value: asStr(p.value), checked: asBool(p.checked ?? true) }))
    .filter((p) => p.name !== "" || p.value !== "");
  if (params.length) out.params = params;
  return Object.keys(out).length > 0 ? out : undefined;
}
/** 保存前对单条 entry 规范化：checked 为 boolean、method/url 为字符串、
 * 各数组均为数组（缺失补 []）、全空行剔除、postData 缺失则不输出。 */
function normalizeForSave(e: HarEntry): HarEntry {
  const request: HarRequest = {
    method: asStr(e.request.method).trim() || "GET",
    url: asStr(e.request.url),
    headers: e.request.headers
      .map((h) => ({ name: asStr(h.name), value: asStr(h.value), checked: asBool(h.checked ?? true) }))
      .filter((h) => h.name !== "" || h.value !== ""),
    cookies: e.request.cookies
      .map((c) => ({ name: asStr(c.name), value: asStr(c.value) }))
      .filter((c) => c.name !== "" || c.value !== ""),
  };
  const queryString = (e.request.queryString ?? [])
    .map((q) => ({ name: asStr(q.name), value: asStr(q.value) }))
    .filter((q) => q.name !== "" || q.value !== "");
  if (queryString.length) request.queryString = queryString;
  const postData = normalizePostData(e.request.postData);
  if (postData) request.postData = postData;
  return {
    checked: asBool(e.checked),
    request,
    success_asserts: normalizeAsserts(e.success_asserts),
    failed_asserts: normalizeAsserts(e.failed_asserts),
    extract_variables: normalizeExtracts(e.extract_variables),
  };
}
function buildDocument(): HarDocument {
  const log = doc.value.log;
  const out: HarLog = {
    version: typeof log.version === "string" && log.version ? log.version : "1.2",
    entries: entries.value.map(normalizeForSave),
  };
  if (log.creator) out.creator = log.creator;
  return { log: out };
}

// ---------------- 组件状态 ----------------

const doc = ref<HarDocument>(freshDoc());
const entries = computed(() => doc.value.log.entries);
const selectedIndex = ref(-1);
const selectedEntry = computed<HarEntry | null>(() =>
  selectedIndex.value >= 0 ? (entries.value[selectedIndex.value] ?? null) : null
);
/** props 为 null 且尚无任何条目时显示整页空状态 */
const docEmpty = computed(() => props.modelValue == null && entries.value.length === 0);

const jsonMode = ref(false);
const jsonText = ref("");

const collapsedSections = ref<Set<string>>(new Set());

const ctrlOpen = ref(false);
const ctrlCustom = ref("");
const ctrlWrapEl = ref<HTMLElement | null>(null);
const CTRL_PRESETS = [
  "{% if  %}",
  "{% else %}",
  "{% endif %}",
  "{% for x in  %}",
  "{% endfor %}",
  "{% while  %}",
  "{% endwhile %}",
];
const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const FROM_SUGGESTIONS = ["content", "status", "header", "header-location"];

function makeEntry(): HarEntry {
  return {
    checked: true,
    request: { method: "GET", url: "", headers: [], cookies: [], queryString: [] },
    success_asserts: [],
    failed_asserts: [],
    extract_variables: [],
  };
}
function cur(): HarEntry | null {
  return selectedEntry.value;
}

function loadFromProp(): void {
  jsonMode.value = false;
  if (props.modelValue == null) {
    doc.value = freshDoc();
    selectedIndex.value = -1;
    return;
  }
  const parsed = parseHarDocument(deepClone(props.modelValue));
  doc.value = parsed ?? freshDoc();
  selectedIndex.value = doc.value.log.entries.length > 0 ? 0 : -1;
}
watch(
  () => props.modelValue,
  () => loadFromProp(),
  { deep: true }
);
loadFromProp();

// ---------------- 条目列表操作 ----------------

function addRequest(): void {
  entries.value.push(makeEntry());
  selectedIndex.value = entries.value.length - 1;
}
function addCtrlStatement(text: string): void {
  const stmt = text.trim();
  if (!stmt) return;
  const entry = makeEntry();
  entry.request.url = stmt;
  const at = selectedIndex.value >= 0 ? selectedIndex.value + 1 : entries.value.length;
  entries.value.splice(at, 0, entry);
  selectedIndex.value = at;
  ctrlOpen.value = false;
  ctrlCustom.value = "";
}
function moveEntry(idx: number, dir: -1 | 1): void {
  const list = entries.value;
  const to = idx + dir;
  if (to < 0 || to >= list.length) return;
  const tmp = list[idx];
  list[idx] = list[to];
  list[to] = tmp;
  if (selectedIndex.value === idx) selectedIndex.value = to;
  else if (selectedIndex.value === to) selectedIndex.value = idx;
}
function duplicateEntry(idx: number): void {
  const e = entries.value[idx];
  if (!e) return;
  entries.value.splice(idx + 1, 0, deepClone(e));
  selectedIndex.value = idx + 1;
}
function removeEntry(idx: number): void {
  entries.value.splice(idx, 1);
  if (selectedIndex.value === idx) {
    selectedIndex.value = idx < entries.value.length ? idx : entries.value.length - 1;
  } else if (selectedIndex.value > idx) {
    selectedIndex.value -= 1;
  }
}

/** 控制语句：qd 特色，URL 直接写 {% ... %} 文本，method 无意义 */
function isCtrlStatement(e: HarEntry): boolean {
  return e.request.url.trimStart().startsWith("{%");
}
function entryBadge(e: HarEntry): string {
  return isCtrlStatement(e) ? "CTRL" : e.request.method.trim().toUpperCase() || "GET";
}
function methodTone(method: string): string {
  const m = method.trim().toUpperCase();
  if (m === "GET") return "get";
  if (m === "POST") return "post";
  if (m === "PUT" || m === "PATCH") return "put";
  if (m === "DELETE") return "del";
  return "other";
}

// ---------------- 详细编辑面板：字段增删 ----------------

function addHeader(): void {
  const e = cur();
  if (e) e.request.headers.push({ name: "", value: "", checked: true });
}
function removeHeader(i: number): void {
  const e = cur();
  if (e) e.request.headers.splice(i, 1);
}
function addCookie(): void {
  const e = cur();
  if (e) e.request.cookies.push({ name: "", value: "" });
}
function removeCookie(i: number): void {
  const e = cur();
  if (e) e.request.cookies.splice(i, 1);
}
function addQueryParam(): void {
  const e = cur();
  if (!e) return;
  if (!e.request.queryString) e.request.queryString = [];
  e.request.queryString.push({ name: "", value: "" });
}
function removeQueryParam(i: number): void {
  const e = cur();
  if (e?.request.queryString) e.request.queryString.splice(i, 1);
}
function addSuccessAssert(): void {
  const e = cur();
  if (e) e.success_asserts.push({ re: "", from: "content" });
}
function removeSuccessAssert(i: number): void {
  const e = cur();
  if (e) e.success_asserts.splice(i, 1);
}
function addFailedAssert(): void {
  const e = cur();
  if (e) e.failed_asserts.push({ re: "", from: "content" });
}
function removeFailedAssert(i: number): void {
  const e = cur();
  if (e) e.failed_asserts.splice(i, 1);
}
function addExtractVar(): void {
  const e = cur();
  if (e) e.extract_variables.push({ name: "", re: "", from: "content" });
}
function removeExtractVar(i: number): void {
  const e = cur();
  if (e) e.extract_variables.splice(i, 1);
}

// ---------------- postData（无 / text / params 三模式） ----------------

/** 保证 postData 存在并按模式初始化，同时保留另一形态的数据便于来回切换 */
function ensurePostData(e: HarEntry, mode: "text" | "params"): HarPostData {
  const pd = e.request.postData;
  const next: HarPostData = { mimeType: typeof pd?.mimeType === "string" ? pd.mimeType : "" };
  if (pd?.text !== undefined) next.text = pd.text;
  if (pd?.params) next.params = pd.params;
  if (mode === "text" && next.text === undefined) next.text = "";
  if (mode === "params" && next.params === undefined) next.params = [];
  e.request.postData = next;
  return next;
}
function addPostParam(): void {
  const e = cur();
  if (!e) return;
  const pd = ensurePostData(e, "params");
  pd.params!.push({ name: "", value: "", checked: true });
}
function removePostParam(i: number): void {
  const pd = cur()?.request.postData;
  if (pd?.params) pd.params.splice(i, 1);
}

const postMode = computed<string>({
  get() {
    const pd = cur()?.request.postData;
    if (!pd) return "none";
    if (pd.params) return "params";
    if (pd.text !== undefined) return "text";
    return "none";
  },
  set(mode: string) {
    const e = cur();
    if (!e) return;
    if (mode === "none") {
      delete e.request.postData;
      return;
    }
    ensurePostData(e, mode === "params" ? "params" : "text");
  },
});
const postMime = computed<string>({
  get() {
    return cur()?.request.postData?.mimeType ?? "";
  },
  set(v: string) {
    const e = cur();
    if (!e) return;
    if (e.request.postData) e.request.postData.mimeType = v;
    else if (v !== "") e.request.postData = { mimeType: v };
  },
});
const postText = computed<string>({
  get() {
    return cur()?.request.postData?.text ?? "";
  },
  set(v: string) {
    const e = cur();
    if (!e) return;
    ensurePostData(e, "text").text = v;
  },
});
const postParams = computed<HarPostParam[]>(() => cur()?.request.postData?.params ?? []);

// ---------------- 折叠区 ----------------

function toggleSection(key: string): void {
  const s = collapsedSections.value;
  if (s.has(key)) s.delete(key);
  else s.add(key);
}
function isCollapsed(key: string): boolean {
  return collapsedSections.value.has(key);
}

// ---------------- JSON 模式 ----------------

function tryParseJson(): HarDocument | null {
  try {
    return parseHarDocument(JSON.parse(jsonText.value));
  } catch {
    return null;
  }
}
const jsonOk = computed(() => jsonMode.value && tryParseJson() !== null);
const jsonErrMsg = computed(() =>
  jsonMode.value && !jsonOk.value ? t("harJsonError") : ""
);
function applyParsed(parsed: HarDocument): void {
  doc.value = parsed;
  selectedIndex.value = parsed.log.entries.length > 0 ? 0 : -1;
}
function toggleJsonMode(): void {
  if (jsonMode.value) {
    const parsed = tryParseJson();
    if (!parsed) return; // 留在 JSON 模式并显示错误提示
    applyParsed(parsed);
    jsonMode.value = false;
  } else {
    jsonText.value = JSON.stringify(doc.value, null, 2);
    jsonMode.value = true;
  }
}
function save(): void {
  if (jsonMode.value) {
    const parsed = tryParseJson();
    if (!parsed) return;
    applyParsed(parsed);
    jsonMode.value = false;
  }
  emit("save", buildDocument());
}
function cancel(): void {
  emit("cancel");
}

// ---------------- 控制语句下拉：点击外部关闭 ----------------

function onDocPointerDown(e: MouseEvent): void {
  if (ctrlOpen.value && ctrlWrapEl.value && !ctrlWrapEl.value.contains(e.target as Node)) {
    ctrlOpen.value = false;
  }
}
onMounted(() => document.addEventListener("mousedown", onDocPointerDown));
onBeforeUnmount(() => document.removeEventListener("mousedown", onDocPointerDown));
</script>

<template>
  <div class="har-editor">
    <datalist id="har-methods">
      <option v-for="m in METHODS" :key="m" :value="m" />
    </datalist>
    <datalist id="har-from">
      <option v-for="f in FROM_SUGGESTIONS" :key="f" :value="f" />
    </datalist>

    <div class="har-toolbar">
      <h2>{{ t('harTitle') }}</h2>
      <div class="har-toolbar-actions">
        <button type="button" class="secondary-button" @click="addRequest">{{ t('harAddRequest') }}</button>

        <div ref="ctrlWrapEl" class="ctrl-wrap">
          <button type="button" class="secondary-button" @click.stop="ctrlOpen = !ctrlOpen">
            {{ t('harControl') }}
            <ChevronDown :size="14" class="ctrl-caret" :class="{ open: ctrlOpen }" />
          </button>
          <div v-if="ctrlOpen" class="ctrl-menu" @click.stop>
            <button
              type="button"
              v-for="stmt in CTRL_PRESETS"
              :key="stmt"
              class="ctrl-item"
              @click="addCtrlStatement(stmt)"
            >
              {{ stmt }}
            </button>
            <div class="ctrl-custom">
              <input
                v-model="ctrlCustom"
                :placeholder="t('harCtrlCustomPlaceholder')"
                @keydown.enter="addCtrlStatement(ctrlCustom)"
              />
              <button type="button" class="add-mini" @click="addCtrlStatement(ctrlCustom)">{{ t('harCtrlInsert') }}</button>
            </div>
          </div>
        </div>

        <button type="button" class="secondary-button" @click="toggleJsonMode">
          {{ jsonMode ? t('harBackVisual') : t('harJsonMode') }}
        </button>
        <button type="button" class="secondary-button" @click="cancel">{{ t('harCancel') }}</button>
        <button type="button" class="primary-button" :disabled="jsonMode && !jsonOk" @click="save">{{ t('harSave') }}</button>
      </div>
    </div>

    <div class="har-body">
      <!-- JSON 模式：整个文档的原始 textarea -->
      <div v-if="jsonMode" class="json-wrap">
        <textarea
          v-model="jsonText"
          class="json-area"
          spellcheck="false"
          :placeholder="t('harJsonPlaceholder')"
        ></textarea>
        <div v-if="jsonErrMsg" class="json-error">{{ jsonErrMsg }}</div>
      </div>

      <template v-else>
        <!-- 空状态：modelValue 为 null 且尚无条目 -->
        <div v-if="docEmpty" class="har-empty-full">
          <FileJson2 :size="44" />
          <p>{{ t('harEmptyDoc') }}</p>
          <span class="hint">{{ t('harEmptyHint') }}</span>
        </div>

        <template v-else>
          <!-- 左侧条目列表（约 40%） -->
          <div class="har-list">
            <div v-if="entries.length === 0" class="har-empty-list">{{ t('harEmptyList') }}</div>
            <div
              v-for="(entry, idx) in entries"
              :key="idx"
              class="har-item"
              :class="{ selected: idx === selectedIndex }"
              @click="selectedIndex = idx"
            >
              <input type="checkbox" v-model="entry.checked" />
              <span
                class="method-badge"
                :class="isCtrlStatement(entry) ? 'ctrl' : methodTone(entry.request.method)"
              >
                {{ entryBadge(entry) }}
              </span>
              <span
                class="har-url"
                :class="{ ctrl: isCtrlStatement(entry), empty: !entry.request.url }"
                :title="entry.request.url"
              >
                {{ entry.request.url || t('harEmptyUrl') }}
              </span>
              <div class="har-item-actions" @click.stop>
                <button
                  type="button"
                  class="icon-button"
                  :title="t('harMoveUp')"
                  :disabled="idx === 0"
                  @click="moveEntry(idx, -1)"
                >
                  <ChevronUp :size="13" />
                </button>
                <button
                  type="button"
                  class="icon-button"
                  :title="t('harMoveDown')"
                  :disabled="idx === entries.length - 1"
                  @click="moveEntry(idx, 1)"
                >
                  <ChevronDown :size="13" />
                </button>
                <button type="button" class="icon-button" :title="t('harDuplicate')" @click="duplicateEntry(idx)">
                  <Copy :size="13" />
                </button>
                <button type="button" class="icon-button danger" :title="t('harDelete')" @click="removeEntry(idx)">
                  <Trash2 :size="13" />
                </button>
              </div>
            </div>
          </div>

          <!-- 右侧详细编辑面板 -->
          <div v-if="selectedEntry" class="har-detail">
            <div class="detail-head">
              <input
                list="har-methods"
                v-model="selectedEntry.request.method"
                class="method-input"
                placeholder="GET"
              />
              <label class="chk">
                <input type="checkbox" v-model="selectedEntry.checked" />
                {{ t('harEnabled') }}
              </label>
              <span class="detail-spacer"></span>
              <button
                type="button"
                class="icon-button"
                :title="t('harDuplicateEntry')"
                @click="duplicateEntry(selectedIndex)"
              >
                <Copy :size="15" />
              </button>
              <button
                type="button"
                class="icon-button danger"
                :title="t('harDeleteEntry')"
                @click="removeEntry(selectedIndex)"
              >
                <Trash2 :size="15" />
              </button>
            </div>

            <label class="field">
              {{ t('harUrl') }}
              <textarea
                v-model="selectedEntry.request.url"
                rows="2"
                :placeholder="t('harUrlPlaceholder')"
              ></textarea>
            </label>

            <!-- Headers -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('headers')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('headers') }" />
                <h4>Headers</h4>
                <button type="button" class="add-mini" @click.stop="addHeader">{{ t('harAddHeader') }}</button>
              </div>
              <div v-show="!isCollapsed('headers')" class="sec-body">
                <div class="rows">
                  <div v-for="(h, i) in selectedEntry.request.headers" :key="i" class="kv-row">
                    <input v-model="h.name" :placeholder="t('harName')" />
                    <input v-model="h.value" :placeholder="t('harValue')" />
                    <input type="checkbox" v-model="h.checked" class="chk-box" :title="t('harEnableHeader')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeHeader(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- Cookies -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('cookies')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('cookies') }" />
                <h4>Cookies</h4>
                <button type="button" class="add-mini" @click.stop="addCookie">{{ t('harAddCookie') }}</button>
              </div>
              <div v-show="!isCollapsed('cookies')" class="sec-body">
                <div class="rows">
                  <div v-for="(c, i) in selectedEntry.request.cookies" :key="i" class="kv-row no-chk">
                    <input v-model="c.name" :placeholder="t('harName')" />
                    <input v-model="c.value" :placeholder="t('harValue')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeCookie(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- Query String（可选） -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('query')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('query') }" />
                <h4>Query String</h4>
                <button type="button" class="add-mini" @click.stop="addQueryParam">{{ t('harAddQuery') }}</button>
              </div>
              <div v-show="!isCollapsed('query')" class="sec-body">
                <div v-if="selectedEntry.request.queryString && selectedEntry.request.queryString.length" class="rows">
                  <div v-for="(q, i) in selectedEntry.request.queryString" :key="i" class="kv-row no-chk">
                    <input v-model="q.name" :placeholder="t('harName')" />
                    <input v-model="q.value" :placeholder="t('harValue')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeQueryParam(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
                <span v-else class="sec-hint">{{ t('harNoQuery') }}</span>
              </div>
            </section>

            <!-- postData -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('post')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('post') }" />
                <h4>{{ t('harPostData') }}</h4>
                <span class="sec-note">{{ postMode === "none" ? t('harNotSet') : postMode }}</span>
              </div>
              <div v-show="!isCollapsed('post')" class="sec-body">
                <div class="post-meta">
                  <label class="field">
                    mimeType
                    <input v-model="postMime" :placeholder="t('harMimePlaceholder')" />
                  </label>
                  <label class="field">
                    {{ t('harMode') }}
                    <select v-model="postMode">
                      <option value="none">{{ t('harNone') }}</option>
                      <option value="text">text</option>
                      <option value="params">params</option>
                    </select>
                  </label>
                </div>
                <div v-if="postMode === 'text'">
                  <label class="field">
                    {{ t('harBodyText') }}
                    <textarea
                      v-model="postText"
                      rows="4"
                      :placeholder="t('harBodyPlaceholder')"
                    ></textarea>
                  </label>
                </div>
                <div v-else-if="postMode === 'params'">
                  <div class="rows">
                    <div v-for="(p, i) in postParams" :key="i" class="kv-row">
                      <input v-model="p.name" :placeholder="t('harName')" />
                      <input v-model="p.value" :placeholder="t('harValue')" />
                      <input type="checkbox" v-model="p.checked" class="chk-box" :title="t('harEnableParam')" />
                      <button type="button" class="row-del" :title="t('harDelete')" @click="removePostParam(i)">
                        <Trash2 :size="13" />
                      </button>
                    </div>
                    <button type="button" class="add-mini" @click="addPostParam">{{ t('harAddQuery') }}</button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 成功断言 -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('success')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('success') }" />
                <h4>{{ t('harSuccessAsserts') }}</h4>
                <button type="button" class="add-mini" @click.stop="addSuccessAssert">{{ t('harAddSuccess') }}</button>
              </div>
              <div v-show="!isCollapsed('success')" class="sec-body">
                <div class="rows">
                  <div v-for="(a, i) in selectedEntry.success_asserts" :key="i" class="assert-row">
                    <input v-model="a.re" :placeholder="t('harRegexSuccessPlaceholder')" />
                    <input list="har-from" v-model="a.from" :placeholder="t('harFrom')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeSuccessAssert(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 失败断言 -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('failed')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('failed') }" />
                <h4>{{ t('harFailedAsserts') }}</h4>
                <button type="button" class="add-mini" @click.stop="addFailedAssert">{{ t('harAddFailed') }}</button>
              </div>
              <div v-show="!isCollapsed('failed')" class="sec-body">
                <div class="rows">
                  <div v-for="(a, i) in selectedEntry.failed_asserts" :key="i" class="assert-row">
                    <input v-model="a.re" :placeholder="t('harRegexFailedPlaceholder')" />
                    <input list="har-from" v-model="a.from" :placeholder="t('harFrom')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeFailedAssert(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 提取变量 -->
            <section class="sec">
              <div class="sec-head" @click="toggleSection('extract')">
                <ChevronDown :size="14" class="chev" :class="{ closed: isCollapsed('extract') }" />
                <h4>{{ t('harExtractVars') }}</h4>
                <button type="button" class="add-mini" @click.stop="addExtractVar">{{ t('harAddExtract') }}</button>
              </div>
              <div v-show="!isCollapsed('extract')" class="sec-body">
                <div class="rows">
                  <div v-for="(x, i) in selectedEntry.extract_variables" :key="i" class="extract-row">
                    <input v-model="x.name" :placeholder="t('harVarName')" />
                    <input v-model="x.re" :placeholder="t('harRegexExtractPlaceholder')" />
                    <input list="har-from" v-model="x.from" :placeholder="t('harFrom')" />
                    <button type="button" class="row-del" :title="t('harDelete')" @click="removeExtractVar(i)">
                      <Trash2 :size="13" />
                    </button>
                  </div>
                </div>
              </div>
            </section>
          </div>

          <!-- 有列表但未选中条目 -->
          <div v-else class="har-detail har-empty-detail">{{ t('harSelectHint') }}</div>
        </template>
      </template>
    </div>
  </div>
</template>

<style scoped>
.har-editor {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: 100%;
  min-height: 400px;
  max-height: 78vh;
  color: #252a26;
  font-size: 13px;
}

/* ---------- 顶部工具条 ---------- */
.har-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.har-toolbar h2 {
  margin: 0;
  font-size: 16px;
  font-weight: 700;
  color: #242824;
}
.har-toolbar-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

/* ---------- 控制语句下拉 ---------- */
.ctrl-wrap {
  position: relative;
}
.ctrl-caret {
  transition: transform 0.15s ease;
}
.ctrl-caret.open {
  transform: rotate(180deg);
}
.ctrl-menu {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 40;
  min-width: 215px;
  padding: 6px;
  display: grid;
  gap: 2px;
  background: #fff;
  border: 1px solid #d9dcd8;
  border-radius: 8px;
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18);
}
.ctrl-item {
  border: 0;
  background: transparent;
  text-align: left;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  font-size: 12px;
  color: #3a403b;
  padding: 6px 8px;
  border-radius: 5px;
  cursor: pointer;
}
.ctrl-item:hover {
  background: #eef2ea;
  color: #4f6f44;
}
.ctrl-custom {
  display: flex;
  gap: 6px;
  margin-top: 4px;
  padding-top: 6px;
  border-top: 1px solid #eceeea;
}
.ctrl-custom input {
  flex: 1;
  min-width: 0;
  height: 30px;
  padding: 0 8px;
  border: 1px solid #d9dcd8;
  border-radius: 5px;
  font-size: 12px;
  outline: 0;
}
.ctrl-custom input:focus {
  border-color: #718169;
}

/* ---------- 主体布局 ---------- */
.har-body {
  flex: 1;
  min-height: 0;
  display: flex;
  gap: 12px;
  border-top: 1px solid #e4e6e3;
  padding-top: 12px;
}

/* ---------- 左侧条目列表 ---------- */
.har-list {
  width: 40%;
  min-width: 260px;
  overflow-y: auto;
  border: 1px solid #e2e5e0;
  border-radius: 8px;
  background: #fafbf9;
}
.har-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  border-bottom: 1px solid #eceeea;
  cursor: pointer;
}
.har-item:last-child {
  border-bottom: 0;
}
.har-item:hover {
  background: #f1f3ef;
}
.har-item.selected {
  background: #e9efe4;
  box-shadow: inset 3px 0 0 #5a7a4f;
}
.har-item input[type="checkbox"] {
  width: 15px;
  height: 15px;
  flex: none;
  accent-color: #5a7a4f;
}
.method-badge {
  flex: none;
  min-width: 44px;
  text-align: center;
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.4px;
  border-radius: 4px;
  padding: 2px 5px;
}
.method-badge.get {
  color: #1d7a3e;
  background: #e3f3e8;
}
.method-badge.post {
  color: #1c5aa8;
  background: #e4eefb;
}
.method-badge.put {
  color: #96610e;
  background: #faf0d7;
}
.method-badge.del {
  color: #b03a2e;
  background: #fbe7e4;
}
.method-badge.other {
  color: #5b615c;
  background: #eceeec;
}
.method-badge.ctrl {
  color: #6d3fb5;
  background: #efe8fb;
}
.har-url {
  flex: 1;
  min-width: 0;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  font-size: 12px;
  color: #3a403b;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.har-url.ctrl {
  color: #7c4fd0;
  font-weight: 600;
}
.har-url.empty {
  color: #9aa09b;
  font-style: italic;
}
.har-item-actions {
  display: flex;
  gap: 2px;
  flex: none;
}
.har-item-actions .icon-button {
  width: 26px;
  height: 26px;
  border-radius: 5px;
}
.icon-button:disabled {
  opacity: 0.35;
  cursor: not-allowed;
}
.icon-button.danger {
  color: #a0564f;
}
.icon-button.danger:hover {
  border-color: #e5c4c1;
  background: #fdf3f1;
  color: #a33a33;
}
.har-empty-list {
  padding: 18px;
  text-align: center;
  color: #9aa09b;
  font-size: 12px;
}

/* ---------- 右侧详细编辑面板 ---------- */
.har-detail {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  border: 1px solid #e2e5e0;
  border-radius: 8px;
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  background: #fff;
}
.har-empty-detail {
  display: grid;
  place-content: center;
  color: #9aa09b;
  font-size: 12px;
  text-align: center;
}
.detail-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
.method-input {
  width: 130px;
  height: 34px;
  padding: 0 10px;
  border: 1px solid #d2d6d1;
  border-radius: 6px;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  font-size: 13px;
  font-weight: 700;
  outline: 0;
  background: #fff;
  color: #252a26;
}
.method-input:focus {
  border-color: #718169;
  box-shadow: 0 0 0 3px #7181691a;
}
.chk {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: #3a403b;
  cursor: pointer;
}
.chk input {
  width: 15px;
  height: 15px;
  accent-color: #5a7a4f;
}
.detail-spacer {
  flex: 1;
}

/* ---------- 表单字段 ---------- */
.field {
  display: grid;
  gap: 6px;
  font-size: 12px;
  font-weight: 650;
  color: #4d534e;
}
.field textarea,
.field input,
.field select {
  width: 100%;
  border: 1px solid #d2d6d1;
  border-radius: 6px;
  outline: 0;
  background: #fff;
  color: #252a26;
  font-size: 13px;
}
.field textarea {
  padding: 8px 10px;
  resize: vertical;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  line-height: 1.45;
}
.field input,
.field select {
  height: 34px;
  padding: 0 10px;
}
.field input:focus,
.field textarea:focus,
.field select:focus {
  border-color: #718169;
  box-shadow: 0 0 0 3px #7181691a;
}
.field textarea::placeholder {
  color: #a8ada7;
}

/* ---------- 可折叠分区 ---------- */
.sec {
  border: 1px solid #e6e8e4;
  border-radius: 8px;
  overflow: hidden;
}
.sec-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 12px;
  background: #f7f8f6;
  cursor: pointer;
  user-select: none;
}
.sec-head h4 {
  margin: 0;
  flex: 1;
  font-size: 13px;
  font-weight: 700;
  color: #3a403b;
}
.sec-head .chev {
  color: #7c837d;
  transition: transform 0.15s ease;
}
.sec-head .chev.closed {
  transform: rotate(-90deg);
}
.sec-note {
  font-size: 11px;
  color: #9aa09b;
  font-weight: 600;
}
.add-mini {
  border: 0;
  background: transparent;
  color: #4f6f44;
  font-size: 12px;
  font-weight: 650;
  cursor: pointer;
  padding: 4px 7px;
  border-radius: 5px;
  white-space: nowrap;
}
.add-mini:hover {
  background: #e6efe1;
}
.sec-body {
  padding: 10px 12px;
  display: grid;
  gap: 8px;
  border-top: 1px solid #eceeea;
}
.sec-hint {
  font-size: 12px;
  color: #9aa09b;
}
.rows {
  display: grid;
  gap: 6px;
}

/* ---------- name/value 行 ---------- */
.kv-row {
  display: grid;
  grid-template-columns: minmax(90px, 1fr) minmax(120px, 1.6fr) auto auto;
  gap: 6px;
  align-items: center;
}
.kv-row.no-chk {
  grid-template-columns: minmax(90px, 1fr) minmax(120px, 1.6fr) auto;
}
.kv-row input {
  height: 30px;
  padding: 0 8px;
  border: 1px solid #d9dcd8;
  border-radius: 5px;
  font-size: 12px;
  outline: 0;
  background: #fff;
  color: #252a26;
}
.kv-row input:focus {
  border-color: #718169;
}
.kv-row .chk-box {
  width: 16px;
  height: 16px;
  margin: 0 auto;
  accent-color: #5a7a4f;
}
.row-del {
  border: 0;
  background: transparent;
  color: #a0564f;
  cursor: pointer;
  padding: 4px;
  border-radius: 5px;
  display: inline-grid;
  place-items: center;
}
.row-del:hover {
  background: #fbeae7;
}

/* ---------- 断言 / 提取行 ---------- */
.assert-row {
  display: grid;
  grid-template-columns: 1fr 170px auto;
  gap: 6px;
  align-items: center;
}
.extract-row {
  display: grid;
  grid-template-columns: 130px 1fr 170px auto;
  gap: 6px;
  align-items: center;
}
.assert-row input,
.extract-row input {
  height: 30px;
  padding: 0 8px;
  border: 1px solid #d9dcd8;
  border-radius: 5px;
  font-size: 12px;
  outline: 0;
  background: #fff;
  color: #252a26;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
}
.assert-row input:focus,
.extract-row input:focus {
  border-color: #718169;
}

/* ---------- postData ---------- */
.post-meta {
  display: grid;
  grid-template-columns: 1fr 140px;
  gap: 10px;
}

/* ---------- JSON 模式 ---------- */
.json-wrap {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.json-area {
  flex: 1;
  min-height: 0;
  width: 100%;
  padding: 12px;
  border: 1px solid #cfd3cd;
  border-radius: 8px;
  outline: 0;
  background: #fbfcfa;
  color: #252a26;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  font-size: 12px;
  line-height: 1.5;
  resize: none;
}
.json-area:focus {
  border-color: #718169;
  box-shadow: 0 0 0 3px #7181691a;
}
.json-error {
  color: #a33a33;
  background: #fdf0ee;
  border: 1px solid #f0cfcb;
  border-radius: 6px;
  padding: 7px 10px;
  font-size: 12px;
}

/* ---------- 空状态 ---------- */
.har-empty-full {
  flex: 1;
  min-height: 0;
  display: grid;
  place-content: center;
  justify-items: center;
  gap: 8px;
  color: #8b928c;
}
.har-empty-full p {
  margin: 0;
  font-size: 14px;
  font-weight: 650;
  color: #5b615c;
}
.har-empty-full .hint {
  font-size: 12px;
  color: #9aa09b;
}

@media (max-width: 640px) {
  .har-body {
    flex-direction: column;
  }
  .har-list {
    width: 100%;
    max-height: 260px;
  }
  .post-meta {
    grid-template-columns: 1fr;
  }
}
</style>
