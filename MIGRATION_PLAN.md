# qdrust 全新实现计划（对齐 QD2 路线图）

## 1. 项目定位

qdrust 是参考 QD 产品能力设计的全新 Rust Web 应用，核心用途是创建、调试、调度和观察 HTTP 自动化任务。

原 Python QD 只作为功能和交互参考，不是兼容契约。项目按新的安全边界、数据模型、API 和 WebUI 规范实现，不逐行翻译 Python 代码。

产品功能范围对齐 QD2 路线图：核心 HAR 引擎、CLI、RESTful Server 和 Vue WebUI 四部分均需实现。数据库、内部 API 和代码实现不兼容旧 QD，但旧 QD HAR 文件是明确的外部兼容契约。

### 1.1 QD2 路线映射

| QD2 模块 | qdrust 模块 | 职责 |
| --- | --- | --- |
| QD_Core | `qdrust-core` | HAR 解析执行、HTTP 客户端、变量/控制流、插件宿主 |
| QD_Cli | `qdrust-cli` | 本地校验、执行 HAR、变量注入和机器可读输出 |
| QD_Server | `qdrust-server` | 数据库、认证、REST API、调度、通知和插件管理 |
| QD_Web | `webui` | Vue WebUI、个人/公共模板、任务、插件、通知和记事本 |

依赖方向固定为：`qdrust-server -> qdrust-core`、`qdrust-cli -> qdrust-core`。`qdrust-core` 不得依赖数据库、Axum 或 WebUI，CLI 与 Server 必须调用同一套 HAR 执行实现。

### 1.2 目标

- 提供可视化 HTTP/HAR 任务编辑、调试和定时执行能力。
- 提供可靠的任务调度、重试、取消、日志和通知机制。
- 使用 Rust 后端与 Vue WebUI，前后端通过版本化 API 协作。
- 默认支持 SQLite，完整部署支持 MySQL 和可选 Redis。
- 将 Docker 作为正式交付方式，同时支持 Windows/Linux 本地开发。
- 从第一版开始具备权限隔离、资源限制、可观测性和可恢复部署。
- 实现 QD2 路线图中的公共模板、插件管理、消息推送、记事本和国际化。
- Core、CLI、Server 和 Web 可以独立测试、构建和发布。

### 1.3 明确不做

- 不导入或原地升级 Python QD 数据库。
- 不解密旧 `mcrypto` 数据，不兼容旧密码哈希和 Tornado Cookie。
- 不兼容旧 URL、HTML 表单和 Tornado API。
- 不兼容旧数据库，但旧 QD 导出的 HAR 文件必须可以无损导入并按旧语义运行。
- 不实现 Python `safe_eval`，不嵌入 Python 运行时。
- 不复用旧 Jinja 页面、Angular/Bower 前端或旧静态资源构建链。
- 首版不提供桌面客户端和移动原生客户端，只提供 WebUI。
- 不加载不受信任的 Rust 动态库插件，不允许插件绕过统一的网络和资源策略。

## 2. 产品范围

### 2.1 P0 核心能力

- 用户注册、登录、退出、修改密码和管理员禁用用户。
- HTTP 模板创建、JSON/HAR 导入、编辑、复制、删除和调试。
- GET/POST/PUT/PATCH/DELETE、query、headers、Cookie、JSON/form/raw body。
- 响应状态、headers、文本和 JSON 数据提取。
- 明确受限的变量替换、条件和循环。
- cron 调度、时区、立即运行、禁用、取消和失败重试。
- 任务执行历史、结构化步骤日志和实时日志。
- 用户资源隔离和管理员站点设置。
- SQLite 单机部署和 Docker 镜像。
- `qdrust-cli validate/run`，能够在没有 Server 和数据库时运行 QD HAR。
- 插件 manifest、配置校验、启停和受控 API 调用。
- 中英文国际化基础设施，后端错误使用稳定 code，显示文本由客户端本地化。

### 2.2 P1 扩展能力

- multipart/file、HTTP/SOCKS 代理和自定义 CA。
- 公共模板发布、搜索、复制和订阅更新。
- 邮件、Webhook 等通知渠道。
- MySQL、Redis Session/队列/分布式租约。
- 批量任务操作、日志保留策略和备份恢复。
- 多实例部署与水平扩展。
- 公共模板完整发布/更新流程、自定义插件管理、消息推送动作和记事本。

