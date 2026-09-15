# 使用

> 返回 [README](../README.md)

## 导入 QD HAR 模板

- 在 WebUI 的模板页点击导入，选择从旧 QD 导出的 `*.har.json`。
- 导入时会校验并**保留原始 HAR JSON**，兼容样本见 `tests/fixtures`。
- 也可用命令行校验：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

### 模板变量是怎么算出来的

基于模板新建任务时，WebUI 会按 QD `HARSave.get_variables` 的语义列出需要填写的变量（`GET /api/v1/templates` 返回的 `variables` 字段）：

- 只看请求实际读取的名字：method、URL、请求体，以及每个请求头 / Cookie 的名字与值；
- 用 Jinja 解析，所以 **过滤器和函数名不算变量**：`{{jpop_username|urlencode}}` 只产出 `jpop_username`，`{{md5(password)}}` 只产出 `password`；
- **按条目顺序判定**：只有被 `extract_variables` 提取**之后**的引用才不算输入。所以登录后的 `{{token}}` 不会被问，而先喂给 `api://util/gb2312` 再由该条目回写提取的 `{{username}}` 仍然是输入；
- 无法解析的片段（如 `{% while ... %}` 控制条目）不产生变量。

`native_v1` 模板则直接用定义里声明的变量名。

## 原生模板 schema v1

除导入旧 QD HAR 外，也可以直接写 qdrust 原生模板。它是版本化 JSON，**不执行 Python**，顶层必须包含 `version: 1`、非空 `name` 和 `steps`：

```json
{
  "version": 1,
  "name": "Health check",
  "variables": {
    "base_url": "https://example.com"
  },
  "steps": [
    {
      "type": "request",
      "name": "Fetch health",
      "method": "GET",
      "url": "{{base_url}}/health",
      "headers": {"accept": "application/json"}
    },
    {
      "type": "extract",
      "name": "Read status",
      "source": "status",
      "selector": "",
      "target": "status",
      "required": true
    }
  ]
}
```

步骤类型：`request`、`extract`、`if`、`for_each`、`delay`；请求 body 支持 `json`、`text` 和 `form`。表达式与 `api://util/*`、`api://browser/*` 的用法见 [模板表达式与内置工具](expressions.md)。

安全限制：最多 1000 个静态步骤、最多 16 层嵌套、单次 delay 最长 5 分钟；运行时还会独立限制请求数、循环次数、响应体和总时长。

## 创建与运行任务

