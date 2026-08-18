# Phase 4 状态：In Progress

目标：将简单 ticker 调度器替换为可恢复、可取消、支持多 worker 竞争的租约状态机。

| 交付物 | 状态 | 说明 |
| --- | --- | --- |
| Run 状态机 | 完成 | pending/leased/running/succeeded/failed/cancelled |
| 原子领取与租约 | 完成 | lease owner、过期时间、续租、attempt |
| 崩溃恢复 | 完成 | 过期 leased/running 回到 pending |
| 立即运行与取消 | 完成 | owner-scoped API 与数据库取消标志 |
| 有界 worker | 完成 | 单 worker 循环、单任务 active run 唯一性 |
| 时区/DST | 未开始 | cron 时区与边界测试 |

Docker 与浏览器检查继续留在统一验收阶段。