### 2.3 后续候选

- 浏览器自动化任务、OCR 和复杂验证码。
- 团队/租户、审计导出和细粒度 RBAC。
- 外部对象存储和大规模日志检索。

候选能力不进入首版数据模型，除非当前设计会明确阻断后续扩展。

## 3. 总体架构

```text
Browser
  -> Vue 3 WebUI
      -> /api/v1 REST
      -> /ws WebSocket
          -> Axum API / Auth / Services
              -> SQLx -> SQLite or MySQL
              -> Scheduler -> Worker -> HTTP Engine
              -> optional Redis
              -> Notification adapters
```

推荐 workspace：

```text
crates/
  qdrust-server       Axum 启动、路由、中间件和静态文件
  qdrust-core         HAR 引擎、HTTP 客户端、插件宿主、领域模型和错误类型
  qdrust-db           SQLx repository 与 migration
  qdrust-engine       HTTP 模板解析和执行
  qdrust-scheduler    调度、队列、租约和恢复
  qdrust-cli          管理、迁移、备份和诊断命令
webui/                Vue 3 WebUI
tests/                跨 crate 集成和端到端 fixture
```

领域层不能依赖 Axum、SQLx 或具体通知实现。HTTP handler 只负责协议转换、认证上下文和调用 service。

当前单 crate 仅作为验证骨架。完成 QD HAR 兼容模型后立即拆分 workspace，避免执行器继续与 Server 数据层耦合。

### 3.1 插件架构

QD 的 `api://` 扩展能力改为显式插件协议：

- Core 定义版本化 `PluginManifest`、输入/输出 JSON schema、能力声明和调用接口。
- 内置插件与外部插件使用同一调用协议；内置插件随版本发布，但不获得额外隐式权限。
- 首版外部插件采用独立子进程或 WASI Component，禁止直接加载 ABI 不稳定的 Rust `.dll/.so`。
- 插件默认无网络、文件、环境变量和进程权限；权限由 manifest 声明并经管理员启用。
- HAR 中的旧 `api://...` 地址通过兼容路由映射至插件调用，无法映射时返回明确诊断。
- 插件有 deadline、输入/输出大小、日志大小和并发限制，Server 负责审计调用。

## 4. 技术决策

| 领域 | 选型 | 说明 |
| --- | --- | --- |
| Rust 运行时 | Tokio | 异步 HTTP、信号和任务管理 |
| Web | Axum + Tower | API、中间件、限流和追踪 |
| 数据库 | SQLx | SQLite/MySQL 双后端和版本 migration |
| HTTP client | Reqwest + rustls | 默认 TLS 和连接池 |
| 密码 | Argon2id | 每用户随机 salt，参数可升级 |
| Session | tower-sessions | 默认数据库 Store，Redis 可选 |
| API 契约 | OpenAPI | 生成 TypeScript client |
| 表达式 | 原生 DSL + QD 兼容表达式层 | 新模板使用安全 DSL；旧 QD HAR 覆盖其实际 Python 表达式语义 |
| 日志 | tracing | request/run/step 关联 ID |
| WebUI | Vue 3 + TypeScript + Vite | 只提供 SPA WebUI |
| 包管理 | npm | 与当前环境一致 |
| UI | Naive UI 或 Element Plus | 阶段 1 通过小型原型确定 |
| 编辑器 | Monaco Editor | JSON/HAR 编辑和诊断 |
| 前端数据 | TanStack Query + Pinia | 服务端状态与少量全局状态分离 |
| 插件协议 | WASI Component 或受控子进程 | manifest + JSON schema，隔离故障和权限 |
| 国际化 | Fluent 或 ICU MessageFormat | WebUI 文案与后端稳定错误 code 分离 |

### 4.1 ExecutionContext

一个任务运行独占一个 `ExecutionContext`：变量、Cookie jar、步骤结果、循环栈、请求预算和日志缓冲都由同一异步任务顺序修改。

- 步骤接口使用 `&mut ExecutionContext`。
- 单任务状态不使用 `Arc<Mutex<_>>`。
- 跨任务只共享只读配置、Reqwest client 和并发配额。
- 每个步骤都检查取消信号、deadline 和剩余请求数。

### 4.2 表达式双轨规范

新建模板使用项目定义的可文档化白名单 DSL：

