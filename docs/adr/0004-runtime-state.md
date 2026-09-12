# ADR-0004：Session、调度 lease 与日志保留

- 状态：Accepted
- 日期：2026-08-17

## 决策

- Session 使用 `tower-sessions`，SQLite 部署使用数据库 store，Redis 是多实例适配器；Cookie 只保存随机 ID。
- 调度采用数据库原子领取与有期限 lease；状态为 pending/leased/running/succeeded/failed/cancelled。
- 单机和多实例使用同一状态机，启动时恢复过期 lease。
- run/step 日志默认保留 30 天，按时间和批次清理；上限和保留期可配置。

## 后果

当前简单 ticker 只是骨架，不能视为该 ADR 已实现。进入生产验收前必须覆盖并发领取、进程崩溃和恢复测试。
