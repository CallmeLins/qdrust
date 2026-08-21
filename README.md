# qdrust

qdrust 是按 QD2 路线重新设计的 Rust 项目，提供旧 QD HAR 的解析与执行、命令行工具、REST API、定时调度和 Vue WebUI。它是一个新项目，不要求复刻旧 QD 数据库。

## 组成

- `crates/qdrust-core`：旧 QD HAR 解析、编译、变量求值和 HTTP 执行核心。
- `crates/qdrust-cli`：无需启动服务即可校验和执行 HAR。
- `crates/qdrust-server`：基于 Axum、SQLx 和 SQLite 的 API、认证、调度与运行管理。
- `webui`：基于 Vue 3、TypeScript 和 Vite 的 WebUI。

当前服务包含用户认证与会话（开放注册、忘记/重置密码、邮箱验证、CSRF 轮换）、管理员 API（用户管理、站点设置、日志清理、备份/恢复）、模板（搜索/分组/分页、公共发布审批 PushRequest、订阅仓库自动导入）、任务（分组/批量操作）、运行记录与步骤、WebSocket 实时步骤流（运行 + 订阅进度）、租约恢复、插件、通知（Webhook + Email）、记事本、可选 Redis 会话缓存、GA 注入、运行时配置热更新以及 OpenAPI。支持 SQLite 与 MySQL 双后端（按 DATABASE_URL 自动选择）。所有用户资源均在服务端执行归属校验。

## 环境要求

- Rust 1.85 或更高版本（项目使用 Rust 2024 edition）
- Node.js 24 和 npm
- Docker（可选；本机通过 WSL Ubuntu 使用）

## 本地运行

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

常用端点：

- WebUI：`http://localhost:8923/`
- 健康检查：`GET /health`
- 数据库就绪检查：`GET /ready`
- OpenAPI：`GET /api/v1/openapi.json`
- 运行步骤实时流：`GET /api/v1/runs/{id}/steps/live`（WebSocket）
- 订阅同步进度：`GET /api/v1/subscriptions/{id}/sync/live`（WebSocket）
- 管理员用户管理：`GET/PATCH /api/v1/admin/users`、`/api/v1/admin/users/{id}`
- 管理员站点设置：`GET/PUT /api/v1/admin/settings/{key}`
- 管理员备份/恢复：`GET /api/v1/admin/backup`、`POST /api/v1/admin/restore`
- 模板发布审批：`POST /api/v1/push-requests`、`POST /api/v1/admin/push-requests/{id}/decision`
- 任务批量操作：`POST /api/v1/tasks/batch`（enable/disable/delete/run）
- 任务分组：`GET /api/v1/task-groups`
- CSRF 轮换：`POST /api/v1/auth/csrf/rotate`
- 邮箱验证：`POST /api/v1/auth/verify-email`

前后端分开开发时运行：

```powershell
cargo run -p qdrust-server
npm --prefix webui run dev
```

Vite 开发服务器位于 `http://localhost:5173`，并将 API 请求代理到 Rust 服务。

## CLI

校验旧 QD HAR：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

执行 HAR，并传入变量和整体超时：

```powershell
cargo run -p qdrust-cli -- run .\template.har.json --var token=abc --timeout 60
```

CLI 默认拒绝私网、localhost 和无效 TLS 证书，不继承旧 QD 的宽松网络策略。

## Docker

使用 Compose 构建并启动：

```powershell
wsl -d Ubuntu -- bash -lc "cd /mnt/c/UserData/WorkSpace/Learn/qdrust && docker compose up --build -d"
```

查看日志或停止服务：

```powershell
wsl -d Ubuntu -- bash -lc "cd /mnt/c/UserData/WorkSpace/Learn/qdrust && docker compose logs -f qdrust"
wsl -d Ubuntu -- bash -lc "cd /mnt/c/UserData/WorkSpace/Learn/qdrust && docker compose down"
```

Compose 使用 `qdrust-data` 命名卷保存 SQLite 数据，容器以 UID/GID `10001` 非 root 用户运行。生产 HTTPS 部署必须设置 `COOKIE_SECURE=true`，部署和备份说明见 [运维文档](docs/phase8/OPERATIONS.md)。

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

## 兼容范围

旧 QD HAR 是核心兼容契约，兼容样本位于 `tests/fixtures`。WebUI 可导入、校验并保留原始 HAR JSON。

qdrust 不导入旧 QD 数据库，不复用旧登录 Cookie，也不承诺兼容旧 URL、旧 API 或任意 Python 动态表达式。详细决策、阶段范围和风险控制见 [迁移计划](MIGRATION_PLAN.md)。

## 项目状态

Phase 0-7 已完成；Phase 8 的代码、容器和发布流水线已实现，仍需在正式发布前完成浏览器人工验收和首个镜像发布检查。
