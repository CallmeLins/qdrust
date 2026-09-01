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

执行器接线（2026-09-01）：`QdExecutor::register_plugin` 把外部插件注入注册表，插件响应与 HTTP 响应走同一条断言/变量提取链路；`PluginRegistry` 暴露 `ids()`，失败信息带上 plugin id 与 action，并按调用记录耗时（target `qdrust_core::plugin`，需在 `RUST_LOG` 中开启 `qdrust_core=info`）。Server 调度器在每次运行时按任务属主加载 enabled 插件并以 `plugin-<id>` 注册（与 `/api/v1/plugins/{id}/invoke` 同址），命令按空白拆成 program + args；插件缺失或命令非法只告警不中断运行，注册表为空时与接线前行为完全一致。

Capability 强制（2026-09-01）：外部插件在响应信封中可选上报 `capabilities_used`（子进程协议增量字段，老插件不上报即视为未使用，api_version 仍为 1）。`SubprocessPlugin::call` 是两条调用路径（模板执行经注册表、`/api/v1/plugins/{id}/invoke`）的共同宿主侧关口，校验"上报 ⊆ 声明"，越界整次调用拒绝并报 `used undeclared capability`。声明存于插件 `config` JSON 的 `capabilities` 数组，store 在保存时校验名称合法（拼写错误即时失败）。上报是协作式的——恶意插件可瞒报，操作系统级沙箱仍是真实边界；本机制让诚实插件被约束在自己的声明内，越界行为在运行期高声失败。
