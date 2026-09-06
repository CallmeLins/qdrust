# qdrust 第三方 IdP 接入实现计划（OIDC / Header）

> 对应 GitHub Issue #3。目标：为 self-hosted 部署接入第三方身份提供商
> （Authentik / Authelia / Keycloak / Pocket ID 等），在**保留本地账密能力**的前提下，
> 提供标准 **OIDC** 与轻量 **Header Auth** 两种外部登录入口。
>
> 本文件是开工依据：含分阶段改动清单、文件级落点、数据模型、配置、API、安全要求与测试映射。
> 与根目录 `MIGRATION_PLAN.md`（全项目路线图）互补，本文件只聚焦认证改造。
>
> **rev 记录**：v1 初稿 → 用户评审提出 10 点工程修正 → 修正版（默认 `auth_mode=local`、
> 哨兵哈希、state 持久化、`ConnectInfo` 启动改造等已并入）。
>
> **进度**：Phase 0–5 **全部完成**
> （server lib 91 passed、clippy/fmt 干净；webui vue-tsc/vitest 16 passed/build 通过）：
> AuthMode+`/auth/config`+入口守卫 + 双后端迁移 + 外部身份 store 层（冲突拒并 + 哨兵建档，
> role 首登写死不随组刷新）+ OIDC state 持久化 + **OIDC 完整授权码+PKCE 流程**（start/callback、
> 确定性 verifier/nonce、ID token 校验、redirect_uri 运行时推导、state cookie Lax 与 qd_session 分开）
> + 外部登录审计 + 登录页按 auth_mode 渲染 + **Header Auth**（ConnectInfo 可信代理、
> 头注入防护、会话复用不膨胀、缺可信代理启动失败）+ **部署文档**（README/.env.example/反代示例）
> + **OIDC groups claim 映射**（`oidc.groups_claim` 默认 `groups`，从已验签 ID token 提取
> 组成员并喂入首登角色解析，命中 `admin_groups` → admin）
> + **OIDC state 过期定时清理**（已接入调度器小时级维护）。
> 待办（收尾）：真实 IdP 端到端人工验证。

> 待办：真实 IdP 端到端验证。

---

## 0. 目标与非目标

### 0.1 核心立场（用户已拍板：**入口可关 · 能力恒在**）

- **本地账密机制、代码、`users` 数据、argon2 校验永久保留，不删。**
- **运行时入口可按部署者配置关闭**：启用 OIDC 的部署，前端不再展示本地登录框、
  本地登录 API 默认返回禁用 —— 日常运维纯走 IdP。
- 本地账密作为**基线与紧急恢复通道**保留，可随时配置切回。

### 0.2 目标

1. 统一"外部身份登录"抽象层，避免把 OIDC 与 Header 混成一个实现。
2. OIDC：Authorization Code + PKCE，标准 discovery，适配 Authentik / Keycloak / Pocket ID 等。
3. Header Auth：可信来源 IP + `Remote-User` 头，作为明确的高级部署模式。
4. 复用现有 `create_session()` / cookie 会话 / 权限体系 —— 业务模块不感知认证来源。
5. 外部用户自动建档，`users` 结构尽量少改。

### 0.3 明确不做（一期）

- 不做"一人绑多个 IdP"的用户自助管理 UI（`unlink`/`identity` 端点后置）。
- 不做 SAML / LDAP。
- 不迁移、不删除任何现有本地用户数据。
- Header Auth 不作为默认入口。
- **第一版不把 `password_hash` 改可空**（见 §3 哨兵方案）。

---

## 1. 现状梳理（已核对代码，作为落点依据）

### 1.1 认证流程

```
login/register/bootstrap
  -> hash_password / verify_password (argon2, auth.rs)
  -> store.credentials_by_username / create_user / create_first_admin
  -> issue_session_response() → store.create_session() → qd_session + csrf cookie
  -> 后续 require_session() / require_csrf() (cookie) 鉴权
```

- 会话完全基于 cookie（`qd_session` HttpOnly **SameSite=Strict** + `qd_csrf`），无 Bearer/header 会话通道。
- 登出走 `revoke_session()`；`users.session_version` 支持全量撤销。
- 首个用户由 `create_first_admin`（事务内 `COUNT(*)==0`）判定为 `admin`；`register` 恒建 `role=user`。

