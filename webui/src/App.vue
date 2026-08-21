<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { Activity, CalendarClock, Check, ChevronDown, CircleHelp, FileJson2, LayoutDashboard, Menu, MoreHorizontal, Plus, RefreshCw, Search, Settings, Trash2, X, Zap } from "@lucide/vue";
import { api, type CreateTask, type Task, type Run, type RunStep, type User, type Template, type Note, type Plugin, type NotificationChannel, type NotificationAction, type TemplateSubscription, type SubscriptionSync, type PushRequest, type SiteSetting } from "./api";
import { formatRunTime } from "./utils";
import { locale, t, toggleLocale } from "./i18n";

const tasks = ref<Task[]>([]);
const loading = ref(true);
const saving = ref(false);
const error = ref("");
const search = ref("");
const showCreate = ref(false);
const menuOpen = ref(false);
const ready = ref(false);
const authenticated = ref(false);
const currentUser = ref<User | null>(null);
const view = ref<"tasks" | "templates" | "notes" | "plugins" | "notifications" | "runs" | "subscriptions" | "push" | "admin" | "settings">("tasks");
const templates = ref<Template[]>([]); const publicTemplates = ref<Template[]>([]);
const showImport = ref(false); const importForm = reactive({name:"",description:"",har:""});
const editingTemplate = ref<number | null>(null);
const notes=ref<Note[]>([]);const noteForm=reactive({id:0,title:"",content:""});
const plugins=ref<Plugin[]>([]);const pluginForm=reactive({name:"",command:""});
const invokeForm=reactive({action:"run",query:"{}"});const pluginResult=ref("");
const channels=ref<NotificationChannel[]>([]);const channelForm=reactive({name:"",url:""});
const actions=ref<NotificationAction[]>([]);const actionForm=reactive({taskId:0,channelId:0,event:"failure"});
const authMode = ref<"login" | "bootstrap" | "register">("login");
const authForm = reactive({ username: "", password: "", email: "" });
const expandedTask = ref<number | null>(null);
const runs = ref<Run[]>([]);
const allRuns = ref<Run[]>([]);
const subscriptions = ref<TemplateSubscription[]>([]);
const subSyncs = ref<SubscriptionSync[]>([]);
const subForm = reactive({ name: "", url: "" });
const pushRequests = ref<PushRequest[]>([]);
const myPushRequests = ref<PushRequest[]>([]);
const pushNote = ref("");
const pushTemplateId = ref(0);
const adminUsers = ref<User[]>([]);
const adminSettings = ref<SiteSetting[]>([]);
const settingsForm = reactive({ requireEmail: false, gaKey: "", retentionDays: 0 });
const steps = ref<RunStep[]>([]);
const form = reactive<CreateTask>({ name: "", cron: "0 * * * * * *", method: "GET", url: "", disabled: false, grp: null });

const filtered = computed(() => {
  const term = search.value.trim().toLowerCase();
  return term ? tasks.value.filter((task) => `${task.name} ${task.url}`.toLowerCase().includes(term)) : tasks.value;
});
const activeCount = computed(() => tasks.value.filter((task) => !task.disabled).length);
const successCount = computed(() => tasks.value.filter((task) => task.last_status && task.last_status < 400).length);
const currentViewName = computed(() => ({ tasks: "任务", templates: "模板", notes: "记事本", plugins: "插件", notifications: "通知", runs: "运行记录", subscriptions: "订阅", push: "发布审批", admin: "管理", settings: "设置" })[view.value]);
const taskName = (taskId: number) => tasks.value.find((task) => task.id === taskId)?.name ?? `任务 #${taskId}`;

async function load() {
  loading.value = true;
  error.value = "";
  try {
    [tasks.value, ready.value] = await Promise.all([api.tasks(), api.ready().then(() => true)]);
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : "加载失败";
  } finally {
    loading.value = false;
  }
}

async function createTask() {
  saving.value = true;
  error.value = "";
  try {
    await api.createTask({ ...form });
    Object.assign(form, { name: "", cron: "0 * * * * * *", method: "GET", url: "", disabled: false });
    showCreate.value = false;
    await load();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : "创建失败";
  } finally {
    saving.value = false;
  }
}