- 基础类型：null、bool、number、string、array、object。
- 操作：比较、布尔、基础算术、索引和空值合并。
- 函数：长度、类型转换、字符串、时间和 JSON 辅助函数。
- 禁止文件、网络、环境变量、进程、动态模块和任意反射。
- 脚本有执行步数、内存、字符串长度和嵌套深度限制。

旧 QD HAR 不自动改写为新 DSL，而是进入独立兼容表达式层。兼容层只实现旧 QD HAR 可达且经过 fixture 验证的 Python 表达式语义；任何未覆盖语法必须在导入诊断和执行错误中明确指出，不能静默改变结果。两条路径都禁止文件、网络、环境变量、进程、模块导入和反射。

阶段 1 用旧 QD 源码、内置 API 模板及真实脱敏 HAR 建立语法清单，再比较 Rhai 与小型解释器。ADR 关注错误可解释性、资源限制、序列化类型、前端语法提示和 QD fixture 通过率。

### 4.3 QD HAR 兼容边界

- 兼容标准 HAR 1.2 结构以及 QD 的 `checked`、`success_asserts`、`failed_asserts`、`extract_variables` 扩展。
- 兼容 QD 在请求 URL 中编码的 `for/while/if/else/end*` 控制指令。
- 兼容请求 method、URL、headers、cookies、postData、Jinja 变量渲染和请求间 Cookie Session。
- 未识别字段必须原样保存，导入和再次导出不得静默丢失数据。
- Python `safe_eval` 不进入通用 Rust 执行环境；建立受限兼容表达式层，逐项覆盖 QD HAR 实际使用的条件、`list()`、`range()`、索引、比较和布尔表达式。
- 导入时产生兼容性诊断；只有尚未实现的语义才拒绝执行，不能在导入时篡改模板。
- 以旧 QD 的 `Fetcher.render/run_rule/parse/do_fetch` 行为和真实 HAR fixture 作为契约测试来源。

### 4.4 数据与认证

- 所有主键、时间、状态和 JSON 格式由新 schema 定义。
- 密码使用 Argon2id，登录成功时可升级参数。
- Session 存服务端，Cookie 只保存随机 session ID，启用 `HttpOnly/SameSite/Secure`。
- 用户具有 `session_version`，修改密码或管理员操作可吊销全部会话。
- repository 必须提供带用户归属条件的方法，如 `find_owned(task_id, user_id)`。
- 管理员跨用户操作走独立 service 并写审计日志。

## 5. WebUI 与 API

### 5.1 页面

- `/login`：登录。
- `/`：运行概览和近期失败。
- `/templates`：模板列表、导入、编辑和调试。
- `/tasks`：任务列表、分组、调度和批量操作。
- `/tasks/:id/runs/:runId`：运行详情和实时步骤日志。
- `/public-templates`：公共模板。
- `/settings`：个人、通知和站点设置。
- `/admin`：用户和系统状态。

### 5.2 API 规则

- JSON API 使用 `/api/v1`，WebSocket 使用 `/ws`。
- OpenAPI 是请求/响应的唯一契约，并生成 TypeScript client。
- 错误包含稳定 `code`、可读 `message`、字段错误和 `request_id`。
- 列表统一使用游标分页，不使用无限 offset 查询。
- POST 创建支持幂等键；运行和通知具有独立幂等 ID。
- API 404 返回 JSON，只有已知 WebUI route 才回退到 `index.html`。
- 身份由 middleware 建立，资源归属在 service/repository 查询中强制执行。

### 5.3 前端安全

- 不把 Session、长期 token 或密码放入 localStorage。
- 修改请求使用 CSRF token；WebSocket 校验 Session 和 Origin。
- HAR、响应和日志以文本方式展示，禁止未经清洗的 HTML 注入。
- Monaco 使用 JSON schema 提示和校验，服务端仍执行完整校验。

## 6. Docker 交付

Docker 从第一阶段持续维护：

