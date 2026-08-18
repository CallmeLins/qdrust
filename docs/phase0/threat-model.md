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
