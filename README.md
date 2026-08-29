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
| `QDRUST_SMTP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` / `_FROM` | 空 | 邮件发送（重置密码 / 邮箱验证 / Email 渠道） |
| `QDRUST_BASE_URL` | `http://localhost:8923` | 密码重置 / 邮箱验证邮件中的链接基址 |
| `REQUIRE_EMAIL_VERIFICATION` | `false` | 新用户登录前必须验证邮箱 |
| `GA_KEY` | 空 | 注入 WebUI 的 Google Analytics 密钥 |
| `REDIS_URL` | 空 | 可选 Redis 会话缓存 |
| `QDRUST_CONFIG_FILE` | 空 | 运行时可调配置的 JSON 文件路径（热更新站点设置） |

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