async function toggle(task: Task) {
  await api.updateTask(task.id, { disabled: !task.disabled });
  await load();
}

async function runNow(task: Task) {
  try { await api.runTask(task.id); await load(); } catch (cause) { error.value = cause instanceof Error ? cause.message : "运行失败"; }
}

async function showRuns(task: Task) {
  expandedTask.value = expandedTask.value === task.id ? null : task.id;
  if (expandedTask.value) { runs.value = await api.taskRuns(task.id); steps.value = runs.value.length ? await api.runSteps(runs.value[0].id) : []; }
}

async function cancel(run: Run) {
  await api.cancelRun(run.id);
  if (expandedTask.value) runs.value = await api.taskRuns(expandedTask.value);
}

async function remove(task: Task) {
  if (!window.confirm(`删除任务“${task.name}”？`)) return;
  await api.deleteTask(task.id);
  await load();
}

async function authenticate() {
  saving.value = true; error.value = "";
  try {
    let session;
    if (authMode.value === "login") session = await api.login(authForm.username, authForm.password);
    else if (authMode.value === "bootstrap") session = await api.bootstrap(authForm.username, authForm.password);
    else session = await api.register(authForm.username, authForm.password, authForm.email || undefined);
    currentUser.value = session.user;
    authenticated.value = true; await load();
  } catch (cause) { error.value = cause instanceof Error ? cause.message : "认证失败"; }
  finally { saving.value = false; }
}

async function logout() {
  try { await api.logout(); } finally { authenticated.value = false; currentUser.value = null; tasks.value = []; runs.value = []; }
}
async function openTemplates(){view.value="templates";[templates.value,publicTemplates.value]=await Promise.all([api.templates(),api.publicTemplates()]);}
async function publish(id:number){await api.publishTemplate(id);await openTemplates();}
async function copyTemplate(id:number){await api.copyPublicTemplate(id);await openTemplates();}
async function removeTemplate(id:number){await api.deleteTemplate(id);await openTemplates();}
async function importHar(){saving.value=true;error.value="";try{const har=JSON.parse(importForm.har);if(editingTemplate.value)await api.updateQdHar(editingTemplate.value,importForm.name,importForm.description,har);else await api.importQdHar(importForm.name,importForm.description,har);showImport.value=false;editingTemplate.value=null;Object.assign(importForm,{name:"",description:"",har:""});await openTemplates();}catch(cause){error.value=cause instanceof Error?cause.message:"保存失败";}finally{saving.value=false;}}
function editTemplate(item:Template){if(item.source_format!=="qd_har")return;editingTemplate.value=item.id;Object.assign(importForm,{name:item.name,description:item.description??"",har:JSON.stringify(item.qd_har,null,2)});showImport.value=true;}
async function openNotes(){view.value="notes";notes.value=await api.notes();}
function editNote(note?:Note){Object.assign(noteForm,note?{id:note.id,title:note.title,content:note.content}:{id:0,title:"",content:""});}
async function saveNote(){if(noteForm.id)await api.updateNote(noteForm.id,noteForm.title,noteForm.content);else await api.createNote(noteForm.title,noteForm.content);editNote();await openNotes();}
async function removeNote(id:number){await api.deleteNote(id);await openNotes();}
async function openPlugins(){view.value="plugins";plugins.value=await api.plugins();}
async function savePlugin(){await api.createPlugin(pluginForm.name,pluginForm.command);Object.assign(pluginForm,{name:"",command:""});await openPlugins();}
async function togglePlugin(plugin:Plugin){await api.updatePlugin(plugin.id,!plugin.enabled);await openPlugins();}
async function removePlugin(id:number){await api.deletePlugin(id);await openPlugins();}
async function invokePlugin(plugin:Plugin){error.value="";pluginResult.value="";try{const query=JSON.parse(invokeForm.query) as Record<string,string>;pluginResult.value=JSON.stringify(await api.invokePlugin(plugin.id,invokeForm.action,query),null,2);}catch(cause){error.value=cause instanceof Error?cause.message:"调用失败";}}
async function openNotifications(){view.value="notifications";[channels.value,tasks.value]=await Promise.all([api.notificationChannels(),api.tasks()]);if(actionForm.taskId)actions.value=await api.notificationActions(actionForm.taskId);}
async function saveChannel(){await api.createWebhook(channelForm.name,channelForm.url);Object.assign(channelForm,{name:"",url:""});await openNotifications();}
async function toggleChannel(channel:NotificationChannel){await api.updateNotificationChannel(channel.id,!channel.enabled);await openNotifications();}
async function removeChannel(id:number){await api.deleteNotificationChannel(id);await openNotifications();}
async function loadActions(){actions.value=actionForm.taskId?await api.notificationActions(actionForm.taskId):[];}
async function saveAction(){await api.createNotificationAction(actionForm.taskId,actionForm.channelId,actionForm.event);await loadActions();}
async function removeAction(id:number){await api.deleteNotificationAction(id);await loadActions();}
async function openRuns(){view.value="runs";tasks.value=await api.tasks();const groups=await Promise.all(tasks.value.map((task)=>api.taskRuns(task.id)));allRuns.value=groups.flat().sort((a,b)=>(b.created_at??0)-(a.created_at??0));}

