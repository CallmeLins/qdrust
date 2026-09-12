# 浏览器插件（无头浏览器签到）

> 返回 [README](../README.md)

qdrust 的签到任务**以 HAR 为主流程**（HTTP 请求，fetcher 直接渲染执行），无头浏览器只用来补上纯 HTTP 做不了的那一步。因此推荐**混合用法**：只有真正需要浏览器的一步走 `api://browser/*`，其余步骤仍走普通 HTTP 请求。这是刻意的最小设计——浏览器只在"生成签名 / 过验证码 / 渲染 JS / 走多步表单交互"这类场景介入。

配置 `QDRUST_BROWSER_URL` 后插件自动启用（无需在插件管理页新建条目），模板里直接写 `api://browser/<action>` 即可。

> **架构**：chromiumoxide（CDP 客户端）在 qdrust server 进程内直接持有，进程启动时创建一次、全局共享。不再像早期版本那样每次调用拉起一个独立子进程二进制。这样 DOM 状态可以在多次独立调用间存活，是实现"多步页面交互 / 人工过验证码后继续"的关键。进程重启即清空会话（向导需重新 `start`）。

## Action 一览

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

## 部署启用（三步）

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

## 混合签到：模板怎么写

核心思路：**普通 HTTP 步骤用 HAR 原样请求；需要浏览器的一步插入 `api://browser/*`，把它的返回体用 `extract_variables` 提取成变量，供后续 HAR 步骤用 `{{var}}` 填充。**

### 一次性用法

#### 例 1：`eval` 取签名 / token，回填到后续 HTTP 请求

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

#### 例 2：`content` 抓渲染后的页面，正则提取变量

SPA 页面内容由 JS 动态生成，普通 HTTP 拿到的是空壳。用 `content` 拿渲染后的 HTML，再提取：

```jsonc
{
  "request": { "method": "GET", "url": "api://browser/content?url=https://example.com/dashboard" },
  "extract_variables": [
    { "name": "nickname", "re": "nickname[^>]*>([^<]+)<", "from": "content" }
  ]
}
```

#### 例 3：`screenshot` 过验证码 / 留档

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

### 会话用法：多步页面交互 / 人工过验证码

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

## 会话生命周期与回收

- **会话存于 server 内存**，由 `QDRUST_BROWSER_URL` 配置的服务进程全局持有（每会话一个标签页）。进程重启即全部清空，向导需重新 `start`。
- **空闲回收**：会话闲置超过 **30 分钟**被后台任务回收；无论是否活跃，**最长存活 24 小时**；并发会话上限 **16**。
- 建议向导式流程结束后显式 `end` 释放标签页；被遗忘的会话会由上面的回收规则兜底，不会泄漏。

## 行为与限制

- **错误处理**：未配置 `QDRUST_BROWSER_URL`、连接失败、找不到元素或会话、参数缺失等都会返回 **502 信封**，可用 `success_asserts` / `failed_asserts` 感知，不会静默降级、也不会中断整个任务。
- **会话与一次性共享同一浏览器连接**：未带 `session` 的 `content` / `eval` / `screenshot` 也复用长连接（只是标签页用完即关），因此免去了每次冷启动的握手开销。
- **选择器找不到**：`type` / `click` 找不到目标元素会返回 502；`click` 的 `wait_selector` 最长轮询 15 秒。
- **无 `clear()`**：想清空输入框用 `type` 的 `clear=1`（内部 Ctrl+A + Backspace）。
- **同会话操作串行化**：同一会话的并发操作按顺序执行，避免两次按键交叠；跨会话可并行。