- Node 阶段使用 npm 构建 WebUI，Rust 阶段构建 release，最终镜像只包含运行文件。
- 最终容器使用固定非 root UID/GID，不包含 Cargo、编译器、Node 或 npm。
- 目标平台为 `linux/amd64` 和 `linux/arm64`。
- SQLite、上传和备份统一位于 `/data` 持久化卷。
- 环境变量用于普通配置，Docker secret/file 用于密码和密钥。
- `/health` 表示进程存活，`/ready` 检查数据库和 migration 状态。
- migration 使用显式 CLI 命令，禁止多个副本无条件并发升级。
- SIGTERM 后停止领取任务，等待或释放 lease，再在期限内退出。
- 提供 SQLite 最小 Compose 和 MySQL + Redis 完整 Compose。
- CI 生成 SBOM、执行漏洞/许可证扫描并签名发布镜像。

Windows 开发机通过 WSL Ubuntu 执行 Docker 命令；生产镜像始终以 Linux 为目标。

## 7. 实施阶段

| 阶段 | 状态 |
| --- | --- |
| 阶段 0：范围与 ADR | Complete（2026-08-17） |
| 阶段 1：Core 与工程基座 | Implementation Complete（统一验收待执行） |
| 阶段 2：新数据库与认证 | Implementation Complete（统一验收待执行） |
| 阶段 3：HTTP 执行引擎 | Complete（本机验收，Docker/浏览器统一验收待执行） |
| 阶段 4：调度与运行记录 | In Progress |
| 阶段 5-7 | Pending |

### 阶段 0：范围与 ADR（1 周）

- 确认 P0/P1 功能矩阵和用户工作流。
- 定义新模板 JSON schema、变量类型和步骤状态机。
- ADR：SQLx 双后端、表达式、Session、调度租约、日志保留、UI 组件库。
- 建立威胁模型：SSRF、脚本逃逸、凭证泄漏、资源耗尽和越权。
- 将当前单 crate 实验骨架调整为 workspace；删除 `rusqlite` 实验数据层。
- 固化 QD2 功能矩阵和 Core/CLI/Server/Web 的依赖边界。
- ADR：插件运行方式、manifest/API 版本、权限模型和旧 `api://` 映射。

验收：所有 P0 数据结构、API 边界和非目标明确，关键 ADR Accepted。

### 阶段 1：Core 与工程基座（2-3 周）

- 拆分 `qdrust-core`、`qdrust-cli`、`qdrust-server` workspace。
- 配置、错误模型、tracing、优雅退出和国际化 message key。
- QD HAR 无损解析、兼容性诊断、HTTP 客户端和黄金 fixture。
- CLI `validate`、`inspect` 和基础 `run` 命令。
- Vue/Vite/TypeScript、Router、UI 库、API client 生成和开发代理。
- `/health`、`/ready`、OpenAPI 和 WebUI 静态托管。
- 多阶段 Dockerfile、`.dockerignore`、SQLite Compose。
- CI：fmt、clippy、test、npm lint/test/build、cargo deny、镜像构建。

验收：同一份 HAR 可由 Core 测试、CLI 和 Server 调用；本地和 Docker 均能打开 WebUI，健康检查和优雅退出通过。

### 阶段 2：新数据库与认证（2-3 周）

- 新 schema：users、sessions、templates、tasks、runs、run_steps、notifications、audit_logs。
- SQLx migration 和 SQLite/MySQL repository 契约测试。
- Argon2id 注册/登录/改密、Session、CSRF、吊销和管理员禁用。
- 配置数据库 min/max connections、timeout 和 idle lifetime。

默认连接池按后端区分：SQLite 保持小池并启用 WAL；MySQL 根据 API/worker 并发压测确定。

验收：认证安全测试、资源归属测试和双数据库 repository 测试通过。

### 阶段 3：HTTP 执行引擎（4-6 周）

1. 模板 schema 解析、版本和诊断。
2. ExecutionContext、变量替换和 Cookie jar。
3. HTTP method/query/headers/body/redirect/TLS/timeout。
4. 响应解码、JSONPath/文本/正则提取。
5. 新表达式、条件、循环和转换函数。
6. 取消、deadline、请求数、响应体和日志大小限制。
7. 日志脱敏、SSRF/代理策略和 mock server 集成测试。
8. QD HAR 的 Jinja 渲染、断言/提取、Cookie Session、控制流和 `api://` 兼容。
9. Core 插件宿主、内置插件及权限/资源限制。

验收：所有规范示例成为黄金测试；畸形模板 fuzz 不 panic；资源限制和取消测试通过。

### 阶段 4：调度与运行记录（3-4 周）

