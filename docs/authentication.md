# 账号与第三方登录

> 返回 [README](../README.md)

## 首次启动与管理员

1. 启动服务后访问 `http://localhost:8923/`（或你的域名）。
2. 首次打开会引导创建**初始管理员账号**；之后其他用户走开放注册流程。
3. 管理员可在「设置 / 管理员」中管理用户、站点设置、日志清理、备份与恢复。

## 第三方登录（OIDC / 反向代理 Header）

自托管部署可选择通过外部身份提供方（IdP）登录，本地账号密码能力**始终保留**。

**认证模式与开关**（默认 `local`，严格后向兼容）：

- `QDRUST_AUTH_MODE`：`local` | `hybrid` | `oidc`。`oidc` 模式下本地登录入口默认关闭；
  若想保留一个本地管理员应急入口，同时设 `QDRUST_LOCAL_LOGIN_ENABLED=true`。
- 外部提供方通过**独立开关**叠加：`QDRUST_OIDC_ENABLED` / `QDRUST_HEADER_AUTH_ENABLED`。

**OIDC（Authorization Code + PKCE）** —— 对接 authentik / authelia / keycloak / pocket-id：

```
QDRUST_AUTH_MODE=hybrid          # 或 oidc（纯 IdP，关本地入口）
QDRUST_OIDC_ENABLED=true
QDRUST_OIDC_PROVIDER_NAME=Authentik   # SSO 按钮上显示的名称（来自显式配置）
QDRUST_OIDC_ISSUER=https://auth.example.com/application/o/qdrust/
QDRUST_OIDC_CLIENT_ID=qdrust
QDRUST_OIDC_CLIENT_SECRET=...
# QDRUST_OIDC_REDIRECT_URI 留空则由服务端在请求时按反代前缀推导
QDRUST_OIDC_SCOPES="openid profile email"
QDRUST_OIDC_AUTO_CREATE_USERS=true    # 首登自动建档（哨兵口令，本地无法登录该外部账号）
QDRUST_OIDC_DEFAULT_ROLE=user         # 未命中 admin 组的建档角色
QDRUST_OIDC_ADMIN_GROUPS=qdrust-admins # 命中则首次建档为 admin
QDRUST_OIDC_GROUPS_CLAIM=groups       # ID token 中携带组成员身份的 claim 名（默认 groups）
# --- 可选：OIDC 单点登出（end-session） ---
# QDRUST_OIDC_LOGOUT_URL=https://auth.example.com/protocol/openid-connect/logout
#   # 用户点击登出时，清本地会话后顶层跳转到该 IdP 端点，把提供方会话也一并结束。
# QDRUST_OIDC_POST_LOGOUT_REDIRECT_URI=https://your.domain/qd/
#   # 可选：IdP 登出后跳回的应用地址。仅在 IdP 侧已登记该地址时才应配置，
#   # 否则多数 IdP 会拒绝未登记的回跳参数，用户将停留在 IdP 自带的登出完成页。
```

在 IdP 侧把回调地址登记为
`https://你的域名[/<base_path>]/api/v1/auth/oidc/callback`。

**纯 OIDC 模式（`QDRUST_AUTH_MODE=oidc`）的登录体验**：未登录用户访问页面时会**自动跳转**到
IdP 发起 SSO，不再先停留在一个多余的登录面板；仅在回调失败（带 `login_error`）或已登录时
保留 SSO 兜底面板，避免重定向循环。

OIDC 首登按 **ID token 里的 `groups` claim**（可通过 `QDRUST_OIDC_GROUPS_CLAIM` 换成
IdP 实际使用的 claim 名，如某些 Keycloak 映射后的 `roles`）解析角色：claim 命中
`QDRUST_OIDC_ADMIN_GROUPS` 中任一成员则建档为 `admin`，否则落 `QDRUST_OIDC_DEFAULT_ROLE`。
若 IdP 不在 ID token 内下发组（部分 IdP 只在 userinfo 返回），则保持 claim 缺失、统一落
`default_role`，再让管理员手动调整。

**新建档用户名的取值链**：按 `preferred_username` → `nickname` → `name` → `email`
本地部分依次取**第一个可读值**。这里会**跳过明显是不透明标识**的候选（如全十六进制/
UUID/纯数字的 `preferred_username`——部分 IdP 把内部 id 直接塞进该 claim），避免把
一串 hex 存成登录名，此时只要 `name`/`email` 可读就会取到它们。