### 1.2 关键文件与结构约束

| 文件 | 结构要点 | 对本次改造的约束 |
|---|---|---|
| `crates/qdrust-server/src/store.rs` | `define_store!` 宏生成 sqlite/mysql 两个模块；运行时 `Store` enum + `delegate!` 转发 | **新增 store 方法需在宏体内写一次（双后端共享），再注册进 `delegate!` 列表** |
| 迁移 | `sqlx::migrate!("../../migrations")`(sqlite) 与 `../../migrations-mysql`(mysql) 双目录 | **schema 改动必须同时落两个目录** |
| `api.rs` | 认证路由集中在 160-205 行；`router_with_auth` 已带 `base_path` 参数 | 新端点复用同前缀/路由机制 |
| `model.rs` | `User`（API 类型，无密码）、`UserCredentials`（本地登录凭据载体） | 哨兵方案不改字段类型（见 §3） |
| `config.rs` | `Config` + `from_env`/`from_file`/`validate` | 新增配置项走同一管线 |
| `auth.rs` | argon2 哈希、令牌、`LoginRateLimiter` | 本地密码能力保留于此，不改 |
| `main.rs` | **`axum::serve(listener, app)`（未用 `into_make_service_with_connect_info`）** | Header Auth 需改启动方式（见 §4/§7.4） |
| `redis_cache.rs` | `REDIS_URL` 可选；session cache | OIDC state 可复用它或 DB |

### 1.3 已有可直接复用的资产

- `store.create_session(user_id, ttl)` + `issue_session_response`：外部登录最终都落到这里。
- `revoke_session` / `session_version`：登出与撤销已完备。
- `QDRUST_BASE_PATH` + `apiPath()` 运行时前缀：`/auth/oidc/callback` 回跳可推导，支持 sub-path。
- `reqwest`（rustls）已存在：可复用做 OIDC discovery / token / userinfo 出站。
- `redis_cache` + `REDIS_URL`：OIDC login-state 的可选存储后端。

---

## 2. 配置模型（auth-mode）

### 2.1 新增配置项（Config 结构 + env + 可选 file + validate）

| 项 | 类型/默认 | 说明 |
|---|---|---|
| `auth_mode` (`QDRUST_AUTH_MODE`) | `local`/`hybrid`/`oidc`，**默认 `local`** | 见 §2.2 语义 |
| `local_login_enabled` (`QDRUST_LOCAL_LOGIN_ENABLED`) | bool, 跟随 auth_mode | 单独强制开本地登录 API/UI |
| `emergency_admin_login` (`QDRUST_EMERGENCY_ADMIN_LOGIN`) | bool | 见 §7.5 |
| `oidc_enabled` | bool, false | |
| `oidc_issuer` | URL | discovery 基址 |
| `oidc_client_id` / `oidc_client_secret` | str | |
| `oidc_redirect_uri` | URL, 默认推导 | 见 §2.3 |
| `oidc_scopes` | str, 默认 `openid profile email` | |
| `oidc_provider_name` | str, **显式公开配置** | 用于 `/auth/config` 展示，不从 issuer 推断 |
| `oidc_auto_create_users` | bool, 默认 true | |
| `oidc_default_role` | `admin`/`user`, 默认 `user` | |
| `oidc_admin_groups` | 逗号分隔 | group→admin 映射 |
| `header_auth_enabled` | bool, false | |
| `header_auth_user_header` | 默认 `Remote-User` | |
| `header_auth_email_header` | 默认 `Remote-Email` | |
| `header_auth_groups_header` | 默认 `Remote-Groups` | 解析格式见 §8 修正项 |
| `header_auth_admin_groups` | 逗号分隔 | |
| `header_auth_trusted_proxies` | 逗号分隔 CIDR | 来源网段白名单 |
| `header_auth_trusted_proxy_required` | bool, 默认 true | 无可信代理配置时 Header Auth **启动失败**，而非静默接受 |
| `header_auth_auto_create_users` | bool, 默认 false | |
| `header_auth_separator` | 默认 `,` | groups header 固定分隔符 |

### 2.2 auth-mode 语义（**默认 local**，严格后向兼容）