- cron + 时区、立即运行、禁用、取消、重试和并发限制。
- pending/leased/running/succeeded/failed/cancelled 状态机。
- 原子领取、lease 续期、过期恢复和幂等运行 ID。
- 有界队列与用户/全局并发配额。
- WebSocket 实时日志和断线后从数据库续读。

验收：重启、崩溃、多实例竞争、时区/DST 和长任务故障测试通过。

### 阶段 5：完整 WebUI（4-5 周）

- 登录、概览、模板编辑/调试、任务、调度、运行日志和设置。
- Monaco JSON schema、字段表单与原始 JSON 双视图。
- TanStack Query 缓存、分页、错误恢复和 WebSocket 重连。
- 管理员用户管理和基础系统状态。
- 公共模板、插件管理、消息推送动作、记事本和语言切换。

验收：Playwright 覆盖主要工作流、移动/桌面视口、权限与 XSS 场景。

### 阶段 6：P1 能力（3-5 周）

- 公共模板、通知 adapter、批量操作和备份恢复。
- MySQL + Redis Compose、多实例 lease 和 Session Store。
- 日志保留、清理和导出；先做索引/分页/保留期限，压测证明需要后再引入全文索引。
- 插件 SDK/示例、开发文档、兼容性版本策略和独立发布流程。

验收：外部通知全部使用 mock，重试不重复发送；完整 Compose 可升级和回滚。

### 阶段 7：发布与硬化（2 周）

- 性能、故障、安全和容器测试。
- amd64/arm64 镜像、SBOM、扫描、签名和版本发布。
- 全新安装、升级、备份、恢复和回滚演练。
- 用户文档、API 文档和模板规范。

验收：P0/P1 完成项通过测试矩阵，无未处理高危安全问题，发布流程可重复。

## 8. 测试门禁

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo deny check
npm --prefix webui run lint
npm --prefix webui test
npm --prefix webui run build
docker build .
```

测试层次：

- 单元：模板解析、表达式、调度时间、权限和状态机。
- 属性/fuzz：模板、URL、Cookie、表达式和响应解析。
- Repository：SQLite/MySQL 同一行为套件。
- 集成：mock HTTP、Redis、邮件和 Webhook。
- E2E：注册、建模板、调试、建任务、运行、查看日志。
- 故障：超时、DB 锁、Redis 中断、进程重启、lease 过期和磁盘不足。
- 容器：非 root、只读根目录、数据卷、healthcheck 和 SIGTERM。

## 9. 安全边界

- 默认禁止访问 loopback、私网、链路本地和云元数据地址；管理员显式配置例外。
- 每次 DNS 解析和重定向后重新校验目标，防止 DNS rebinding。
- 用户可控 header 不得覆盖内部追踪和代理认证字段。
- 请求、响应、日志、循环、脚本和总运行时间均设置硬上限。
- 密码、Cookie、Authorization、token 和自定义敏感变量默认脱敏。
- 通知和运行具有幂等键，重试不会重复产生外部副作用。
- 权限查询必须包含资源所有者，不能只依赖 WebUI 隐藏按钮。

## 10. 里程碑与工期

| 里程碑 | 阶段 | 结果 |
| --- | --- | --- |
| M0 设计冻结 | 0 | 新规范、schema、威胁模型和 ADR |
| M1 Core/CLI 可用 | 1 | QD HAR 可校验和命令行执行，模块边界稳定 |
| M2 Server 基座 | 2 | WebUI、REST API、数据库和认证 |
| M3 核心闭环 | 3-4 | QD HAR/插件可执行、任务可可靠调度 |
| M4 产品可用 | 5 | 完整 WebUI、公共模板、插件、推送、记事本和 i18n |
| M5 完整发布 | 6-7 | Docker、多架构、SDK 与生产文档 |

初始估算为 2-3 人全职约 4-6 个月。阶段 0 和执行引擎原型结束后重新估算，不以当前数字承诺固定发布日期。

## 11. 下一步

1. 按 ADR-0008 完成连接级 DNS 地址绑定及 rebinding 测试。
2. 扩充 QD Jinja helper、`api://util/*` 内置插件和真实 HAR fixture。
3. 完成 CLI 变量输入、JSON 输出和执行诊断。
4. 建立 OpenAPI 生成链路和 Element Plus 按需引入。
5. 构建首个 SQLite Docker 镜像，保持每阶段可运行。
