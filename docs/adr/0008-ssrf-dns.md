# ADR-0008：SSRF 与 DNS 连接绑定

- 状态：Accepted
- 日期：2026-08-17

## 决策

默认只允许公共 HTTP/HTTPS 目标。每次请求先解析 DNS，拒绝 private、loopback、link-local、multicast、unspecified、云元数据和特殊用途地址，再把选中的已校验地址固定到该次 Reqwest 连接，避免校验后重新解析产生 DNS rebinding 时间窗。

旧 QD 默认不校验证书且可访问内网的行为不作为安全默认值。管理员可以分别开启私网访问和无效证书，但配置必须审计并在 UI 标为高风险。重定向默认关闭；未来启用时每一跳重新执行同一策略。

## 实现状态

Core 已实现安全默认值、请求前地址分类和 Reqwest 连接级地址固定。测试使用不可解析域名并将其固定到本地 mock 地址，证明连接不再二次查询系统 DNS。后续若启用重定向，每一跳仍须重复该流程。

两个放宽开关均已由 `qdrust-server` 接到运行时设置：管理员在管理页「站点设置」里**分别**开启 `security.allow_private_network` 与 `security.allow_invalid_certificates`，也可用 `QDRUST_ALLOW_PRIVATE_NETWORK` / `QDRUST_ALLOW_INVALID_CERTIFICATES` 在部署时预设；WebUI 各配一条高风险说明，每次修改写一条 `admin.setting_changed` 审计记录，保存后下一次运行生效。两者互相独立，非布尔取值一律忽略（失败方向是保持关闭）。回归测试用本地回环服务与自签证书 HTTPS 服务，覆盖「只开一个开关」的两种情况。
