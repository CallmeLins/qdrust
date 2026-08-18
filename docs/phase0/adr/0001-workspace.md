# ADR-0001：Workspace 与依赖方向

- 状态：Accepted
- 日期：2026-08-17

## 决策

使用 `qdrust-core`、`qdrust-cli`、`qdrust-server` 和 `webui` 四个产品边界。Core 不依赖 Axum、SQLx 或 Server；CLI 与 Server 复用同一 Core。后续仅在复杂度确实需要时从 Server 拆出 db/scheduler crate。

## 原因

这对应 QD2 的 Core/Cli/Server/Web 路线，同时避免过早拆出大量空 crate。该依赖方向已经由 Cargo workspace 编译验证。
