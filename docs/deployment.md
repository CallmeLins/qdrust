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

### 数据目录的属主

镜像以 uid **10001**（用户 `qdrust`）运行。SQLite 的 WAL 模式要同时写主库文件和 `-wal`、`-shm` 两个附属文件，所以这三个文件都得对 10001 可写。

容器启动时先以 root 检查一次：`/data` 的属主不是 10001 就把整个目录改过来，然后立刻用 `setpriv` 降回 10001 再启动服务——**服务进程本身任何时候都不是 root**，root 只存在于启动脚本执行的那一小段里。因此把 `/data` 从一个容器拷出来、挂进另一个容器，可以直接用，不必在宿主上 `chown`。

不想要 root 阶段的有两个出口：

| 做法 | 效果 |
|---|---|
| compose 里加 `user: "10001:10001"` | 跳过整个 root 阶段（`id -u` 不是 0，脚本直接 exec） |
| `QDRUST_CHOWN=0` | 仍以 root 启动，只跳过那次 chown |

顶层属主已经正确、只有个别文件不对时脚本不会递归（它只在目录本身不对时才加 `-R`）。这种情形手动修：

```bash
chown 10001:10001 /data/qdrust.db /data/qdrust.db-wal /data/qdrust.db-shm
```

**数据库不可写时服务不会带病启动。** 启动时会先探一次写权限，失败就直接退出，错误里写明是哪个文件、属主是谁、以及该跑哪条 `chown`。没有这道检查的话，SQLite 会照常打开只读库：`/ready` 返回 200、读接口全部正常，只有写入静默失败——注册会回一句「用户名已被占用或非法」，而真相是磁盘上一个字节都没落。

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
| `QDRUST_DEFAULT_LOCALE` | `zh-CN` | 首次访问者的 WebUI 语言，`zh-CN`（默认）或 `en-US`（也接受裸 `zh` / `en`）。**只是默认值**：页头语言切换器选过的语言会存在该浏览器里，已存的偏好永远优先于本变量，所以它不会覆盖老用户的语言 |
| `QDRUST_BASE_PATH` | 空 | 反代到二级目录时设为该前缀（如 `/qd`，伺服 `https://host/qd`）。WebUI 以相对 base 构建、运行时自适应，**无需**为每个前缀重建。置空=根路径。可选编译期固定见 `VITE_BASE_PATH` |
| `QDRUST_SMTP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` / `_FROM` | 空 | 邮件发送（重置密码 / 邮箱验证 / Email 渠道） |
| `QDRUST_BASE_URL` | `http://localhost:8923` | 密码重置 / 邮箱验证邮件中的链接基址 |
| `REQUIRE_EMAIL_VERIFICATION` | `false` | 新用户登录前必须验证邮箱 |
| `GA_KEY` | 空 | 注入 WebUI 的 Google Analytics 密钥 |
| `REDIS_URL` | 空 | 可选 Redis 会话缓存 |
| `QDRUST_METRICS` | 空 | 设为 `1` / `true` 时开放 `GET /metrics`（Prometheus 文本格式）且**不校验凭据**，适合只在内网或 sidecar 抓取的场景 |
| `QDRUST_METRICS_TOKEN` | 空 | 设置后自动启用 `/metrics`，并要求 `Authorization: Bearer <token>`。两者都不设时 `/metrics` 返回 404（等同于不存在），因为运行计数本身就是部署信息 |
| `QDRUST_CONFIG_FILE` | 空 | 运行时可调配置的 JSON 文件路径（热更新站点设置） |
| `QDRUST_BROWSER_URL` | 空 | 浏览器插件端点，配置后启用 `api://browser/*`（进程内 chromiumoxide，CDP：`http://localhost:9222` / `ws://localhost:3000` / `wss://chrome.browserless.io?token=...`） |
| `QDRUST_ALLOW_PRIVATE_NETWORK` | `false` | **高风险**：允许**全部出站请求**（模板运行、直接填 URL 的任务、通知渠道、订阅抓取、模板里的 `api://util/dddd` 转发）访问内网 / 回环 / 链路本地地址。仅在确实依赖内网服务（如自建 flaresolverr、内网 ntfy、内网 DdddOCR）时开启，理由、风控与升级影响见[出站请求访问内网地址](usage.md#出站请求访问内网地址高风险开关)。也可在管理页「站点设置」里运行时开关，优先级高于本变量 |
| `QDRUST_ALLOW_INVALID_CERTIFICATES` | `false` | **高风险**：不再校验证书（自签名 / 过期 / 域名不匹配都接受）。与上一个开关互相独立，理由见[接受无效证书](usage.md#接受无效证书高风险开关)。同样可在管理页运行时开关，优先级高于本变量 |

第三方登录（OIDC / 反向代理 Header）另有一组变量，见 [账号与第三方登录](authentication.md)。

## 监控指标

`GET /metrics` 输出 Prometheus 文本格式，数据每次抓取时用几条聚合查询现算，不驻留内存，因此进程重启不会丢计数、也不会和表里的数据对不上。默认关闭：

```bash
# 受凭据保护（推荐）
QDRUST_METRICS_TOKEN=change-me

# 或者只在内网信任、由 sidecar 抓取
QDRUST_METRICS=1
```

```yaml
scrape_configs:
  - job_name: qdrust
    authorization:
      credentials: change-me
    static_configs:
      - targets: ["qdrust:8923"]
```

指标含义：

| 指标 | 类型 | 用途 |
|---|---|---|
| `qdrust_runs_total{status=...}` | gauge | 各终态的累计运行数。六种状态**都**会输出，为 0 也输出，否则 `rate()` 与告警规则会在第一次失败时才发现序列不存在 |
| `qdrust_runs_active` | gauge | 排队 + 执行中的运行数 |
| `qdrust_runs_oldest_pending_age_seconds` | gauge | 最久等待的那个运行已经等了多久；队列空时为 0。调度器卡住时这个数会一直涨 |
| `qdrust_tasks_total{disabled=...}` | gauge | 已启用 / 已停用的任务数 |
| `qdrust_notification_deliveries_total{status=...}` | gauge | 通知投递成功 / 失败数，配合运行详情页的「投递记录」定位「签到成功但没收到消息」 |
| `qdrust_users_total` | gauge | 账号总数 |
| `qdrust_build_info{version=...}` | gauge | 恒为 1，只用来把版本带进标签 |

数据库读不到时返回 `503` 并附一行 `# scrape failed:` 说明，而不是输出一排 0 —— 一排 0 在面板上看起来等于「一切正常」。

## 数据库

- **SQLite**：默认，零配置，文件位于 `DATABASE_URL` 指向的路径。
- **MySQL**：设置 `DATABASE_URL=mysql://...` 即自动切换；迁移脚本位于 `migrations-mysql/`。
- 迁移为**前向 only（forward-only）**，升级前务必备份数据库。

### 直接复制数据库文件（热拷贝）

停掉服务再拷最稳妥。运行中直接拷走 `qdrust.db` + `qdrust.db-wal` + `qdrust.db-shm` 也行，但有前提：

- **三个文件必须同一时刻拷齐。** 只拷 `.db` 会丢掉还在 WAL 里、尚未 checkpoint 的数据。
- 拷贝过程中若发生 checkpoint，`.db` 与 `-wal` 会互相不一致，库可能损坏——低峰期做风险最低。
- `-shm` 拷不拷都行，它会自行重建。

三个文件在同一个容器里、属主一致时，新容器起来即可用（属主问题见 [数据目录的属主](#数据目录的属主)）。

若在意一致性，可以让 SQLite 自己产出一个自洽的单文件快照，只拷这一个文件：

```bash
# 需要 SQLite 3.27+，且镜像里没有 sqlite3 命令 —— 在装了它的机器上执行
sqlite3 /data/qdrust.db "VACUUM INTO '/data/snapshot.db'"
```

跨后端、且完全不碰文件的做法是管理页的**备份 / 恢复**（`GET /api/v1/admin/backup`），导出的是全表 JSON，SQLite 与 MySQL 通用。两者是独立的两条路，取舍见 [运维与发版](operations.md)。

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

若健康检查失败：停新容器 → 恢复升级前数据库备份 → 启动记录的上一镜像 digest。详见 [运维与发版](operations.md)。
