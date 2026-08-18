# Phase 0 状态：Complete

Phase 0 的目标是冻结范围、模块边界和高风险技术决策，不要求完成业务功能。

| 交付物 | 状态 | 说明 |
| --- | --- | --- |
| QD2 功能矩阵 | 完成 | 见 `feature-matrix.md` |
| Core/CLI/Server/Web 边界 | 完成 | ADR-0001 Accepted，workspace 已落地 |
| 新模板 schema v1 | 完成 | 见 `../template-schema-v1.md` |
| QD HAR 外部兼容边界 | 完成 | ADR-0002 Accepted，解析、无损存储和控制树已有测试 |
| 数据库策略 | 完成 | ADR-0003 Accepted，当前先交付 SQLite |
| Session、调度租约、日志保留 | 完成 | ADR-0004 Accepted |
| 威胁模型 | 完成 | 见 `threat-model.md` |
| 表达式实现选择 | 完成 | ADR-0005 Accepted；真实 HAR 语料是 Phase 3 验收 |
| 插件隔离方式 | 完成 | ADR-0006 Accepted；JSON-lines 子进程 PoC 已通过 |
| UI 组件库 | 完成 | ADR-0007 Accepted，选择 Element Plus + Lucide |
| SSRF/DNS 策略 | 完成 | ADR-0008 Accepted，连接级绑定在 HTTP 引擎阶段实现 |

Phase 0 已完成。所有范围、模块边界和高风险实现方向均已冻结并有 Accepted ADR。尚未完成的真实 HAR 兼容率、连接级 DNS 固定、插件授权和完整 UI 属于后续阶段的实现与验收，不是未决架构问题。