**ID token 只给 `sub` 时的 UserInfo 回退**：少数 IdP（部分 authentik/Keycloak
配置、企业 IdP）签发的 ID token **只含 `sub`**，把 `preferred_username`/`name`/`email`
全部放在 **UserInfo 端点**。此时 qdrust 会用换来的 access_token 调 `userinfo_endpoint`
补取这些 claim 再走上面的取值链，从而拿到可读用户名而不是那串 `sub` id。该调用是
**尽力而为**：provider 未公布 userinfo、或调用失败，都只记日志、不阻断已验签的登录。

**最终兜底（整条链全空时）**：若 ID token 与 UserInfo 都没给出任何可读字段（IdP
只发一个不透明 `sub`），qdrust 不再把原始 `sub` 当用户名，而是从中派生一个短且稳定
的可读句柄 `user-<前 8 位>`（如 `user-23cc07f8`；派生前会去掉 uuid 连字符等分隔符，
同一 `sub` 恒定映射）。`sub` 本身仍单独存储，作为账号的稳定身份键。想彻底避免出现
兜底名，可在 IdP 侧给 ID token 补上 `preferred_username`/`name` 的 mapper（或确保
`profile`/`email` scope 生效）。只影响**新建档**，存量用户需手动改名或重新建档。

**二级目录部署下 SSO 登录**：登录判定**不再依赖** `/ready` 探针（后端把 `/ready`/`/health`
保留在根路径供 Docker HEALTHCHECK；经原样转发的 nginx 时，二级目录页面发起的根路径探针
本就不会命中）。会话有效性改为以 `/session` 成功为准，因此子路径部署时 SSO 登录不再被
探针失败误判为"未登录"。

**反向代理 Header 认证（forward-auth）** —— 对接 nginx `auth_request` / authelia forward-auth /
authentik proxy：反代在**每个请求**注入身份头，服务端仅在源 IP 属于可信代理时才信任：

```
QDRUST_HEADER_AUTH_ENABLED=true
QDRUST_HEADER_TRUSTED_PROXIES=127.0.0.1,10.0.0.1   # 必须显式配置
QDRUST_HEADER_TRUSTED_PROXY_REQUIRED=true           # 默认 true：无可信代理则拒绝启动
QDRUST_HEADER_USER_HEADER=Remote-User
QDRUST_HEADER_EMAIL_HEADER=Remote-Email
QDRUST_HEADER_GROUPS_HEADER=Remote-Groups
QDRUST_HEADER_GROUPS_SEPARATOR=,
QDRUST_HEADER_AUTO_CREATE_USERS=false               # 默认 false：仅已建档用户可登录
QDRUST_HEADER_DEFAULT_ROLE=user
QDRUST_HEADER_ADMIN_GROUPS=qdrust-admins
```

Header 认证**不每次建会话**：仅当无有效会话、或头身份与会话用户不一致时才会建/刷新
（身份漂移会撤销旧会话）。登出只清除 qdrust 自身会话。被隔离到 `/api/v1/auth/*` 之外的
普通 API 与 SPA 均由该中间件兜底。

反代需放行真实源 IP（Header 认证要读 `ConnectInfo` 源地址），示例 nginx（在 `location /`
里把已认证的用户身份注入并转发给 8923）：

```nginx
location / {
    proxy_pass http://127.0.0.1:8923;
    proxy_set_header Host $host;
    # authelia/authentik 校验通过后注入；务必去掉客户端可能伪造的同名头
    proxy_set_header Remote-User   $remote_user;     # 或取自 auth_request 变量
    proxy_set_header Remote-Email  $upstream_http_remote_email;
    proxy_set_header Remote-Groups $upstream_http_remote_groups;
}
```

> 注：Header 与 OIDC 两种外部认证的 `groups -> admin` 角色都在**首次建档时写死**，后续组变化
> 不会自动升/降权，需管理员显式调整。OIDC 从 ID token 的 `groups` claim（可配置）取组；
> 若 IdP 不把组放进 ID token（仅在 userinfo），该 claim 缺失，首登统一落 `default_role`。
> 组到角色的精细实时同步留待后续按需扩展。

详细的认证改造设计与风险控制见 [认证改造计划](EXTERNAL_IDP_PLAN.md)。