1. **模板必选**：新建任务时先选模板——任务的请求完全由模板决定（多页模板常混合 GET/POST），任务本身不携带请求方法 / URL / 请求头 / 请求体。
2. 填写变量（模板需要的变量会按 QD 语义自动列出）、配置 cron / 间隔调度。
3. 用「立即运行」手动触发，或在运行记录中查看每一步的请求 / 响应 / 提取变量；支持取消正在执行的运行。
4. 模板页提供两种导入来源：从本地导入 QD HAR，或订阅一个模板库后按需挑选（见[订阅模板库](#订阅模板库选择性导入)）；公共模板经 PushRequest 审批后发布。

> 任务只保存「模板 + 变量 + 调度」，任务列表仅展示任务名——请求方法与 URL 在列表里既冗余又难扫读，需要时到任务详情里看（那里仍会镜像模板首条请求）。早于该规则的存量任务（未绑定模板、自带一条请求）编辑时仍会保留「（无模板）」选项与其原有请求。

## 可视化 HAR 编辑器

WebUI 提供可视化编辑器，可直接增删改请求、设置请求头 / 表单 / 鉴权、编写 `success_asserts` 与 `extract_variables`，无需手改 JSON。

## 调度与随机延迟

可视化调度器支持固定间隔与 cron；「随机延迟」可让同一任务的多次执行在时间上打散，避免被目标站点识别为固定节奏。模板变量支持在创建任务时**预填**默认值。

### 时区与夏令时

cron 按每个任务各自的 IANA 时区（`timezone` 字段）求值，**DST 感知**：设 `America/New_York` 的任务，`0 9 * * *` 会全年在当地 09:00 触发，无需为冬令/夏令手改 cron。任务未设 `timezone` 时使用服务端 `QDRUST_DEFAULT_TIMEZONE`（默认 `Asia/Shanghai`）。WebUI 中任务列表与运行历史的「上次运行/开始时间」也按该任务时区展示（未设时区则用查看者浏览器本地时区）。

## 运行日志与历史

每次运行会记录 QD 风格的文本日志；在任务详情页可查看该任务的运行历史，并支持**清空**历史记录。活跃运行的步骤通过 WebSocket 实时推送。

**运行日志总览**（任务页点标题右侧「运行日志」展开，不是独立页面）把**所有任务**的运行放在一张表里，按时间倒序，可按「全部 / 成功 / 失败」筛选、按任务过滤，支持分页加载更多，并能在行内取消或删除。任务多的时候用它一眼扫完当天所有任务，不必逐个点进任务详情；点任务名可以直接跳到该任务的运行历史。对应接口为 `GET /api/v1/runs`（`status` / `task_id` / `limit` / `before_id` 参数，游标分页）。

## 订阅模板库（选择性导入）

支持订阅官方模板库 <https://github.com/qd-today/templates>，以及任何兼容同一规范的第三方库或单个模板文件 URL。订阅是模板页的第二种导入来源——没有单独的「订阅」页。

1. 在「模板」页点标题右侧的「从订阅导入」展开订阅面板，填入库地址（例如 `https://github.com/qd-today/templates`），并选择导入方式：
   - **按需导入**（默认）：库只当目录用，**不会**自动把模板拉进本地——官方库里是几百个模板，全量导入会把你自己建的模板淹掉；
   - **全部导入**：每次同步把来源里的全部模板导入本地，只有这种订阅会被后台定时同步。
2. 点该订阅的「模板库」进入库浏览页：列出全部条目（名称、作者、版本、日期、变量说明），可搜索名称 / 作者，可按「全部 / 已导入 / 有更新」筛选；勾选后点「导入所选」。库浏览页的「返回模板」回到模板页，订阅面板保持展开。
3. 已导入的条目标「已导入」；上游版本变新时标「有更新」，再次勾选导入即**原地更新**，不会生成重名副本。
4. 「记录」保留每次全量同步的进度与结果（同步过程可通过 WebSocket 实时查看）。

来源的发现规则（与 QD 第三方库规范一致）：

- 仓库根目录有 `tpls_history.json` 时按其清单读取：`har` 以模板名为键，值里的 `author` / `comments` / `version` / `date` / `commenturl` 就是上面展示的元数据；HAR 优先取内联的 base64 `content`，否则按 `url`、再否则按 `filename` 下载。
- 没有清单时回退为扫描仓库树中的 `*.har` / `*.json`，此时条目不携带版本，因此不会提示更新。
- 变量说明（`comments`）在服务端被规整为纯文本，不注入到页面里。

来源记录写在 `template_imports`（订阅 + 条目名 → 本地模板 + 上游版本），用于判断「已导入 / 有更新」。删除订阅只删除来源记录，**已导入的模板会保留**（导入是复制到你自己的库，不是软链）。

对应接口：`GET /api/v1/subscriptions/{id}/library`（列目录）与 `POST /api/v1/subscriptions/{id}/import`（导入所选，逐条返回成功 / 更新 / 失败）。

## 命令行（CLI）

校验旧 QD HAR：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

执行 HAR，并传入变量和整体超时：

```powershell
cargo run -p qdrust-cli -- run .\template.har.json --var token=abc --timeout 60
```

CLI 默认拒绝私网、localhost 和无效 TLS 证书，不继承旧 QD 的宽松网络策略。
