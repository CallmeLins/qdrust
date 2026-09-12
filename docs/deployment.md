# 部署

> 返回 [README](../README.md)

## 环境要求

- Rust 1.85 或更高版本（项目使用 Rust 2024 edition）
- Node.js 24 和 npm（仅本地构建 WebUI 时需要；Docker 镜像已内置前端产物）
- Docker（推荐用于生产部署；本机通过 WSL Ubuntu 使用）

## 本地开发运行

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

## Docker 部署（生产，推荐）

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

## 反向代理到二级目录（sub-path）

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

## 环境变量

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

第三方登录（OIDC / 反向代理 Header）另有一组变量，见 [账号与第三方登录](authentication.md)。

## 数据库

- **SQLite**：默认，零配置，文件位于 `DATABASE_URL` 指向的路径。
- **MySQL**：设置 `DATABASE_URL=mysql://...` 即自动切换；迁移脚本位于 `migrations-mysql/`。
- 迁移为**前向 only（forward-only）**，升级前务必备份数据库。

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

若健康检查失败：停新容器 → 恢复升级前数据库备份 → 启动记录的上一镜像 digest。详见 [运维手册](operations.md) 与 [发布检查清单](release-checklist.md)。