| 值 | 含义 | 何时进入 | 本地登录 API | 前端登录框 |
|---|---|---|---|---|
| `local`（**默认**） | 仅本地（=今日现状） | 初始 / 未显式配置任何外部 provider | 开 | 显示本地 |
| `hybrid` | 本地 + 已启用外部 provider | **显式**配置了 OIDC 或 Header 后由默认进入 | 开 | 本地 + SSO 按钮 |
| `oidc` | 走 IdP；本地机制保留可切回 | 显式 | 关（除非 `local_login_enabled=true`） | 隐藏本地，SSO / 自动跳转 |

> **兼容性原则**：默认 `local` 保证升级现有部署后登录页行为**零变化**。
> 只有当部署者显式设置 `oidc_enabled=true` / `header_auth_enabled=true` 时，auth_mode 才进入
> `hybrid`（或部署者主动设 `oidc`）。禁止"检测到配置就隐式切换"而改变现有登录页。

### 2.3 redirect_uri：运行时推导（兼容 sub-path）

- 默认推导：`{X-Forwarded-Proto}://{X-Forwarded-Host}{base_path}/api/v1/auth/oidc/callback`
  （`X-Forwarded-*` 仅当反代/可信代理场景采用）。
- `oidc_redirect_uri` 作显式覆盖。

---

## 3. 数据模型改动

### 3.1 决策：**不把 `password_hash` 改可空 —— 采用不可登录哨兵哈希**（修正项 2）

**理由**：改 nullable 会引入双迁移复杂度，并在 `UserCredentials`/`credentials_from_row`/多处放大
`Option<String>` 影响面。更稳妥：

- `users.password_hash` **保持 `NOT NULL`**，类型不变。
- 外部用户写入一个**固定、永远无法验证成功的 Argon2 哨兵哈希**（如 `hash_password(随机长串)` 预生成常量）。
- 效果：
  - 现有 `credentials_by_username`/`credentials_from_row` **零改动**；
  - 本地登录对外部用户**天然失败**（哨兵验不过）；
  - 无需在大量路径引入 `Option<String>`。
- 后续若确需区分"无本地密码 vs 有本地密码"，再增加 `auth_source = local|oidc|header` 列，
  **让该列承担"来源"语义**，而非让 `password_hash` nullable 承担多个语义。

> `create_user`/`create_first_admin` 的 argon2 前缀断言保持不动。外部建档走新方法（见 §3.3）。

### 3.2 新增 `external_identities` 表（双后端迁移）

```sql
-- sqlite: migrations/20260906XXXX_external_identities.sql
CREATE TABLE external_identities (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider      TEXT NOT NULL,             -- 'oidc' | 'header'
    issuer        TEXT NOT NULL DEFAULT '',  -- OIDC issuer；header 可为 '' 或固定 'header'
    subject       TEXT NOT NULL,             -- OIDC sub / header Remote-User（归一化后）
    email         TEXT,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER NOT NULL,
    UNIQUE (provider, issuer, subject)
);
CREATE INDEX idx_external_identities_user ON external_identities(user_id);
```

```sql
-- mysql: migrations-mysql/20260906XXXX_external_identities.sql（列类型/主键对应调整）
```

> 映射键：**provider + issuer + subject**。不依赖 email（email 可空、可变、无唯一约束）。

### 3.3 新增 OIDC login-state 表（修正项 3：state 必须持久化）

**原则**：state/nonce/PKCE 不能只存进程内 —— 否则多实例、重启或回调落到不同实例都会失败。

- 配置了 Redis（`REDIS_URL`）→ 优先存 Redis（带 TTL）。
- 无 Redis → 存数据库表：

```sql
-- sqlite: migrations/20260906XXXX_oidc_login_states.sql（并入同一迁移或独立）
CREATE TABLE oidc_login_states (
    state_hash              TEXT PRIMARY KEY,
    nonce_hash              TEXT NOT NULL,
    pkce_verifier_encrypted TEXT NOT NULL,   -- 用服务端密钥加密（见下）
    redirect_uri            TEXT NOT NULL,
    created_at              INTEGER NOT NULL,
    expires_at              INTEGER NOT NULL
);
-- 定期清理过期行（可复用调度器，参照 log_retention）
```

- **`pkce_verifier_encrypted`**：verifier 是敏感的（可换 token），应加密存储，不落明文。
- 单实例无 Redis 时，进程内存缓存可作为**可选加速**，但**主存储必须可跨实例**。

