# 威胁模型

## 资产与信任边界

- 资产：用户密码、Session、HAR 中的 Cookie/token、任务变量、运行日志、插件配置和通知凭证。
- 不可信输入：浏览器请求、导入 HAR、目标 HTTP 响应、表达式、插件包、Webhook 内容和代理配置。
- 边界：Browser/API、Server/数据库、Worker/目标网络、Core/插件进程以及容器/宿主机。

## P0 威胁与控制

| 威胁 | 必须控制 | 验证方式 |
| --- | --- | --- |
| SSRF/DNS rebinding | 禁止私网、loopback、链路本地和元数据地址；每次解析及重定向复检 | DNS/重定向集成测试 |
| 表达式逃逸 | 无文件、网络、环境变量、进程、导入和反射；步数/内存/deadline 限制 | 语法拒绝与超限测试 |
| 插件越权 | 独立隔离、显式 capability、默认无权限、调用审计 | 恶意插件 fixture |
| 凭证泄漏 | header/Cookie/变量脱敏；错误和 trace 不记录原文 | 日志快照测试 |
| 横向越权 | repository 查询必须包含 owner；管理员走独立 service | 双用户资源测试 |
| CSRF/Session 劫持 | 服务端 Session、SameSite/HttpOnly/Secure、CSRF token、Origin 校验 | API 安全测试 |
| 资源耗尽 | 请求数、循环、响应体、日志、并发和总时长上限 | 边界与取消测试 |
| 重复副作用 | run/通知幂等 ID、原子 lease、重试策略 | 并发及故障测试 |
| HAR/XSS | HAR 与响应只按文本展示，不执行 HTML | WebUI XSS 测试 |
| 供应链 | lockfile、cargo deny、npm audit、SBOM、镜像签名 | CI 门禁 |

任何允许访问内网、关闭 TLS 校验或授予插件权限的配置都必须由管理员显式开启，并在 UI/API 中标为高风险。旧 QD 兼容行为不能自动扩大这些权限。

当前 Core 已默认启用 TLS 校验，在请求前阻止私网/loopback/链路本地地址，并将已校验地址固定到实际 Reqwest 连接。重定向默认关闭；未来启用时必须逐跳复检。

ADR-0008 承诺的两个开关都已落地：管理员在「站点设置」里**分别**开启 `security.allow_private_network` 与 `security.allow_invalid_certificates`（或部署时设 `QDRUST_ALLOW_PRIVATE_NETWORK=true` / `QDRUST_ALLOW_INVALID_CERTIFICATES=true`），WebUI 各配一条高风险说明，每次修改写一条 `admin.setting_changed` 审计记录，保存后下一次运行即生效。私网开关只放宽「地址是否公网」这一项判断——解析校验、已校验地址固定、超时与重定向策略都不变；证书开关只把 Reqwest 的 `danger_accept_invalid_certs` 打开，不触及地址校验。两者互相独立、可分别开启；非布尔取值一律忽略，失败方向都是保持关闭。

这道闸门现在覆盖**全部出站请求**，共五条：模板运行、不绑模板的任务、通知渠道与渠道测试、模板订阅抓取，以及模板里 `api://util/dddd/...` 的转发。前四条走 `qdrust-server` 的 `OutboundHttp`，最终都落到 `qdrust_core::executor::guarded_client_for_url`，服务端不再持有裸 `reqwest::Client`；第五条在 `qdrust_core::plugin` 里，由执行器在每次运行构造 `UtilityPlugin` 时注入当次政策。不受约束的只有**部署者写死的固定端点**：OIDC 身份提供方（配置文件里的 URL）、SMTP 投递、浏览器插件的 CDP 端点（`QDRUST_BROWSER_URL`，官方示例即 `http://localhost:9222` / `ws://browserless:3000`），以及 Redis / 数据库这类非 HTTP 连接——它们的目标由管理员指定，模板改不动（浏览器插件的 `_browser_url` 覆盖在 `ensure_override_matches` 处被拒）。

第五条值得单独记一笔，因为它是**审计漏掉过一次**的那类。它不在服务端，所以服务端「crate 内不得出现 `reqwest::Client`」的结构断言扫不到它；core 里那条同构断言只读 `executor.rs`，也扫不到 `plugin.rs`——一条路径正好落在两道断言的缝上。而它的目标由模板的 `_server` 参数决定，是用户可控的，裸 client 还默认跟随最多 10 跳重定向，比其余几条都松。两道断言各自都藏着一个「我以为已经全覆盖了」的范围假设，所以这里的结论不是「再人工回忆一遍还有哪些路径」，而是把 `plugin.rs` 一并纳入断言：范围由代码结构强制，而不是由记忆维持。

此前只有模板受防护，其余四条路径——包括最普通的「用 URL 建个任务」——可以直连内网，使「没开开关就谁都打不到内网」这个判断对它们不成立。开关按请求读实时设置，不在启动时快照；执行器是每次运行新建的，所以它持有的政策快照即该次运行的口径。
