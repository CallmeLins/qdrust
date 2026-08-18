# Phase 3 状态：Complete（本机验收）

目标：把 QD HAR 兼容执行与新模板执行统一到可取消、可限额的 HTTP 执行边界。

| 交付物 | 状态 | 说明 |
| --- | --- | --- |
| QD HAR executor | 基础完成 | 请求、Cookie、断言、提取、控制流、插件、SSRF/DNS 和 TLS 安全策略已存在 |
| ExecutionContext | 完成 | 变量、请求预算和循环预算已实现；请求/循环限制可由 ExecutorOptions 配置并有回归测试 |
| Native template executor | 基础完成 | Request、Query/模板渲染、JSON/Text/Header/Status 提取、If、ForEach、Delay 已实现并有 mock 测试 |
| 取消与 deadline | 完成 | `execute_with_deadline` 和 Core 内置 CancellationToken 可中止整个执行树；Server 取消状态机属于 Phase 4 |
| 响应/日志限制 | 完成 | 响应体、请求/循环、错误文本均有上限；run_steps 不持久化响应正文 |
| Mock/集成测试 | 基础完成 | mock server 覆盖请求、Cookie、提取、SSRF、DNS 固定、deadline 和取消 |
| Server run 入口 | 完成 | Task 可关联 native/QD Template，Scheduler 调用 Core executor 并记录 `run_steps`，失败执行也写入步骤记录 |

本阶段不会复制一套 HTTP SSRF 实现；native executor 复用 Core 已验证的客户端安全策略。Phase 3 已通过本机测试、Clippy、OpenAPI 和 WebUI 构建验收；Docker/浏览器检查留到统一验收。实际用户取消状态机和调度恢复进入 Phase 4。