### 3.4 新增 store 方法（宏体内写一次 + 注册进 `delegate!`）

```rust
pub async fn find_or_create_external_user(
    &self,
    provider: &str,
    issuer: &str,
    subject: &str,
    email: Option<&str>,
    username_hint: Option<&str>,
    auto_create: bool,
    role: &str,
) -> Result<Option<User>>;

pub async fn external_identity_for(
    &self,
    provider: &str, issuer: &str, subject: &str,
) -> Result<Option<(i64, String)>>;

// 仅当确需区分来源时（二期），才加 auth_source 列相关方法。
```

> 建档写入哨兵哈希（§3.1）。**外部身份冲突策略在 Phase 0 定义**（见 §4.3）。

---

## 4. 统一外部身份登录层

### 4.1 抽象

```
ExternalProvider trait（一期两个实现：OidcProvider, HeaderProvider）
  begin(...) -> Redirect / Identity
  resolve_identity(...) -> ExternalIdentity
ExternalIdentity { provider, issuer, subject, email, username_hint, groups: Vec<String> }
  -> find_or_create_external_user → 角色解析 → issue_session_response()/create_session() → qd_session
```

业务模块完全不感知认证来源。

### 4.2 角色解析（默认保守）

```
匹配 admin_groups → admin；否则 → oidc_default_role / user。
角色同步默认关闭；仅"首次建档时写死"，除非部署者显式开启"每次登录按组同步"。
```

### 4.3 外部身份冲突策略（**Phase 0 必须定义**）（修正项 8）

典型冲突：OIDC subject 已绑定 user A，但 email 对应本地 user B。

- **默认：拒绝自动合并**。任何"用 email 自动挂靠/合并"都可能账号接管。
- 冲突时：不建档、不自动登录，**记录安全审计日志**，返回明确错误码，要求管理员处理
  （管理员端核实后手动关联或合并）。
- 禁止通过 email 隐式把外部身份合并到本地同名用户。

---

## 5. API 设计

### 5.1 公开端点

| 端点 | 说明 |
|---|---|
| `GET /api/v1/auth/config` | 公开登录配置（**不含 secret**） |
| `GET /api/v1/auth/oidc/start` | 发起：生成 state+nonce+PKCE verifier → **持久化到 Redis/DB** → 302 IdP |
| `GET /api/v1/auth/oidc/callback` | 回跳：校验 state → 取持久化记录 → code 换 token → 校验 → 查/建用户 → 建会话 |

### 5.2 认证/账号端点（一期后置/留桩）

| 端点 | 说明 |
|---|---|
| `GET /api/v1/auth/identity` | 当前用户外部身份（一期空列表） |
| `POST /api/v1/auth/oidc/unlink` | 解绑（后置） |

### 5.3 `GET /api/v1/auth/config` 响应契约

```json
{
  "auth_mode": "local",
  "local_login_enabled": true,
  "oidc_enabled": false,
  "oidc_provider_name": "Authentik",
  "header_auth_enabled": false
}
```

> **`oidc_provider_name` 是显式公开配置项**（`oidc_provider_name`），**不从 issuer URL 推断**（修正项 10）。
> 绝不返回 client_secret / 任何敏感项。

### 5.4 OIDC 回调安全（修正项 4：不自拼 JWT/JWKS）

- **用 `openidconnect` crate**，不自行实现 JWT/JWKS 校验。实现前确认：
  - discovery 的 **JWKS 自动刷新**；
  - `issuer`/`audience`/`nonce`/`exp` 校验；
  - **多个 audience** 支持；
  - `at_hash` 是否需要（按需）；
  - clock skew 容忍；
  - JWKS 轮换。
- **PKCE 强制启用（S256）**，不写成 "best effort"。
- confidential client + client_secret 做 token 端点认证。

---

## 6. 本地登录 API 的禁用语义（入口可关）

当 `local_login_enabled=false`（由 auth_mode=oidc 决定）时：

| 端点 | 行为 |
|---|---|
| `POST /api/v1/auth/login` | 403 code `local_login_disabled` |
| `POST /api/v1/auth/register` | 禁用 |
| `POST /api/v1/auth/forgot-password` | 禁用 |
| `POST /api/v1/auth/bootstrap` | 若无任何用户，仍允许（首管理员引导） |
| 已有有效 session 的 API | 不受影响（禁的是"新登录入口"，不影响已签发会话/账户内操作） |

