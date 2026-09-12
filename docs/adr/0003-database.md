# ADR-0003：SQLx 与数据库交付顺序

- 状态：Accepted
- 日期：2026-08-17

## 决策

使用 SQLx。P0 先完整交付 SQLite WAL 单机模式；repository 边界稳定后增加 MySQL 契约测试。migration 只能前进，不修改已发布 migration。连接池、busy timeout 和生命周期全部配置化。

## 原因

新项目没有旧库迁移义务。先保证单机闭环能降低同时维护双 SQL 方言造成的早期成本，同时保留 MySQL P1 目标。
