# ADR-0006：插件隔离与 api:// 映射

- 状态：Accepted
- 日期：2026-08-17

## 决策

插件使用版本化 manifest、JSON 输入输出和显式 capability。旧 `api://name/path` 经兼容路由解析到插件 ID/action。内置插件实现 Core trait；外部插件首版使用不经过 shell 的 JSON-lines 子进程，不加载 Rust 动态库。WASI Component 保留为后续 adapter，不作为 P0 依赖。

## 安全约束

- 子进程直接传入可执行文件和参数，禁止拼接 shell 命令。
- 超时或 Future 取消时终止子进程；限制 stdin/stdout/stderr 和日志大小。
- 未声明 capability 的调用在宿主层拒绝。操作系统级沙箱不可用时，禁止安装不受信任插件。
- 插件签名、管理员授权和调用审计在 Server 阶段实现。

## 当前证据

Core 已实现 API v1 manifest、capability、`api://<plugin>/<action>` 解析、注册表、重复注册保护、调用超时和统一响应。`api://util/delay` 已通过 HAR 状态断言与变量提取测试；JSON-lines echo 子进程已在 Windows 通过真实进程测试。Linux Docker 是 Phase 1 容器门禁。