---

## 7. 实施顺序（Phase）

### Phase 0：AuthMode + 公开配置 + 入口守卫 + 外部身份表 + state 存储

- [x] `config.rs`：`AuthMode` enum（默认 `local`）+ 配置项 + env/file/validate（已含 `/auth/config` 公开项）
- [x] 双后端迁移：`external_identities` 表 + `oidc_login_states` 表
      （`migrations/202609060001_*` + `migrations-mysql/202609060001_*`）
- [x] `model.rs`：`ExternalIdentity` / `ExternalIdentityClaim` / `ExternalLoginResolution` /
      `OidcLoginState` 模型
- [x] `store.rs`：宏体新方法 + 注册 `delegate!`
      （`external_identity_for` / `resolve_external_identity` / `insert_oidc_login_state` /
      `take_oidc_login_state` / `purge_expired_oidc_login_states`，哨兵 hash 建档，单次消费）
- [x] `api.rs`：`GET /auth/config` + 登录端点禁用守卫（§6）
- [x] **外部身份冲突策略落地**（§4.3：email 归属他人 → 拒自动合并 + 审计）
- [x] 测试：auth-mode 解析、config 不含 secret、禁用语义、冲突策略、单次/过期 state
      （server lib 67 passed）

### Phase 1：OIDC discovery + Authorization Code + PKCE

- [x] `Cargo.toml`：加 `openidconnect` 3.5.0（PKCE S256 强制，reqwest 后端）
- [x] `config.rs`：`OidcConfig`（issuer/client_id/secret/redirect/scopes/auto_create/default_role/admin_groups）
      + validate 强校验（oidc_enabled 时 issuer+client_id+client_secret 必填）
- [x] `oidc.rs`：discovery（`discover_async`）、确定性 PKCE verifier/nonce（`b64url(sha256(state‖secret))`，
      DB 只存 hash）、state 持久化（`insert_oidc_login_state` 单次消费）、token 交换、
      ID token 校验（issuer/audience/nonce/exp/sig）
- [x] `api.rs`：`/auth/oidc/start`、`/auth/oidc/callback`（start 307 IdP + `qd_oidc_state` Lax cookie；
      callback 校验 state/nonce/redirect → 换 token → 验 ID token → `resolve_external_identity` →
      签发 qd_session 后 307 回 `{base}/`）
- [x] redirect_uri 运行时推导（X-Forwarded-Proto/Host + base_path），兼容 sub-path
- [x] cookie 处理：state cookie `SameSite=Lax` 与最终 `qd_session`(Strict) 分开；callback 后清除 state cookie
- [ ] 端到端（连真实 IdP）人工验证 / mock IdP 集成测试
- [ ] state 过期定时清理接入调度器（DB 路径 `purge_expired_oidc_login_states` 已备）

### Phase 2：外部用户映射 + 冲突策略 + 角色组映射

- [x] `find_or_create_external_user` auto_create（哨兵 hash）
      （实为 `resolve_external_identity` + `provision_external_user`，Phase 0 已落地并测试）
- [x] 同 sub 不重复建档；email 变化不建新用户
- [x] 组→admin 映射（默认首登写死）——role 仅在首次建档时由 `groups_overlap` 决定；
      后续登录**不刷新角色**（已补注释 + 测试 `external_role_is_pinned_at_first_login_not_refreshed_by_later_groups` 固化）
- [x] 冲突审计日志——handler 层落 `auth.external_refused`（actor=None，details 含冲突邮箱）；
      成功建档/复用落 `auth.external_user_created` / `auth.external_login`
      （helper `record_external_login_audit` + api 测试 `external_login_writes_created_and_reused_audit` /
      `external_conflict_writes_refused_audit_with_claimed_email`）
- [x] 测试

### Phase 3：前端登录策略

- [x] 登录页读 `/auth/config`；按 auth_mode 渲染
      （`api.authConfig()` + App.vue `authPolicy`；工具 `ssoAvailable/ssoOnly/localLoginAvailable`）
