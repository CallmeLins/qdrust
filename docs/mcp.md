# MCP server（`qdrust-mcp`）

> 返回 [README](../README.md)

`qdrust-mcp` 是一个 **stdio MCP server**，把一个 qdrust 实例的 REST API 暴露给 MCP 客户端（Claude Desktop、Cursor、Cline、Deepseek Harness 等），让模型直接建任务、跑任务、看运行结果、读写记事本槽位，不用切到 WebUI。

它是**纯 Python、不需要编译**的：本质只是一个带 Bearer 认证的 HTTP 客户端，每个工具对应一次 qdrust REST 调用。代码在 [`mcp/`](../mcp/)，用 [`uv`](https://docs.astral.sh/uv/) 运行——uv 会自己下载 Python，你不需要单独装 Python。

## 前置：装 uv

MCP 客户端会通过 `uvx` 拉起它，所以需要 uv：

- **macOS / Linux**：`curl -LsSf https://astral.sh/uv/install.sh | sh`
- **Windows（PowerShell）**：`powershell -ExecutionPolicy ByPass -c "irm https://astral.sh/uv/install.ps1 | iex"`

装完重开终端，`uv --version` 有输出即可。

## 三步用起来

1. **建 API 令牌**：WebUI → **设置 → API 令牌** → 新建，复制 `qd_…`（只显示一次）。它等同于你账号的完整 API 权限，请当作密码。
2. **配置 MCP 客户端**（见下），把 `args` 里的路径换成你 clone 下来的 `qdrust/mcp` 的绝对路径。
3. **重启客户端**，让它重新读配置。

## 客户端配置

**方式 A：不安装，直接跑（推荐）**

```json
{
  "mcpServers": {
    "qdrust": {
      "command": "uvx",
      "args": ["--from", "/absolute/path/to/qdrust/mcp", "qdrust-mcp"],
      "env": {
        "QDRUST_URL": "https://qd.example.com",
        "QDRUST_TOKEN": "qd_..."
      }
    }
  }
}
```

- Windows 路径要写成双反斜杠：`"C:\\Users\\you\\qdrust\\mcp"`。
- `QDRUST_URL` 默认 `http://localhost:8923`；反代到二级目录要带前缀，如 `https://example.com/qd`（漏掉前缀的症状见[自检与排障](#自检与排障)）。
- 可选 `QDRUST_TIMEOUT`：单次请求超时秒数，默认 `120`。模板步骤多、或服务端自身的请求超时配得比它长时，要调大。
- 这套配置里藏着令牌，**别提交进仓库、别贴到聊天里**（用下面的 AI 提示词时也一样，用完顺手删掉那条消息）。

**方式 B：先装到 PATH，配置更干净**

```bash
uv tool install --from /absolute/path/to/qdrust/mcp qdrust-mcp
```

```json
{
  "mcpServers": {
    "qdrust": {
      "command": "qdrust-mcp",
      "env": { "QDRUST_URL": "https://qd.example.com", "QDRUST_TOKEN": "qd_..." }
    }
  }
}
```

> 别用 `uvx --from git+https://…#subdirectory=mcp`：uv 对 git 子目录的支持还不稳（uv issues #12713 / #16303），本地路径或用本地路径 `uv tool install` 才是可靠路径。

常见客户端配置文件位置：

| 客户端 | 配置文件 |
|---|---|
| Claude Desktop（macOS） | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Claude Desktop（Windows） | `%APPDATA%\Claude\claude_desktop_config.json` |
| Cursor | `~/.cursor/mcp.json`（也可放到项目内 `.cursor/mcp.json`） |
| Cline / Roo（VS Code） | 扩展设置里的 MCP Servers |
| 其他 | 任何支持 stdio（`command` / `args` / `env`）的客户端都行 |

## 让 AI 帮你配（推荐）

下面这段是**自包含**的，复制给你的 AI 助手（Claude Code / Cursor / Codex 这类有终端和文件权限的），它会帮你装 uv、定位配置文件、写配置并验证：

```text
请帮我在本机把 qdrust 的 MCP server 配好，让我能在 MCP 客户端里直接操作 qdrust。
按下面的步骤做；需要我提供的信息就问我，不要猜、不要编。

【目标环境】
- qdrust 地址：<例如 https://qd.example.com 或 http://localhost:8923>
- qdrust 仓库位置（我 clone 下来的，里面有 mcp/ 目录）：<绝对路径>
- 要配置的客户端：<Claude Desktop / Cursor / Cline / 其他>

【步骤】
1. 判断操作系统和 shell，检查 `uv --version`。
   没有就安装：macOS/Linux 用 `curl -LsSf https://astral.sh/uv/install.sh | sh`；
   Windows PowerShell 用 `irm https://astral.sh/uv/install.ps1 | iex`。
   装完确认 `uv` 在新终端里可用（必要时刷新 PATH）。
2. 我会去 qdrust WebUI 的「设置 → API 令牌」新建一个令牌，然后把 `qd_...` 给你。
   你只把它写进本地客户端配置：不要 echo、不要写进仓库里的文件、不要贴进任何日志或提交。
3. 定位客户端配置文件并**先备份**，然后在 `mcpServers` 下新增一项，保留已有内容、保持 JSON 合法：
   "qdrust": {
     "command": "uvx",
     "args": ["--from", "<仓库绝对路径>/mcp", "qdrust-mcp"],
     "env": { "QDRUST_URL": "<地址>", "QDRUST_TOKEN": "<令牌>" }
   }
   （Windows 路径用双反斜杠转义。）
4. 验证：
   a. 直接跑一次 `uvx --from <仓库绝对路径>/mcp qdrust-mcp`（把 stdin 关掉即可）：
      能起来就说明依赖没问题；缺 token 时只会打印一行 "QDRUST_TOKEN is required" 并以码 2 退出。
   b. 用令牌验证 REST：`curl -H "Authorization: Bearer <令牌>" <地址>/api/v1/tasks`
      期望 200 且返回 `[]` 或任务数组；如果回来的是 HTML，说明这个地址不是 API（多半漏了反代前缀），
      先改对再继续。
   c. 如果客户端可脚本化，确认能列出 23 个工具（list_tasks、create_task、run_task …）。
5. 告诉我需要重启哪个客户端，并提醒我把刚才那条带令牌的消息删掉。

【约束】
- 不要修改 qdrust 服务端本身，不要碰它的数据库。
- 不要把令牌写进任何会被 git 跟踪的文件。
- 改客户端配置前先备份；改完用 JSON 解析器校验一遍。
- 如果 `uvx` 不在客户端进程的 PATH 里（GUI 应用继承的 PATH 常和终端不同）：
  改用 `uv tool install --from <仓库绝对路径>/mcp qdrust-mcp`，
  并把配置里的 command 换成 `qdrust-mcp` 的绝对路径。
```

## 工具一览

| 工具 | 对应接口 | 说明 |
|---|---|---|
| `list_tasks` / `get_task` | `GET /api/v1/tasks[/{id}]` | 任务列表（`grp` 按分组过滤）/ 单个任务 |
| `create_task` | `POST /api/v1/tasks` | 建任务；绑定 `template_id` 时请求由模板决定，否则传 `url`（+ `method`/`headers`/`body`） |
| `update_task` | `PUT /api/v1/tasks/{id}` | **只改传了的字段**，省略即保持原值；`clear=[…]` 显式清空某个字段（如 `clear=["grp"]`） |
| `delete_task` | `DELETE /api/v1/tasks/{id}` | 删任务及其运行记录 |
| `run_task` | `POST /api/v1/tasks/{id}/run` | 立即运行，返回新的 run |
| `cancel_run` | `POST /api/v1/runs/{id}/cancel` | 取消执行中的 run |
| `delete_run` | `DELETE /api/v1/runs/{id}` | 删一条运行记录及其步骤 |
| `list_runs` | `GET /api/v1/runs` | 聚合运行日志，可按 `status` / `task_id` / `limit` 过滤，`before_id` 往回翻页 |
| `list_task_runs` | `GET /api/v1/tasks/{id}/runs` | 某任务的运行历史 |
| `get_run_steps` | `GET /api/v1/runs/{id}/steps` | 单次运行的逐步结果 |
| `batch_tasks` | `POST /api/v1/tasks/batch` | 批量 `enable` / `disable` / `delete` / `run` |
| `list_task_groups` | `GET /api/v1/task-groups` | 分组名 |
| `list_templates` / `get_template` | `GET /api/v1/templates[/{id}]` | 模板列表（可按 `q` / `grp` 搜索，`cursor` 翻页）/ 单个模板（含变量与默认值） |
| `test_template` | `POST /api/v1/templates/{id}/test` | 用给定变量试跑模板，**不建任务、不落运行记录** |
| `import_template` | `POST /api/v1/templates/import-qd-har` | 导入 QD HAR |
| `list_notepads` | `GET /api/v1/notepads` | 记事本槽位一览（编号、大小、预览） |
| `get_notepad` / `set_notepad` / `delete_notepad` | `/api/v1/notepads/{notepad_id}` 的 `GET` / `PUT` / `DELETE` | 读 / 建或覆盖 / 删一个槽位；写入上限 256 KiB |
| `list_notification_channels` | `GET /api/v1/notification-channels` | 通知渠道 |
| `bind_notification` | `POST /api/v1/notification-actions/batch` | 把渠道绑到一个或多个任务（默认 `failure`） |

## 自检与排障

- **单独跑一次**：`uvx --from <路径>/mcp qdrust-mcp`。缺 token 会打印 `qdrust-mcp: QDRUST_TOKEN is required …` 并以码 2 退出；这是配置问题，不是依赖问题。
- **验证令牌**：`curl -H "Authorization: Bearer $QDRUST_TOKEN" https://你的域名/api/v1/tasks` → 200。返回 401 说明令牌被撤销或过期。
- **客户端里看不到工具**：九成是客户端进程的 `PATH` 里没有 `uvx`（GUI 应用不继承终端 PATH）。用 `uv tool install` 装好后把 `command` 写成绝对路径即可。
- **工具报「… is not JSON」或一条 3xx**：`QDRUST_URL` 没指到 API。反代到二级目录漏了前缀时，请求会落到 WebUI 的页面（`index.html`，200 + `text/html`）或一次重定向，而不是 JSON。
- **工具报超时**：一次请求超过 `QDRUST_TIMEOUT`（默认 120 秒）就放弃。模板步骤多或服务端请求超时更长时，把它调大。
- **工具调用报错**：错误会以工具错误返回并带上 qdrust 自己的 message（例如 `qdrust returned 422: task URL is required when no template is bound`），照它改参数即可。
- **出站限制**：MCP 只是转发到 qdrust，所有出站请求仍走 qdrust 的同一道闸门（SSRF、请求数/循环/超时上限），`test_template` 与真实运行同策略。

## 限制

- **令牌目前没有 scope**：任何令牌都拥有其属主的全部权限。要细分 read / write 需要给服务端所有写接口加统一关口，属于后续可加项。
- 工具名是**客户端契约**：改名会让已保存的客户端配置失效，所以有一条测试把工具集合钉住（`mcp/tests/test_server.py`）。
- **依赖不锁版本**：仓库不提交 `mcp/uv.lock`，所以每次 `uvx` 拉到的可能是更新的 MCP SDK；能变的行为由测试钉住（工具集合、上报的版本号）。CI 会用**最新版**与 `pyproject.toml` 里的**下界**（`mcp>=1.22`，更老的版本在现代 pydantic 下装不起来）各跑一遍。