async function openSubscriptions() {
  view.value = "subscriptions";
  [subscriptions.value, templates.value] = await Promise.all([api.subscriptions(), api.templates()]);
}
async function saveSubscription() {
  await api.createSubscription(subForm.name, subForm.url);
  Object.assign(subForm, { name: "", url: "" });
  await openSubscriptions();
}
async function toggleSubscription(sub: TemplateSubscription) {
  await api.updateSubscription(sub.id, !sub.enabled);
  await openSubscriptions();
}
async function removeSubscription(id: number) {
  if (!window.confirm("删除该订阅？")) return;
  await api.deleteSubscription(id);
  await openSubscriptions();
}
async function syncSubscription(id: number) {
  await api.syncSubscription(id);
  subSyncs.value = await api.subscriptionSyncs(id);
}
async function showSubSyncs(id: number) {
  subSyncs.value = await api.subscriptionSyncs(id);
}
async function openPush() {
  view.value = "push";
  [pushRequests.value, myPushRequests.value, templates.value] = await Promise.all([
    api.adminPushRequests("pending").catch(() => []),
    api.myPushRequests(),
    api.templates()
  ]);
}
async function submitPush() {
  await api.createPushRequest(pushTemplateId.value, pushNote.value);
  pushNote.value = ""; pushTemplateId.value = 0;
  await openPush();
}
async function decidePush(id: number, approve: boolean) {
  await api.decidePushRequest(id, approve);
  await openPush();
}
async function openAdmin() {
  view.value = "admin";
  [adminUsers.value, adminSettings.value] = await Promise.all([api.adminUsers(), api.adminSettings()]);
  const requireEmail = adminSettings.value.find((s) => s.key === "require_email_verification");
  const ga = adminSettings.value.find((s) => s.key === "ga_key");
  const retention = adminSettings.value.find((s) => s.key === "logs.retention_days");
  settingsForm.requireEmail = requireEmail?.value === true;
  settingsForm.gaKey = typeof ga?.value === "string" ? ga.value : "";
  settingsForm.retentionDays = typeof retention?.value === "number" ? retention.value : 0;
}
async function toggleUser(user: User) {
  await api.adminUpdateUser(user.id, { disabled: !user.disabled });
  await openAdmin();
}
async function saveAdminSettings() {
  await api.adminSetSetting("require_email_verification", settingsForm.requireEmail);
  if (settingsForm.gaKey) await api.adminSetSetting("ga_key", settingsForm.gaKey);
  await api.adminSetSetting("logs.retention_days", settingsForm.retentionDays);
  await openAdmin();
}
async function downloadBackup() {
  const backup = await api.adminBackup();
  const blob = new Blob([JSON.stringify(backup, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url; a.download = `qdrust-backup-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.json`;
  a.click(); URL.revokeObjectURL(url);
}
async function restoreBackup(file: File) {
  const text = await file.text();
  await api.adminRestore(JSON.parse(text));
  await openAdmin();
}

onMounted(async () => {
  try { const session = await api.session(); currentUser.value = session.user; authenticated.value = true; await load(); }
  catch { authenticated.value = false; loading.value = false; }
});
</script>

<template>
  <main v-if="!authenticated" class="auth-page">
    <form class="auth-panel" @submit.prevent="authenticate">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <h1>{{ authMode === "login" ? t('login') : authMode === "bootstrap" ? t('bootstrap') : "注册" }}</h1>
      <label>用户名<input v-model="authForm.username" required autocomplete="username" /></label>
      <label v-if="authMode === 'register'">邮箱（可选，用于验证）<input v-model="authForm.email" type="email" autocomplete="email" /></label>
      <label>密码<input v-model="authForm.password" required minlength="12" type="password" :autocomplete="authMode === 'login' ? 'current-password' : 'new-password'" /></label>
      <div v-if="error" class="error-banner">{{ error }}</div>
      <button class="primary-button" :disabled="saving" type="submit">{{ saving ? "处理中" : authMode === "login" ? "登录" : authMode === "bootstrap" ? "创建管理员" : "注册" }}</button>
      <button class="secondary-button" type="button" @click="authMode = authMode === 'login' ? 'bootstrap' : authMode === 'bootstrap' ? 'register' : 'login'; error = ''">{{ authMode === "login" ? "首次使用？初始化管理员" : authMode === "bootstrap" ? "需要账户？注册" : "已有账户？返回登录" }}</button>
    </form>
  </main>
  <div v-else class="app-shell">
    <aside :class="['sidebar', { open: menuOpen }]">
      <div class="brand"><span class="brand-mark"><Zap :size="18" /></span><span>qdrust</span></div>
      <nav aria-label="主导航">
        <a :class="['nav-link',{active:view==='tasks'}]" href="#" @click.prevent="view='tasks'"><LayoutDashboard :size="18" />{{ t('tasks') }}</a>
        <a :class="['nav-link',{active:view==='templates'}]" href="#" @click.prevent="openTemplates"><FileJson2 :size="18" />{{ t('templates') }}</a>
        <a :class="['nav-link',{active:view==='notes'}]" href="#" @click.prevent="openNotes"><FileJson2 :size="18" />{{ t('notes') }}</a>
        <a :class="['nav-link',{active:view==='plugins'}]" href="#" @click.prevent="openPlugins"><Settings :size="18" />{{ t('plugins') }}</a>
        <a :class="['nav-link',{active:view==='notifications'}]" href="#" @click.prevent="openNotifications"><Activity :size="18" />{{ t('notifications') }}</a>
        <a :class="['nav-link',{active:view==='runs'}]" href="#" @click.prevent="openRuns"><Activity :size="18" />运行记录</a>
        <a :class="['nav-link',{active:view==='subscriptions'}]" href="#" @click.prevent="openSubscriptions"><RefreshCw :size="18" />订阅</a>
        <a v-if="currentUser?.role === 'admin'" :class="['nav-link',{active:view==='push'}]" href="#" @click.prevent="openPush"><FileJson2 :size="18" />发布审批</a>
        <a v-if="currentUser?.role === 'admin'" :class="['nav-link',{active:view==='admin'}]" href="#" @click.prevent="openAdmin"><Settings :size="18" />管理</a>
        <a :class="['nav-link',{active:view==='settings'}]" href="#" @click.prevent="view='settings'"><Settings :size="18" />设置</a>
      </nav>
      <div class="sidebar-bottom">
        <a class="nav-link" href="#"><CircleHelp :size="18" />帮助</a>
        <div class="system-state"><span :class="['state-dot', { online: ready }]" />{{ ready ? "服务正常" : "连接中" }}</div>
      </div>
    </aside>

    <div v-if="menuOpen" class="scrim" @click="menuOpen = false" />
    <main v-if="view !== 'runs' && view !== 'settings' && view !== 'subscriptions' && view !== 'push' && view !== 'admin'">
      <header class="topbar">
        <button class="icon-button mobile-menu" title="打开导航" @click="menuOpen = true"><Menu :size="20" /></button>
        <div class="breadcrumb">工作区 <span>/</span> {{ currentViewName }}</div>
        <button class="icon-button" :title="locale" @click="toggleLocale">{{ locale === 'zh-CN' ? 'EN' : '中' }}</button><span class="account-name">{{ currentUser?.username }} · {{ currentUser?.role }}</span><button class="avatar" :title="t('logout')" @click="logout">{{ currentUser?.username.slice(0, 2).toUpperCase() }}</button>
      </header>

      <div v-if="view === 'tasks'" class="page">
        <section class="page-heading">
          <div><p class="eyebrow">AUTOMATION</p><h1>任务</h1><p>管理计划任务与 HTTP 自动化。</p></div>
          <button class="primary-button" @click="showCreate = true"><Plus :size="17" />新建任务</button>
        </section>

        <section class="stats" aria-label="任务概览">
          <div><span>任务总数</span><strong>{{ tasks.length }}</strong><small><CalendarClock :size="14" />已配置</small></div>
          <div><span>运行中</span><strong>{{ activeCount }}</strong><small class="positive"><Activity :size="14" />调度已启用</small></div>
          <div><span>最近成功</span><strong>{{ successCount }}</strong><small><Check :size="14" />有运行结果</small></div>
        </section>

        <section class="task-section">
          <div class="toolbar">
            <label class="search"><Search :size="17" /><input v-model="search" type="search" placeholder="搜索任务或 URL" /></label>
            <button class="icon-button" title="刷新" @click="load"><RefreshCw :class="{ spin: loading }" :size="18" /></button>
          </div>

          <div v-if="error" class="error-banner">{{ error }}<button title="关闭" @click="error = ''"><X :size="16" /></button></div>
          <div v-if="loading" class="loading-state"><RefreshCw class="spin" :size="22" />正在读取任务</div>
          <div v-else-if="filtered.length === 0" class="empty-state">
            <span><CalendarClock :size="25" /></span><h2>{{ search ? "没有匹配的任务" : "创建第一个自动化任务" }}</h2>
            <button v-if="!search" class="secondary-button" @click="showCreate = true"><Plus :size="16" />新建任务</button>
          </div>
          <div v-else class="table-wrap">
            <table>
              <thead><tr><th>任务</th><th>计划</th><th>最近运行</th><th>状态</th><th><span class="sr-only">操作</span></th></tr></thead>
              <tbody>
                <template v-for="task in filtered" :key="task.id">
                <tr>
                  <td><div class="task-name"><span :class="['method', task.method.toLowerCase()]">{{ task.method }}</span><div><strong>{{ task.name }}</strong><small>{{ task.url }}</small></div></div></td>
                  <td><code>{{ task.cron }}</code></td>
                  <td>{{ formatRunTime(task.last_run_at) }}</td>
                  <td><button :class="['status-pill', { paused: task.disabled }]" @click="toggle(task)"><span />{{ task.disabled ? "已暂停" : "运行中" }}</button></td>
                  <td class="row-actions"><button class="icon-button" title="立即运行" @click="runNow(task)"><Zap :size="17" /></button><button class="icon-button" title="运行记录" @click="showRuns(task)"><Activity :size="17" /></button><button class="icon-button" title="删除任务" @click="remove(task)"><Trash2 :size="17" /></button><button class="icon-button" title="更多操作"><MoreHorizontal :size="18" /></button></td>
                </tr>
                <tr v-if="expandedTask === task.id" class="run-detail"><td colspan="5"><div v-if="runs.length === 0">暂无运行记录</div><div v-for="run in runs" :key="run.id" class="run-row"><span>#{{ run.id }}</span><strong>{{ run.status }}</strong><span v-if="run.http_status">HTTP {{ run.http_status }}</span><span v-if="run.id === runs[0]?.id">步骤 {{ steps.length }}</span><span v-if="run.error">{{ run.error }}</span><button v-if="['pending','leased','running'].includes(run.status)" class="icon-button" title="取消运行" @click="cancel(run)"><X :size="15" /></button></div></td></tr>
                </template>
              </tbody>
            </table>
          </div>
        </section>
      </div>
      <div v-else-if="view==='templates'" class="page"><section class="page-heading"><div><p class="eyebrow">TEMPLATES</p><h1>模板</h1></div><button class="primary-button" @click="showImport=true"><Plus :size="17" />导入 QD HAR</button></section><section class="task-section"><h2>个人模板</h2><div v-for="item in templates" :key="item.id" class="run-row"><strong>{{ item.name }}</strong><span>{{ item.source_format }}</span><button v-if="item.source_format==='qd_har'" class="secondary-button" @click="editTemplate(item)">编辑</button><button class="secondary-button" @click="publish(item.id)">发布</button><button class="icon-button" title="删除模板" @click="removeTemplate(item.id)"><Trash2 :size="16" /></button></div><h2>公共模板</h2><div v-for="item in publicTemplates" :key="item.id" class="run-row"><strong>{{ item.name }}</strong><span>{{ item.source_format }}</span><button class="secondary-button" @click="copyTemplate(item.id)">复制到个人模板</button></div></section></div>
      <div v-else-if="view==='notes'" class="page"><section class="page-heading"><div><p class="eyebrow">NOTES</p><h1>记事本</h1></div></section><section class="task-section"><form class="modal" @submit.prevent="saveNote"><label>标题<input v-model="noteForm.title" required /></label><label>内容<textarea v-model="noteForm.content" rows="8" /></label><button class="primary-button">{{ noteForm.id?'保存':'新建记事' }}</button></form><div v-for="note in notes" :key="note.id" class="run-row"><strong>{{ note.title }}</strong><button class="secondary-button" @click="editNote(note)">编辑</button><button class="icon-button" title="删除记事" @click="removeNote(note.id)"><Trash2 :size="16" /></button></div></section></div>
      <div v-else-if="view==='plugins'" class="page"><section class="page-heading"><div><p class="eyebrow">PLUGINS</p><h1>插件</h1></div></section><section class="task-section"><form class="modal" @submit.prevent="savePlugin"><label>名称<input v-model="pluginForm.name" required /></label><label>可执行命令<input v-model="pluginForm.command" required /></label><button class="primary-button">注册插件</button></form><div class="run-row"><label>Action <input v-model="invokeForm.action" /></label><label>Query JSON <input v-model="invokeForm.query" /></label></div><div v-for="plugin in plugins" :key="plugin.id" class="run-row"><strong>{{ plugin.name }}</strong><code>{{ plugin.command }}</code><button v-if="plugin.enabled" class="secondary-button" @click="invokePlugin(plugin)">调用</button><button class="secondary-button" @click="togglePlugin(plugin)">{{ plugin.enabled?'禁用':'启用' }}</button><button class="icon-button" title="删除插件" @click="removePlugin(plugin.id)"><Trash2 :size="16" /></button></div><pre v-if="pluginResult"><code>{{ pluginResult }}</code></pre><div v-if="error" class="error-banner">{{ error }}</div></section></div>
      <div v-else class="page"><section class="page-heading"><div><p class="eyebrow">NOTIFICATIONS</p><h1>通知</h1></div></section><section class="task-section"><form class="modal" @submit.prevent="saveChannel"><label>渠道名称<input v-model="channelForm.name" required /></label><label>Webhook HTTPS URL<input v-model="channelForm.url" required type="url" /></label><button class="primary-button">创建 Webhook</button></form><div v-for="channel in channels" :key="channel.id" class="run-row"><strong>{{ channel.name }}</strong><span>{{ channel.kind }}</span><button class="secondary-button" @click="toggleChannel(channel)">{{ channel.enabled?'禁用':'启用' }}</button><button class="icon-button" title="删除渠道" @click="removeChannel(channel.id)"><Trash2 :size="16" /></button></div><h2>任务通知动作</h2><form class="modal" @submit.prevent="saveAction"><label>任务<select v-model="actionForm.taskId" required @change="loadActions"><option :value="0" disabled>选择任务</option><option v-for="task in tasks" :key="task.id" :value="task.id">{{ task.name }}</option></select></label><label>渠道<select v-model="actionForm.channelId" required><option :value="0" disabled>选择渠道</option><option v-for="channel in channels" :key="channel.id" :value="channel.id">{{ channel.name }}</option></select></label><label>事件<select v-model="actionForm.event"><option value="success">成功</option><option value="failure">失败</option><option value="always">始终</option></select></label><button class="primary-button">添加动作</button></form><div v-for="action in actions" :key="action.id" class="run-row"><strong>{{ action.event }}</strong><span>渠道 #{{ action.channel_id }}</span><button class="icon-button" title="删除动作" @click="removeAction(action.id)"><Trash2 :size="16" /></button></div></section></div>
    </main>

    <div v-if="view === 'runs'" class="page overlay-page"><section class="page-heading"><div><p class="eyebrow">RUN HISTORY</p><h1>运行记录</h1><p>查看所有任务的最近执行结果。</p></div><button class="secondary-button" @click="load"><RefreshCw :size="16" />刷新</button></section><section class="task-section content-panel"><div v-if="allRuns.length === 0" class="empty-state"><Activity :size="24" /><h2>暂无运行记录</h2></div><div v-else class="run-list"><div v-for="run in allRuns" :key="run.id" class="run-row"><span class="run-id">#{{ run.id }}</span><strong>{{ taskName(run.task_id) }}</strong><span class="status-text">{{ run.status }}</span><span v-if="run.http_status">HTTP {{ run.http_status }}</span><span class="run-time">{{ formatRunTime(run.started_at ?? run.created_at) }}</span></div></div></section></div>
    <div v-if="view === 'subscriptions'" class="page overlay-page">
      <section class="page-heading"><div><p class="eyebrow">TEMPLATE SOURCES</p><h1>订阅</h1><p>订阅 GitHub 仓库或模板文件 URL，自动导入 QD HAR 模板。</p></div></section>
      <section class="task-section content-panel">
        <form class="modal" @submit.prevent="saveSubscription"><label>订阅名称<input v-model="subForm.name" required /></label><label>GitHub 仓库或文件 URL<input v-model="subForm.url" required type="url" placeholder="https://github.com/owner/repo 或 https://example.com/template.har.json" /></label><button class="primary-button">添加订阅</button></form>
        <div v-for="sub in subscriptions" :key="sub.id" class="run-row">
          <strong>{{ sub.name }}</strong><span>{{ sub.url }}</span>
          <span v-if="sub.last_synced_at" class="run-time">上次同步 {{ formatRunTime(sub.last_synced_at) }}</span>
          <span v-if="sub.last_error" class="error-banner" style="margin:0">{{ sub.last_error }}</span>
          <button class="secondary-button" @click="syncSubscription(sub.id)">同步</button>
          <button class="secondary-button" @click="showSubSyncs(sub.id)">记录</button>
          <button class="secondary-button" @click="toggleSubscription(sub)">{{ sub.enabled ? "停用" : "启用" }}</button>
          <button class="icon-button" title="删除订阅" @click="removeSubscription(sub.id)"><Trash2 :size="16" /></button>
        </div>
        <h2 v-if="subSyncs.length">同步记录</h2>
        <div v-for="s in subSyncs" :key="s.id" class="run-row"><span>#{{ s.id }}</span><strong>{{ s.status }}</strong><span>{{ s.message }}</span><span class="run-time">{{ formatRunTime(s.created_at) }}</span></div>
      </section>
    </div>
    <div v-if="view === 'push'" class="page overlay-page">
      <section class="page-heading"><div><p class="eyebrow">PUBLISH REVIEW</p><h1>发布审批</h1><p>提交模板到公共库，由管理员审批。</p></div></section>
      <section class="task-section content-panel">
        <h2>我的请求</h2>
        <form class="modal" @submit.prevent="submitPush"><label>模板<select v-model="pushTemplateId" required><option :value="0" disabled>选择模板</option><option v-for="t in templates" :key="t.id" :value="t.id">{{ t.name }}</option></select></label><label>说明<textarea v-model="pushNote" rows="3" placeholder="为什么发布？" /></label><button class="primary-button">提交发布请求</button></form>
        <div v-for="r in myPushRequests" :key="r.id" class="run-row"><strong>模板 #{{ r.template_id }}</strong><span>{{ r.status }}</span><span v-if="r.note">{{ r.note }}</span></div>
        <h2>待审批</h2>
        <div v-for="r in pushRequests" :key="r.id" class="run-row"><strong>模板 #{{ r.template_id }}</strong><span>请求者 #{{ r.owner_id }}</span><span v-if="r.note">{{ r.note }}</span><button class="secondary-button" @click="decidePush(r.id, true)">批准</button><button class="secondary-button" @click="decidePush(r.id, false)">拒绝</button></div>
      </section>
    </div>
    <div v-if="view === 'admin'" class="page overlay-page">
      <section class="page-heading"><div><p class="eyebrow">ADMINISTRATION</p><h1>管理</h1><p>用户、站点设置、备份与恢复。</p></div></section>
      <section class="task-section content-panel">
        <h2>用户</h2>
        <div v-for="user in adminUsers" :key="user.id" class="run-row"><strong>{{ user.username }}</strong><span>{{ user.role }}</span><span v-if="user.email">{{ user.email }}</span><span>{{ user.email_verified ? "已验证" : "未验证" }}</span><button class="secondary-button" @click="toggleUser(user)">{{ user.disabled ? "启用" : "禁用" }}</button></div>
        <h2>站点设置</h2>
        <form class="modal" @submit.prevent="saveAdminSettings">
          <label class="checkbox"><input v-model="settingsForm.requireEmail" type="checkbox" />注册后必须验证邮箱</label>
          <label>GA_KEY <input v-model="settingsForm.gaKey" placeholder="G-XXXXXXX" /></label>
          <label>日志保留天数 <input v-model.number="settingsForm.retentionDays" type="number" min="0" /></label>
          <button class="primary-button">保存设置</button>
        </form>
        <h2>备份 / 恢复</h2>
        <div class="run-row"><button class="secondary-button" @click="downloadBackup">导出 JSON 备份</button><label class="secondary-button" style="margin:0">导入恢复<input type="file" accept="application/json" style="display:none" @change="(event) => restoreBackup((event.target as HTMLInputElement).files![0])" /></label></div>
      </section>
    </div>
    <div v-if="view === 'settings'" class="page overlay-page"><section class="page-heading"><div><p class="eyebrow">WORKSPACE</p><h1>设置</h1><p>查看当前工作区与账户状态。</p></div></section><section class="settings-grid"><div class="content-panel"><p class="eyebrow">ACCOUNT</p><h2>账户</h2><dl class="settings-list"><div><dt>用户名</dt><dd>{{ currentUser?.username }}</dd></div><div><dt>角色</dt><dd>{{ currentUser?.role }}</dd></div></dl></div><div class="content-panel"><p class="eyebrow">SERVICE</p><h2>服务</h2><dl class="settings-list"><div><dt>状态</dt><dd><span class="state-dot online" />正常</dd></div><div><dt>API 文档</dt><dd><a href="/api/v1/openapi.json" target="_blank">OpenAPI</a></dd></div></dl></div></section></div>

    <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
      <form class="modal" @submit.prevent="createTask">
        <div class="modal-header"><div><p class="eyebrow">NEW AUTOMATION</p><h2>新建任务</h2></div><button class="icon-button" type="button" title="关闭" @click="showCreate = false"><X :size="20" /></button></div>
        <label>任务名称<input v-model="form.name" required maxlength="100" placeholder="例如：每日状态检查" /></label>
        <div class="form-row"><label>请求方法<select v-model="form.method"><option>GET</option><option>POST</option><option>PUT</option><option>PATCH</option><option>DELETE</option></select><ChevronDown :size="16" /></label><label>Cron 表达式<input v-model="form.cron" required /></label></div>
        <label>请求 URL<input v-model="form.url" required type="url" placeholder="https://example.com/api/health" /></label>
        <label class="checkbox"><input v-model="form.disabled" type="checkbox" />创建后先暂停</label>
        <div class="modal-actions"><button class="secondary-button" type="button" @click="showCreate = false">取消</button><button class="primary-button" :disabled="saving" type="submit">{{ saving ? "创建中" : "创建任务" }}</button></div>
      </form>
    </div>
  </div>
  <div v-if="showImport" class="modal-backdrop"><form class="modal" @submit.prevent="importHar"><div class="modal-header"><h2>导入 QD HAR</h2><button class="icon-button" type="button" title="关闭" @click="showImport=false"><X :size="20" /></button></div><label>模板名称<input v-model="importForm.name" required /></label><label>描述<input v-model="importForm.description" /></label><label>HAR JSON<textarea v-model="importForm.har" required rows="14" spellcheck="false" /></label><div v-if="error" class="error-banner">{{ error }}</div><div class="modal-actions"><button class="primary-button" :disabled="saving">{{ saving?'导入中':'导入' }}</button></div></form></div>
</template>
