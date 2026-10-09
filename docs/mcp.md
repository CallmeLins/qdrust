# MCP server（`qdrust-mcp`）

> 返回 [README](../README.md)

`qdrust-mcp` 是一个 **stdio MCP server**，把一个 qdrust 实例的 REST API 暴露给 MCP 客户端（Claude Desktop、Cursor、Deepseek Harness 等），这样模型可以直接建任务、跑任务、看运行结果，不用切到 WebUI。它是**独立二进制**，不属于服务端镜像：由 MCP 客户端拉起，并指向你的 qdrust 地址。

## 先建一个 API 令牌

MCP 用**个人访问令牌**认证。在 WebUI **设置 → API 令牌**新建一个，明文（`qd_…`）**只显示这一次**，服务端只存它的哈希。

令牌等同于你账号的完整 API 权限，请当作密码保管；不用了就在同一页撤销，下一次请求即失效。请求以 `Authorization: Bearer qd_…` 发出，因此跳过 CSRF（没有 cookie 可供跨站利用）。

详见 [账号与第三方登录 · 个人访问令牌](authentication.md#个人访问令牌api-token)。

## 构建

```bash
cargo build -p qdrust-mcp --release
# 产物：target/release/qdrust-mcp（Windows 为 qdrust-mcp.exe）
# 或者装进 PATH：
cargo install --path crates/qdrust-mcp
```

## 客户端配置

```json
{
  "mcpServers": {
    "qdrust": {
      "command": "qdrust-mcp",
      "env": {
        "QDRUST_URL": "https://qd.example.com",
        "QDRUST_TOKEN": "qd_..."
      }
    }
  }
}
```

- `QDRUST_URL`：qdrust 基址，默认 `http://localhost:8923`。反代到二级目录时要带上前缀，如 `https://example.com/qd`。
- `QDRUST_TOKEN`：必填。缺失时进程直接以明确错误退出。

## 工具一览

| 工具 | 对应接口 | 说明 |
|---|---|---|
| `list_tasks` / `get_task` | `GET /api/v1/tasks[/{id}]` | 任务列表（可按分组）/ 单个任务 |
| `create_task` | `POST /api/v1/tasks` | 建任务；绑定模板时请求由模板决定 |
| `update_task` | `PUT /api/v1/tasks/{id}` | **只改传了的字段**，省略即保持原值 |
| `delete_task` | `DELETE /api/v1/tasks/{id}` | 删任务及其运行记录 |
| `run_task` | `POST /api/v1/tasks/{id}/run` | 立即运行，返回新的 run |
| `cancel_run` | `POST /api/v1/runs/{id}/cancel` | 取消执行中的 run |
| `list_runs` | `GET /api/v1/runs` | 聚合运行日志，可按状态 / 任务过滤 |
| `list_task_runs` | `GET /api/v1/tasks/{id}/runs` | 某任务的运行历史 |
| `get_run_steps` | `GET /api/v1/runs/{id}/steps` | 单次运行的逐步结果 |
| `batch_tasks` | `POST /api/v1/tasks/batch` | 批量 `enable` / `disable` / `delete` / `run` |
| `list_task_groups` | `GET /api/v1/task-groups` | 分组名 |
| `list_templates` / `get_template` | `GET /api/v1/templates[/{id}]` | 模板列表（可搜索）/ 单个模板（含变量与默认值） |
| `test_template` | `POST /api/v1/templates/{id}/test` | 用给定变量试跑模板，不建任务、不落运行记录 |
| `import_template` | `POST /api/v1/templates/import-qd-har` | 导入 QD HAR |
| `list_notification_channels` | `GET /api/v1/notification-channels` | 通知渠道 |
| `bind_notification` | `POST /api/v1/notification-actions/batch` | 把渠道绑到一个或多个任务（默认 `failure`） |

## 限制与注意

- **令牌目前没有 scope**：任何令牌都拥有其属主的全部权限。按需细分 read / write 是后续可加项。
- 出站请求和 WebUI 走同一道闸门（SSRF、请求数 / 循环 / 超时上限）；`test_template` 与真实运行共用同一套策略。
- **工具名是客户端契约**：改名会让已保存的客户端配置失效，所以有一条测试把工具集合钉住。