- [x] SSO 按钮 → `/auth/oidc/start`；oidc 模式无本地框
      （`oidcStartUrl()` 运行时前缀；纯 OIDC 渲染 SSO-only 面板；local_login_enabled=false 隐藏本地表单）
- [x] 回调错误/成功落地页（回到 sub-path）
      （读 `?login_error=<code>` → 友好文案映射；成功由服务端 307 回 `{base}/` 后 session 检测登录）
- [x] vitest（utils 策略 3 + oidcStartUrl 2；webui 16 passed、vue-tsc 干净、build 通过）

### Phase 4：Header Auth + ConnectInfo + 可信代理校验（修正项 5）

- [x] **启动改造**：`main.rs` 改为
      `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())`，
      否则 handler 拿不到真实源 IP（原代码为裸 `axum::serve`）
- [x] 中间件：`header_auth_middleware` 基于 `ConnectInfo<SocketAddr>` 扩展判源 IP ∈ `header.trusted_proxies`
      （`src/header_auth.rs` 纯逻辑 `source_is_trusted` + `extract_header_identity` + `strip_identity_headers`）
- [x] `header.trusted_proxy_required=true`（默认）：`config.rs` `validate()` 在
      header auth 开启且无可信代理时 **启动失败**（fail-fast，非运行时静默接受）
- [x] 读取 user/email/groups header（可配 `Remote-User`/`Remote-Email`/`Remote-Groups`，
      分隔符可配默认逗号）；**不可信来源剥离外部同名 header**（防伪造）
- [x] **不每次请求建会话**：仅当无有效 qd_session / header 身份与 session 用户不一致
      （身份漂移 revoke 旧 session 后重建）才建/刷；否则复用既有 session，避免会话膨胀与审计噪音
- [x] `resolve_external_identity`（auto_create 默认 false；enabled 时才开）
- [x] 登出只清 qdrust session（logout 走既有 revoke_session）
- [x] 测试：不可信源 403、trusted_proxy_required=false 忽略、缺用户名拒、可信源建档+下次带 cookie 认证、
      auto_create off 拒、session 复用不膨胀、同名 email 冲突拒并、登出仅清 qdrust session、
      config 缺可信代理启动失败（server lib 91 passed）

### Phase 5：应急本地管理员入口 + 文档

- [x] `QDRUST_LOCAL_LOGIN_ENABLED` / bootstrap 兜底（§6/§7.5）
      （`auth_mode=oidc` + `QDRUST_LOCAL_LOGIN_ENABLED=true` 保留本地入口；
      bootstrap 在无管理员时始终可用；config 测试 `oidc_mode_keeps_local_entry_only_when_forced` 已固化）
- [x] README / .env.example / 反代示例
      （README 新增「第三方登录」小节：auth_mode/local 开关、OIDC env、Header Auth env、
      nginx forward-auth 示例与安全注记；`.env.example` 补齐全部 `QDRUST_*` 认证变量注释）
- [x] 全量回归（server lib 91 passed、clippy -D warnings 干净、fmt 干净；webui 16 passed、build 通过）

### Phase 6：OIDC groups claim 映射 + state 过期清理接线

- [x] `OidcConfig.groups_claim`（默认 `groups`，env `QDRUST_OIDC_GROUPS_CLAIM` / config-file `groups_claim`）
- [x] `oidc::groups_from_id_token`：从**已验签** ID token 提取组成员（数组或逗号串；缺失/畸形 → 空）
      —— openidconnect 把 token 绑到 `EmptyAdditionalClaims`，非标准 claim 在反序列化即被丢弃，
      故验签后重解 payload（`IdToken::to_string()` 还原 compact JWT）读取，安全性不变。
- [x] OIDC 回调 `claim.groups` 由真实 claim 填充 → 首登命中 `admin_groups` 即为 admin，
      否则落 `default_role`（store `groups_overlap` 逻辑复用，已测试）。
- [x] 调度器维护循环接入 `purge_expired_oidc_login_states()`（小时级清理，与
      `purge_expired_sessions` 等并列）。

---

## 8. 测试映射与安全清单

