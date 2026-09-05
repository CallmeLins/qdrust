# qdrust

> 一个用 Rust + Vue 3 重写的 **QD 兼容 HTTP 定时任务自动执行框架**，基于 HAR 编辑器与 Axum 服务。

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-linux%20%2F%20amd64%20%2F%20arm64-blue)](https://github.com/)

qdrust 是按 [QD](https://github.com/qd-today/qd)（HTTP 请求定时任务自动执行框架）路线重新设计的 Rust 实现：解析并执行旧 QD 的 HAR 模板，提供命令行工具、REST API、定时调度与 WebUI。它是一个**全新项目**，不导入旧 QD 数据库、不复用旧登录 Cookie、不承诺兼容旧 URL / 旧 API / 任意 Python 动态表达式——但对核心兼容契约（旧 QD HAR）做了完整对齐，并补齐了 Jinja2 表达式与 `api://util/*` 内置工具。

---

## 介绍

### 它解决什么问题

把浏览器里抓到的 HTTP 请求（HAR）存成模板，按 cron 或固定间隔自动执行，用于签到、监控、API 轮询等定时任务；执行结果可通过 10 种渠道推送通知。

### 核心特性

- **旧 QD HAR 兼容**：导入、校验、保留原始 HAR JSON，并重放执行。
- **可视化 HAR 编辑器**：在 WebUI 里直接编辑请求、变量与断言，无需手写 JSON。
- **双后端数据库**：SQLite（开箱即用）与 MySQL（按 `DATABASE_URL` 自动选择）。
- **完整通知体系**：Webhook + Email + 8 种推送渠道，共 **10 种**。
- **模板表达式**：26 个 Jinja2 过滤器 + 38 个表达式函数，以及 `api://util/*` 内置工具（时间 / 编码 / 哈希 / 正则 / JSON / RSA / OCR）。
- **无头浏览器插件**（可选）：`api://browser/*` 通过 CDP 驱动远程无头浏览器，补齐"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 做不了的一步。支持跨步骤存活的会话复用与 `type`/`click` DOM 操作，结果可提取成变量回填到后续请求（详见 [浏览器插件](#浏览器插件无头浏览器签到)）。
- **任务调度**：分组、批量操作、可视化调度器（含随机延迟）、模板变量预填。
- **运行可观测**：QD 风格运行日志、按任务查看运行历史、WebSocket 实时步骤流。
- **安全与多租户**：开放注册 / 忘记密码 / 邮箱验证 / CSRF 轮换；所有用户资源均做服务端归属校验。
- **运维友好**：Docker 镜像（amd64 / arm64）、管理员备份恢复、运行时配置热更新、可选 Redis 会话缓存。

### 与 QD 的刻意差异

- **单进程异步模型**（`axum::serve` + Tokio），而非 Tornado 多进程；横向扩展靠 Docker 副本 + 反向代理。
- **非持久化本地队列**基于 SQLite 事务（`claim_run` + 租约恢复），保证单实例正确性与多实例不重复执行。

---

## 组成

- `crates/qdrust-core`：旧 QD HAR 解析、编译、变量求值和 HTTP 执行核心。
- `crates/qdrust-cli`：无需启动服务即可校验和执行 HAR。
- `crates/qdrust-server`：基于 Axum、SQLx 和 SQLite/MySQL 的 API、认证、调度与运行管理。
- `webui`：基于 Vue 3、TypeScript 和 Vite 的 WebUI。

当前服务包含用户认证与会话（开放注册、忘记/重置密码、邮箱验证、CSRF 轮换）、管理员 API（用户管理、站点设置、日志清理、备份/恢复）、模板（搜索/分组/分页、可视化 HAR 编辑器、公共发布审批 PushRequest、订阅仓库自动导入）、任务（分组/批量操作、可视化调度器含随机延迟、模板变量预填）、运行记录与步骤（QD 风格运行日志、按任务查看运行历史并支持清空）、WebSocket 实时步骤流（运行 + 订阅进度）、租约恢复、插件系统、通知（Webhook + Email + 8 种推送渠道，共 10 种）、记事本、可选 Redis 会话缓存、GA 注入、运行时配置热更新以及 OpenAPI。模板支持 26 个 Jinja2 过滤器与 38 个表达式函数，以及 `api://util/*` 内置工具（见下文）。支持 SQLite 与 MySQL 双后端（按 DATABASE_URL 自动选择）。所有用户资源均在服务端执行归属校验。

---

## 模板表达式与内置工具

模板变量与步骤请求支持两种动态求值方式，兼容旧 QD HAR：

- **Jinja2 表达式**：由 `minijinja` 驱动，内置 26 个过滤器（`upper`/`lower`/`replace`/`split`/`join`/`sort`/`unique`/`tojson`/`fromjson` 等）和 38 个表达式函数（`int`/`float`/`len`/`b64encode`/`md5`/`sha1`/`hash`/`timestamp`/`strftime`/`random_int`/`fake`/`regex_*` 等）。
- **`api://util/*` 内置工具**：当模板步骤 URL 以 `api://` 开头时，executor 在进程内计算并返回结果，不发起真实 HTTP 请求。时间、编码、哈希、正则、JSON 类工具已与 QD 对齐：

  | 工具 | 说明 |
  |---|---|
  | `delay` | 固定 / 随机延迟 |
  | `timestamp` | 当前时间戳（多种格式） |
  | `unicode` / `urldecode` / `urlencode` | 编码转换 |
  | `gb2312` | GB2312 百分号编码（urllib.quote 语义） |
  | `base64` (encode / decode) | Base64 编解码 |
  | `hash` (md5 / sha1 / sha256 / sha512) | 哈希 |
  | `uuid` / `random` (float) | 随机值 |
  | `regex` (findall / replace) | 正则提取与替换 |
  | `string/replace` | 正则替换，支持组引用与文本模式 |
  | `rsa` (encode / decode) | PKCS1 v1.5 加解密 |
  | `json` (parse / stringify / pretty) | JSON 处理 |
  | `dddd/*` | OCR / 验证码识别，转发到外部 DdddOCR 服务 |

  外部插件二进制（带 manifest、API 版本校验与能力声明 network / read_file / write_file / environment）通过子进程 JSON 协议调用，WebUI 提供插件管理页。

- **`api://browser/*` 浏览器插件（可选）**：进程内持有 chromiumoxide 驱动远程无头浏览器，处理"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 步骤做不了的一步。配置 `QDRUST_BROWSER_URL` 即可启用（无需在插件管理页新建条目），`content`/`eval`/`screenshot`/`start`/`end`/`type`/`click`/`keepalive` 等 action 见 [浏览器插件（无头浏览器签到）](#浏览器插件无头浏览器签到)。

---

## 浏览器插件（无头浏览器签到）

qdrust 的签到任务**以 HAR 为主流程**（HTTP 请求，fetcher 直接渲染执行），无头浏览器只用来补上纯 HTTP 做不了的那一步。因此推荐**混合用法**：只有真正需要浏览器的一步走 `api://browser/*`，其余步骤仍走普通 HTTP 请求。这是刻意的最小设计——浏览器只在"生成签名 / 过验证码 / 渲染 JS / 走多步表单交互"这类场景介入。

配置 `QDRUST_BROWSER_URL` 后插件自动启用（无需在插件管理页新建条目），模板里直接写 `api://browser/<action>` 即可。

> **架构**：chromiumoxide（CDP 客户端）在 qdrust server 进程内直接持有，进程启动时创建一次、全局共享。不再像早期版本那样每次调用拉起一个独立子进程二进制。这样 DOM 状态可以在多次独立调用间存活，是实现"多步页面交互 / 人工过验证码后继续"的关键。进程重启即清空会话（向导需重新 `start`）。

### Action 一览

浏览器插件分**一次性**与**会话**两类 action：

| Action | 类型 | 用途 | 参数 | 返回体 |
|---|---|---|---|---|
| `start` | 会话 | 开一个标签页并导航到 `url`，返回可复用的会话 id | `url` 必填 | `{"session": "<id>"}` |
| `end` | 会话 | 关闭会话并释放标签页 | `session` 必填 | `{"session": ..., "status": "closed"}` |
| `keepalive` | 会话 | 刷新会话的空闲 TTL（长时间等待人工输入时保活） | `session` 必填 | `{"ok": true, "session": ...}` |
| `type` | 会话 | 在 `selector` 元素上输入 `value`（原生键盘事件，兼容 SPA）；`clear=1` 先清空（Ctrl+A+Backspace），`submit=1` 输入后按回车 | `session`、`selector`、`value` 必填；可选 `clear`、`submit` | `{"ok": true, "session": ..., "selector": ...}` |
| `click` | 会话 | 点击 `selector` 元素；可选 `wait=<ms>` 点击后等待、`wait_selector=<css>` 轮询等某元素出现（最长 15s） | `session`、`selector` 必填；可选 `wait`、`wait_selector` | `{"ok": true, "session": ..., "selector": ...}` |
| `content` | 一次性 | 渲染后取页面 HTML（JS 已执行，适合 SPA / 动态加载） | `url` 必填 | 页面 HTML 文本 |
| `eval` | 一次性 | 在页面里执行 JS，取 token / 签名 / cookie / 页面状态 | `url` 必填，`expr` 必填 | 求值结果的 JSON |
| `screenshot` | 一次性 | 截图（过验证码给人看 / 留档） | `url` 必填，可选 `full_page=1`、`format=png\|jpeg`、`width`/`height`（视口）、`wait=<ms>`（截图前等待） | `{"mimeType": ..., "data": <base64>}` |

**一次性与会话的判定规则**：`content` / `eval` / `screenshot` 若带 `session` 参数则在指定会话页上执行，否则开一张一次性标签页（用完即关）。`type` / `click` 必须带 `session`。

端点支持三后端：

- **Browserless 自托管**：`ws://localhost:3000`（推荐，`compose.yaml` 里已备好注释掉的 `browserless` 服务，取消注释并设置 `QDRUST_BROWSER_URL: ws://browserless:3000` 即可）。
- **本地 Chromium / obscura**：`http://localhost:9222`（带 `--remote-debugging-port=9222` 启动的 Chrome）。
- **Browserless 云端**：`wss://chrome.browserless.io?token=...`。

### 部署启用（三步）

1. 准备一个无头浏览器端点（上面三选一）。
2. 给服务设置 `QDRUST_BROWSER_URL`（Docker 部署在 `compose.yaml` 的 environment 里，见下方示例）。
3. 重启服务。之后模板里就能用 `api://browser/*`，无需在 WebUI 插件管理页新建条目。

`compose.yaml` 中与 Browserless 一起启用的最小配置：

```yaml
services:
  qdrust:
    # ...
    environment:
      QDRUST_BROWSER_URL: ws://browserless:3000
  browserless:
    image: ghcr.io/browserless/chromium:latest
    environment:
      TOKEN: change-me
      CONCURRENT: 5
    restart: unless-stopped
```

> 浏览器客户端代码已编译进 server 二进制，不再需要单独的 `qdrust-plugin-browser` 子进程或 `QDRUST_BROWSER_PLUGIN_BIN` 配置。

### 混合签到：模板怎么写

核心思路：**普通 HTTP 步骤用 HAR 原样请求；需要浏览器的一步插入 `api://browser/*`，把它的返回体用 `extract_variables` 提取成变量，供后续 HAR 步骤用 `{{var}}` 填充。**

#### 一次性用法

##### 例 1：`eval` 取签名 / token，回填到后续 HTTP 请求

浏览器执行 JS 拿到 `window.__sig()` 的结果（返回体是 JSON，如 `"a1b2c3"`），再用正则提出来，填进真正的签到请求：

```jsonc
{
  "entries": [
    {
      // 浏览器步骤：登录页里执行 JS 取签名
      "request": { "method": "GET", "url": "api://browser/eval?url=https://example.com/signin&expr=window.__sig()" },
      "extract_variables": [
        { "name": "sig", "re": "\"([a-zA-Z0-9_-]+)\"", "from": "content" }
      ]
    },
    {
      // 真正的签到请求：把签名回填进 body
      "request": {
        "method": "POST",
        "url": "https://example.com/api/checkin",
        "postData": { "mimeType": "application/json", "text": "{\"sig\":\"{{sig}}\"}" }
      }
    }
  ]
}
```

> `extract_variables` 的 `re` 是对整个返回体文本做正则匹配。`eval` 返回的是 JSON（字符串值带引号），所以取字符串时要带上引号转义：`"([a-zA-Z0-9_-]+)"`；数字/布尔值则不带引号。

##### 例 2：`content` 抓渲染后的页面，正则提取变量

SPA 页面内容由 JS 动态生成，普通 HTTP 拿到的是空壳。用 `content` 拿渲染后的 HTML，再提取：

```jsonc
{
  "request": { "method": "GET", "url": "api://browser/content?url=https://example.com/dashboard" },
  "extract_variables": [
    { "name": "nickname", "re": "nickname[^>]*>([^<]+)<", "from": "content" }
  ]
}
```

##### 例 3：`screenshot` 过验证码 / 留档

截图把当前页面状态打给人看（返回 `data` 为 base64）。适合无法自动破解的验证码场景——人工看图后把答案填进下一步的变量，或单纯用于失败时留档排查：

```jsonc
{
  "request": {
    "method": "GET",
    "url": "api://browser/screenshot?url=https://example.com/captcha&wait=2000&format=png"
  },
  "extract_variables": [
    { "name": "captcha_img", "re": "\"data\":\"([A-Za-z0-9+/=]+)\"", "from": "content" }
  ]
}
```

#### 会话用法：多步页面交互 / 人工过验证码

对于"登录后拿 cookie 再签到"这类**需要在同一页面上连续操作**的流程，用 `start` 开一个会话，跨多个步骤对同一 `session` 做 `type` / `click` / `content`。会话在多次独立调用间保持存活，天然支持中间等人（如人工过验证码）。示意流程：

```jsonc
{
  "entries": [
    {
      // 1. 开会话进登录页，记录下 session id 到变量
      "request": { "method": "GET", "url": "api://browser/start?url=https://example.com/login" },
      "extract_variables": [ { "name": "sess", "re": "\"session\":\"([0-9a-f]+)\"", "from": "content" } ]
    },
    {
      // 2. 往 #user 输入账号
      "request": { "method": "GET", "url": "api://browser/type?session={{sess}}&selector=%23user&value=my_account" }
    },
    {
      // 3. 往 #pass 输入密码，submit=1 回车提交（触发登录跳转）
      "request": { "method": "GET", "url": "api://browser/type?session={{sess}}&selector=%23pass&value=my_pw&submit=1" }
    },
    {
      // 4. 等 dashboard 出现后抓渲染后 HTML，提取登录态信息
      "request": { "method": "GET", "url": "api://browser/content?session={{sess}}&url=https://example.com/dashboard" },
      "extract_variables": [ { "name": "nickname", "re": "nickname[^>]*>([^<]+)<", "from": "content" } ]
    },
    {
      // 5. 用完关会话
      "request": { "method": "GET", "url": "api://browser/end?session={{sess}}" }
    }
  ]
}
```

> **用 `#` 选择器时务必做 URL 编码**（`#user` → `%23user`），否则 `#` 会被当作片段标识符截断 query。会话 id 通过第一步 `extract_variables` 提出并存在变量里，供后续步骤以 `{{sess}}` 复用。

> 需要"人等验证码"的流程，可在人工环节前后用 `keepalive`（或直接让页面停留）刷新会话空闲 TTL，避免会话被后台 30 分钟空闲回收提前清掉。

### 会话生命周期与回收

- **会话存于 server 内存**，由 `QDRUST_BROWSER_URL` 配置的服务进程全局持有（每会话一个标签页）。进程重启即全部清空，向导需重新 `start`。
- **空闲回收**：会话闲置超过 **30 分钟**被后台任务回收；无论是否活跃，**最长存活 24 小时**；并发会话上限 **16**。
- 建议向导式流程结束后显式 `end` 释放标签页；被遗忘的会话会由上面的回收规则兜底，不会泄漏。

### 行为与限制

- **错误处理**：未配置 `QDRUST_BROWSER_URL`、连接失败、找不到元素或会话、参数缺失等都会返回 **502 信封**，可用 `success_asserts` / `failed_asserts` 感知，不会静默降级、也不会中断整个任务。
- **会话与一次性共享同一浏览器连接**：未带 `session` 的 `content` / `eval` / `screenshot` 也复用长连接（只是标签页用完即关），因此免去了每次冷启动的握手开销。
- **选择器找不到**：`type` / `click` 找不到目标元素会返回 502；`click` 的 `wait_selector` 最长轮询 15 秒。
- **无 `clear()`**：想清空输入框用 `type` 的 `clear=1`（内部 Ctrl+A + Backspace）。
- **同会话操作串行化**：同一会话的并发操作按顺序执行，避免两次按键交叠；跨会话可并行。

---

## 部署

### 环境要求

- Rust 1.85 或更高版本（项目使用 Rust 2024 edition）
- Node.js 24 和 npm（仅本地构建 WebUI 时需要；Docker 镜像已内置前端产物）
- Docker（推荐用于生产部署；本机通过 WSL Ubuntu 使用）

### 本地开发运行

安装前端依赖并构建 WebUI：

```powershell
npm --prefix webui ci --cache .npm-cache
npm --prefix webui run generate:api
npm --prefix webui run build
```

准备配置并启动服务：

```powershell
Copy-Item .env.example .env
cargo run -p qdrust-server
```

服务默认监听 `http://localhost:8923`。首次打开 WebUI 时创建初始管理员账号。运行数据默认写入 `data/qd.db`。

前后端分开开发时运行：

```powershell
cargo run -p qdrust-server
npm --prefix webui run dev
```

Vite 开发服务器位于 `http://localhost:5173`，并将 API 请求代理到 Rust 服务。

### Docker 部署（生产，推荐）

镜像已发布到 GitHub 容器注册表（GHCR），直接拉取运行即可，无需本地构建：

```powershell
docker pull ghcr.io/callmelins/qdrust:latest
```

最简启动（数据保存在命名卷 `qdrust-data`，监听 8923）：

```powershell
docker run -d --name qdrust -p 8923:8923 -v qdrust-data:/data ghcr.io/callmelins/qdrust:latest
```

生产推荐使用 Compose 管理（命名卷 + 健康检查 + 开机自启）：

```yaml
# docker-compose.yml
services:
  qdrust:
    image: ghcr.io/callmelins/qdrust:latest
    container_name: qdrust
    ports:
      - "8923:8923"
    volumes:
      - qdrust-data:/data
    environment:
      DATABASE_URL: sqlite:///data/qdrust.db
      COOKIE_SECURE: "true"        # 走 HTTPS 反向代理后必须开启
      QDRUST_BASE_URL: "https://your.domain"
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "--fail", "--silent", "http://127.0.0.1:8923/ready"]
      interval: 30s
      timeout: 5s
      retries: 3

volumes:
  qdrust-data:
```

```powershell
docker compose up -d
docker compose logs -f qdrust   # 查看日志
docker compose down             # 停止
```

> 本地从源码构建（开发者）：仓库自带 `compose.yaml`，执行 `docker compose up --build -d` 会构建本地 `qdrust:local` 镜像，适合贡献代码时自测。

### 反向代理到二级目录（sub-path）

站点默认伺服在根路径（适合**二级域名** `https://qd.your.domain` 或裸域名）。若想反代到**二级目录**（如 `https://your.domain/qd`，少一条 DNS 记录、且路径对中间人不可见）：

- **前端自适应（无需重建）**：WebUI 以**相对 base** 构建，API 前缀在**运行时**从当前页面 URL 推导（见 `webui/src/api.ts` 的 `detectUrlPrefix`）。因此**同一份镜像**既能在根路径用，也能在任意二级目录用——不需要为每个前缀重新构建前端。
- **后端设置前缀**：将 `QDRUST_BASE_PATH` 设为实际反代前缀，使后端在该目录下伺服静态资源与 SPA。`/health`、`/ready` 探针仍留在根路径，供健康检查。
- **反向代理原样转发**（**不要**剥离前缀），例如 nginx：

```nginx
location /qd/ {
    proxy_pass http://127.0.0.1:8923;   # 不剥离 /qd
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

配置示例（`https://your.domain/qd`）：

```
# 后端
QDRUST_BASE_PATH=/qd

# nginx：location /qd/ 原样转发到 8923（见上）
```

访问 `https://your.domain/qd/`（末尾斜杠）即可；浏览器按相对路径自动解析静态资源与 API 请求。

> **默认**：置空 `QDRUST_BASE_PATH` 即伺服在根路径（二级域名 / 裸域名场景不受影响）。
>
> **旧用法（可选，编译期固定）**：若偏好把前缀在构建时写死，可用 `VITE_BASE_PATH=/qd npm run build`（Docker 用 `--build-arg VITE_BASE_PATH=/qd`）得到绝对 base 的 UI，仍受支持。
>
> **不支持的形态**：剥离前缀式反代（`proxy_pass .../;` + URL 改写）与本运行时方案不兼容——请用"原样转发"。

### 环境变量

复制 `.env.example` 为 `.env` 后按需修改。关键变量：

| 变量 | 默认值 | 说明 |
|---|---|---|
| `BIND` / `PORT` | `0.0.0.0` / `8923` | 监听地址与端口 |
| `DATABASE_URL` | `sqlite://data/qd.db` | 数据库；MySQL 用 `mysql://user:pass@host:3306/qdrust` |
| `DATABASE_MIN_CONNECTIONS` / `DATABASE_MAX_CONNECTIONS` | `1` / `8` | 连接池上下限 |
| `SCHEDULER_INTERVAL_SECONDS` | `15` | 调度器轮询间隔 |
| `REQUEST_TIMEOUT_SECONDS` | `30` | 单步 HTTP 请求超时 |
| `SESSION_TTL_SECONDS` | `604800` | 会话有效期（7 天） |
| `COOKIE_SECURE` | `false` | **HTTPS 下必须设为 `true`** |
| `LOGIN_RATE_LIMIT_ATTEMPTS` / `_WINDOW_SECONDS` | `5` / `60` | 登录限流 |
| `LOG_RETENTION_DAYS` | `0` | 已完成运行记录保留天数（`0` = 永久） |
| `QDRUST_DEFAULT_TIMEZONE` | `Asia/Shanghai` | 未显式设时区的任务按其 cron 调度的 IANA 时区（DST 感知）。欧美部署可设如 `America/New_York` |
| `QDRUST_BASE_PATH` | 空 | 反代到二级目录时设为该前缀（如 `/qd`，伺服 `https://host/qd`）。WebUI 以相对 base 构建、运行时自适应，**无需**为每个前缀重建。置空=根路径。可选编译期固定见 `VITE_BASE_PATH` |
| `QDRUST_SMTP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` / `_FROM` | 空 | 邮件发送（重置密码 / 邮箱验证 / Email 渠道） |
| `QDRUST_BASE_URL` | `http://localhost:8923` | 密码重置 / 邮箱验证邮件中的链接基址 |
| `REQUIRE_EMAIL_VERIFICATION` | `false` | 新用户登录前必须验证邮箱 |
| `GA_KEY` | 空 | 注入 WebUI 的 Google Analytics 密钥 |
| `REDIS_URL` | 空 | 可选 Redis 会话缓存 |
| `QDRUST_CONFIG_FILE` | 空 | 运行时可调配置的 JSON 文件路径（热更新站点设置） |
| `QDRUST_BROWSER_URL` | 空 | 浏览器插件端点，配置后启用 `api://browser/*`（进程内 chromiumoxide，CDP：`http://localhost:9222` / `ws://localhost:3000` / `wss://chrome.browserless.io?token=...`） |

### 数据库

- **SQLite**：默认，零配置，文件位于 `DATABASE_URL` 指向的路径。
- **MySQL**：设置 `DATABASE_URL=mysql://...` 即自动切换；迁移脚本位于 `migrations-mysql/`。
- 迁移为**前向 only（forward-only）**，升级前务必备份数据库。

---

## 使用

### 首次启动与管理员

1. 启动服务后访问 `http://localhost:8923/`（或你的域名）。
2. 首次打开会引导创建**初始管理员账号**；之后其他用户走开放注册流程。
3. 管理员可在「设置 / 管理员」中管理用户、站点设置、日志清理、备份与恢复。

### 导入 QD HAR 模板

- 在 WebUI 的模板页点击导入，选择从旧 QD 导出的 `*.har.json`。
- 导入时会校验并**保留原始 HAR JSON**，兼容样本见 `tests/fixtures`。
- 也可用命令行校验：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

### 创建与运行任务

1. 新建模板或导入 HAR 后，基于模板创建任务并填写变量。
2. 配置 cron / 间隔调度，或手动「立即运行」。
3. 在运行记录中查看每一步的请求 / 响应 / 提取变量；支持取消正在执行的运行。
4. 订阅仓库的任务会按仓库自动导入并同步；公共模板经 PushRequest 审批后发布。

### 可视化 HAR 编辑器

WebUI 提供可视化编辑器，可直接增删改请求、设置请求头 / 表单 / 鉴权、编写 `success_asserts` 与 `extract_variables`，无需手改 JSON。

### 调度与随机延迟

可视化调度器支持固定间隔与 cron；「随机延迟」可让同一任务的多次执行在时间上打散，避免被目标站点识别为固定节奏。模板变量支持在创建任务时**预填**默认值。

#### 时区与夏令时

cron 按每个任务各自的 IANA 时区（`timezone` 字段）求值，**DST 感知**：设 `America/New_York` 的任务，`0 9 * * *` 会全年在当地 09:00 触发，无需为冬令/夏令手改 cron。任务未设 `timezone` 时使用服务端 `QDRUST_DEFAULT_TIMEZONE`（默认 `Asia/Shanghai`）。WebUI 中任务列表与运行历史的「上次运行/开始时间」也按该任务时区展示（未设时区则用查看者浏览器本地时区）。

### 运行日志与历史

每次运行会记录 QD 风格的文本日志；在任务详情页可查看该任务的运行历史，并支持**清空**历史记录。活跃运行的步骤通过 WebSocket 实时推送。

### 命令行（CLI）

校验旧 QD HAR：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

执行 HAR，并传入变量和整体超时：

```powershell
cargo run -p qdrust-cli -- run .\template.har.json --var token=abc --timeout 60
```

CLI 默认拒绝私网、localhost 和无效 TLS 证书，不继承旧 QD 的宽松网络策略。

---

## 推送 / 通知

任务成功或失败时，可通过通知渠道推送提醒。系统共支持 **10 种**渠道，在「通知」页新建渠道并绑定到任务即可。

| 渠道 | 所需配置 | 说明 |
|---|---|---|
| **Webhook** | `url` | 向该 URL `POST` JSON（含 `event` / `task_name` / `run_id` / `http_status` / `error`） |
| **Email** | `to` | 经 SMTP 发送，需先配置 `QDRUST_SMTP_*` 环境变量 |
| **Bark** | `url` | iOS 推送，设备地址形如 `https://api.day.app/你的Key` |
| **Server 酱** | `sendkey` | 微信推送 SendKey |
| **Telegram** | `token`, `chat_id`, `host`(可选) | 机器人 token 与 chat id；`host` 可指定自建 API 域名 |
| **钉钉机器人** | `access_token` | 自定义机器人 access_token |
| **WxPusher** | `app_token`, `uid` | 应用 token 与目标 UID |
| **WxPusher SPT** | `spt` | 主题推送码（逗号分隔可多个） |
| **企业微信应用** | `corpid`, `secret`, `agentid`, `to_user`(默认 `@all`), `proxy`(可选) | 自建应用消息；`proxy` 可指定 API 代理 / 自定义域名 |
| **企业微信群机器人** | `key` | 群机器人 webhook key |

通知标题与正文由调度器统一渲染，并在 Email 与推送渠道间共享；单个渠道投递失败只记录日志，不会中断任务执行。

---

## 更新

qdrust 使用不可变语义版本镜像标签。升级步骤：

1. **停止写入并备份数据库**（迁移前向 only，回滚必须连同数据库一起恢复）：

```powershell
pwsh -File scripts/backup-db.ps1 -Database data/qdrust.db -Output backups/qdrust-$(Get-Date -Format yyyyMMdd-HHmmss).db
```

2. 记录当前镜像 digest，拉取目标版本并以相同的数据卷启动：

```powershell
docker compose pull && docker compose up -d
# 或锁定到具体版本：docker pull ghcr.io/callmelins/qdrust:v0.1.0
```

3. 验证 `/health`、`/ready`、登录与至少一个只读流程。

若健康检查失败：停新容器 → 恢复升级前数据库备份 → 启动记录的上一镜像 digest。详见 [运维文档](docs/phase8/OPERATIONS.md) 与 [发布检查清单](docs/phase8/RELEASE_CHECKLIST.md)。

---

## 常见问题（FAQ）

**Q：HTTPS 部署后登录会话不生效 / 一直被登出？**
A：生产环境必须把 `COOKIE_SECURE` 设为 `true`，否则安全 Cookie 不会被浏览器接受。

**Q：邮件相关功能（重置密码、邮箱验证、Email 渠道）用不了？**
A：需要先配置 `QDRUST_SMTP_*` 系列环境变量并确认 `QDRUST_BASE_URL` 指向对外可访问的地址。若启用了 `REQUIRE_EMAIL_VERIFICATION`，新用户必须验证邮箱才能登录。

**Q：数据库升级后还能回退到旧版本吗？**
A：迁移是**前向 only** 的。仅回退镜像而不恢复数据库不被支持；回退请同时用备份恢复数据库，再启动旧镜像 digest。

**Q：CLI 报网络被拒绝（私网 / localhost / 证书）？**
A：这是刻意的安全策略——CLI 默认拒绝私网、localhost 和无效 TLS 证书，不像旧 QD 那样宽松。服务端执行不受此限制。

**Q：调度好像没按我设的时间跑？**
A：`SCHEDULER_INTERVAL_SECONDS`（默认 15 秒）是调度器轮询间隔，任务触发精度受其影响；如需更精确请相应调小（会增加数据库轮询压力）。

**Q：运行记录越来越多，能自动清理吗？**
A：设置 `LOG_RETENTION_DAYS`（默认 `0` 永久保留），系统每小时清理超出天数的已完成运行。

**Q：多实例 / 横向扩展怎么弄？**
A：单实例基于 SQLite 事务 + 租约保证不重复执行；多实例共享数据库时同样由租约机制保证。推荐 Docker 副本 + 反向代理（`docker compose up --scale qdrust=N`）。Redis 仅用于会话缓存加速，非必需。

**Q：插件 / `api://util` 工具不全？**
A：`api://util/*` 已对齐 QD 的核心工具（时间 / 编码 / 哈希 / 正则 / JSON / RSA / GB2312 / 字符串替换 / OCR）， toolbox 与 notepad 属 Web 工具箱页面，不在模板 API 范围内，未移植。

**Q：站点要 JS 生成签名 / 过验证码，纯 HAR 跑不了怎么办？**
A：用 `api://browser/*` 无头浏览器插件。配置 `QDRUST_BROWSER_URL` 后启用，模板里插入 `api://browser/eval`（执行 JS 取 token/签名）、`api://browser/content`（抓渲染后 HTML）、或 `api://browser/screenshot`（截图），再用 `extract_variables` 把结果提成变量回填。详见 [浏览器插件（无头浏览器签到）](#浏览器插件无头浏览器签到)。

**Q：浏览器插件能自动跑多步页面交互（点按钮、填表单）吗？**
A：可以。用 `start` 开一个会话，对同一 `session` 跨步骤执行 `type`（输入）/ `click`（点击）/ `content`（取渲染后状态），`extract_variables` 提出会话 id 后用 `{{var}}` 复用于后续步骤；流程结束用 `end` 关会话。会话在 server 内存中跨调用存活（空闲 30 分钟 / 最长 24 小时回收），所以中途可以停下等人——比如让人工过完验证码再继续。会话里的 `#` 等选择器要 URL 编码（`#id` → `%23id`）。

---

## 架构决策说明

与旧 QD 的两处刻意差异：

- **单进程异步模型**：qdrust 采用单进程 Tokio 异步模型（`axum::serve`），而非 Tornado 多进程。
  横向扩展通过 Docker 副本 + 反向代理（`docker compose up --scale qdrust=N`）实现；
  开发期热重载使用 `cargo watch -x run`，运行时配置热更新由 `QDRUST_CONFIG_FILE` 与
  站点设置（admin API）提供，无需重启进程。
- **非持久化本地队列**：运行队列基于 SQLite 事务（`claim_run`/租约恢复），保证单实例
  正确性；多实例共享数据库时由租约机制保证不重复执行。Redis 可选用于会话缓存加速。

---

## 兼容范围

旧 QD HAR 是核心兼容契约，兼容样本位于 `tests/fixtures`。WebUI 可导入、校验并保留原始 HAR JSON。

qdrust 不导入旧 QD 数据库，不复用旧登录 Cookie，也不承诺兼容旧 URL、旧 API 或任意 Python 动态表达式。详细决策、阶段范围和风险控制见 [迁移计划](MIGRATION_PLAN.md)。

---

## 项目状态

Phase 0-8 代码与容器已实现完成（Phase 8 状态为 Implementation Complete）；发布前仍待完成浏览器人工验收与首个镜像（amd64 / arm64）发布检查。详见 [发布检查清单](docs/phase8/RELEASE_CHECKLIST.md)。

---

## 验证

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
npm --prefix webui run lint
npm --prefix webui run test
npm --prefix webui run build
```

完整发布验收项目见 [发布检查清单](docs/phase8/RELEASE_CHECKLIST.md)。

---

## 许可与致谢

### 许可

[MIT 许可证](LICENSE)。Copyright (c) 2026 CallmeLins。

### 致谢

- 本项目在设计与协议层面参考了 [QD（qd-today/qd）](https://github.com/qd-today/qd)——一个优秀的 HTTP 定时任务自动执行框架。qdrust 是其**独立、干净重写的 Rust 实现**：不继承其代码、不导入其数据库、不复用其 Cookie，仅对齐核心 HAR 兼容契约与 `api://util` 工具语义。
- 感谢 QD 社区与所有贡献者提供的思路与协议参考。
