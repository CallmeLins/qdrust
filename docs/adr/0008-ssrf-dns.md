# ADR-0008：SSRF 与 DNS 连接绑定

- 状态：Accepted
- 日期：2026-08-17

## 决策

默认只允许公共 HTTP/HTTPS 目标。每次请求先解析 DNS，拒绝 private、loopback、link-local、multicast、unspecified、云元数据和特殊用途地址，再把选中的已校验地址固定到该次 Reqwest 连接，避免校验后重新解析产生 DNS rebinding 时间窗。

旧 QD 默认不校验证书且可访问内网的行为不作为安全默认值。管理员可以分别开启私网访问和无效证书，但配置必须审计并在 UI 标为高风险。重定向默认关闭；未来启用时每一跳重新执行同一策略。

## 实现状态

Core 已实现安全默认值、请求前地址分类和 Reqwest 连接级地址固定。测试使用不可解析域名并将其固定到本地 mock 地址，证明连接不再二次查询系统 DNS。后续若启用重定向，每一跳仍须重复该流程。

该固定有一个已知边界：它只在本进程自己建立连接时生效。部署环境若设置了 `HTTP_PROXY` / `HTTPS_PROXY`（或策略里显式配了 proxy），请求会交给代理，`host -> address` 映射不再参与，DNS 落到代理解析，上面那个 rebinding 时间窗随之重开。地址分类仍在本进程完成——两个开关照旧生效——失去的只是「校验之后不再重新解析」这一层，这与「显式配了 proxy 就不固定主机」是同一条设计。带代理的环境因此不适合跑这组测试：变异脚本会先清掉代理变量再执行，否则固定相关的用例会因为请求根本没走本进程连接而失败。

两个放宽开关均已由 `qdrust-server` 接到运行时设置：管理员在管理页「站点设置」里**分别**开启 `security.allow_private_network` 与 `security.allow_invalid_certificates`，也可用 `QDRUST_ALLOW_PRIVATE_NETWORK` / `QDRUST_ALLOW_INVALID_CERTIFICATES` 在部署时预设；WebUI 各配一条高风险说明，每次修改写一条 `admin.setting_changed` 审计记录，保存后下一次运行生效。两者互相独立，非布尔取值一律忽略（失败方向是保持关闭）。回归测试用本地回环服务与自签证书 HTTPS 服务，覆盖「只开一个开关」的两种情况。

策略的覆盖面是分两步补齐的。此前防护只落在模板运行上：不绑模板的任务（直接用 URL 建的那种）、通知渠道、渠道测试、模板订阅抓取各自用另一个不带防护的 `reqwest::Client`，即「通知能打内网、模板不能」——而那条最普通的路径恰恰完全没有防护。第一步让服务端不再持有裸 client：那四条路径都通过 `qdrust_core::executor` 里同一个受防护的 client 构造（`guarded_client_for_url`），开关按请求读实时设置。

第二步补的是第五条——`qdrust_core` 内建 `util` 插件里 `api://util/dddd/...` 的转发。它不在服务端，所以服务端那条「crate 内不得出现 `reqwest::Client`」的结构断言看不到它；core 里那条只读 `executor.rs`，同样看不到 `plugin.rs`。一条路径正好落在两道断言的缝上，而且性质更差：目标来自模板的 `_server` 参数（**用户可控**），裸 client 还默认跟随最多 10 跳重定向。现在 `UtilityPlugin` 在构造时取得本次运行的政策（`QdExecutor` 每次运行新建，因此快照即该次运行的口径），转发与其余请求共用同一道闸门。

这两步都是**破坏性**的：原先依赖内网的通知渠道、订阅源与自建 DdddOCR 服务需要开启开关，且按本 ADR 的要求一律不跟随重定向（逐跳复检仍是未实现项，见上文）。OIDC 身份提供方与 SMTP 投递不在范围内。