| 场景 | 期望 |
|---|---|
| OIDC state 不匹配 | 拒，不建 session |
| OIDC nonce 不匹配 | 拒 |
| issuer / audience / token 过期 | 拒 |
| 用户 disabled 后 OIDC 登录 | 拒 |
| 相同 sub 重复登录 | 复用同用户 |
| email 变化 | 不建新用户 |
| 组→admin（首登） | role 首登由组决定；**后续登录不刷新角色**（写死语义，见 Phase 2） |
| 外部登录成功（新建/复用） | 审计 `auth.external_user_created` / `auth.external_login` |
| 外部登录拒绝（冲突/禁用/auto_create 关） | 审计 `auth.external_refused`（actor=None，details 含 reason+claimed email） |
| **subject 绑 A 但 email 对 B** | **拒自动合并 + 审计 + 管理员处理**（Phase 0 定义） |
| Header 非可信来源 | 403 |
| **无可信代理配置时开 Header Auth** | **启动失败** |
| Header 缺用户名 | 拒/忽略 |
| **Header 连续请求** | **只首请求建/刷 session，后续复用，不膨胀** |
| **groups header 格式** | **仅接受固定分隔符（默认逗号）；明确 trim 与大小写；角色同步默认关** |
| 本地登录禁用 | login/register/forgot 禁用；已登录 session 可用 |
| OIDC 回调失败 | 不建 session |
| 登出 | revoke_session 生效 |
| discovery / IdP 不可达 | 优雅降级，不锁死已登录用户 |
| `/auth/config` | 不含 secret；provider_name 来自显式配置 |
| **OIDC state 过期清理** | 定期清理 `oidc_login_states` 过期行 |
| **OIDC groups→admin** | 从已验签 ID token 读 `groups_claim`（默认 groups，数组或逗号串），首登命中 `admin_groups` → admin；claim 缺失统一落 default_role |

---

## 9. 风险与注意

1. **Header Auth 安全边界最脆弱**：源 IP 判定基于 `ConnectInfo`（需先改启动方式）；
   `header_auth_trusted_proxy_required` 保证"没配可信代理就不开"，杜绝静默接受 header。
2. **redirect_uri 与 sub-path**：硬编码破坏单镜像多前缀；用运行时推导 + 显式覆盖。
3. **迁移双后端**：漏 mysql 目录则 MySQL 部署挂。
4. **store 方法三处落点**：宏体 + `delegate!`，缺一则编译失败或运行 panic。
5. **`oidc` auth-mode 别删本地能力**：只是入口关闭，靠 `local_login_enabled` 保底。
6. **OIDC state 不落内存**：Redis/DB 持久化，跨实例/重启安全；verifier 加密存储。
7. **SameSite=Strict**：OIDC callback 为顶层导航，需单独验证；state cookie 用 Lax、与 `qd_session` 分开。
8. 前端登录框显隐由服务端 `/auth/config` 驱动，不硬编码前端。
9. 不自拼 JWT/JWKS，用 `openidconnect`（含 JWKS 自动刷新/轮换、多 audience、clock skew）。

---

## 10. 与 v1 及社区建议的差异（评审修正汇总）

| 项 | v1 / 社区 | 修正版 |
|---|---|---|
| 默认 auth_mode | hybrid | **local**（后向兼容，显式配置才进 hybrid/oidc） |
| `password_hash` | nullable | **保持 NOT NULL + 哨兵哈希**；需区分来源时再加 `auth_source` 列 |
| OIDC state/nonce/PKCE | 未明确存储 | **Redis(优先)/DB 持久化**，非进程内；verifier 加密 |
| OIDC token 校验 | openidconnect（best-effort PKCE） | **openidconnect 强制 PKCE S256**；确认 JWKS 刷新/轮换/多 audience/clock skew |
| Header 源 IP | 提到 ConnectInfo | **改 main.rs 启动为 `into_make_service_with_connect_info`**；加 `trusted_proxy_required`，无配置则启动失败 |
| SameSite | 沿用 Strict | state cookie 用 **Lax**，与 `qd_session` 分开；验证 callback 流程 |
| Header session 创建 | 每请求 | **仅无 session/身份不一致/身份变化时才建或刷新** |
| 外部身份冲突 | 未明确 | **默认拒自动合并 + 审计 + 管理员处理**（Phase 0 定义） |
| Remote-Groups 解析 | 未明确 | 固定分隔符（默认逗号）、trim、大小写策略明确；角色同步默认关 |
| provider 展示名 | issuer 推断 | **`oidc_provider_name` 显式公开配置** |
