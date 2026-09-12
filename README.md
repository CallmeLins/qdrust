# qdrust

> 一个用 Rust + Vue 3 重写的 **QD 兼容 HTTP 定时任务自动执行框架**，基于 HAR 编辑器与 Axum 服务。

[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-linux%20%2F%20amd64%20%2F%20arm64-blue)](https://github.com/)

qdrust 是按 [QD](https://github.com/qd-today/qd)（HTTP 请求定时任务自动执行框架）路线重新设计的 Rust 实现：解析并执行旧 QD 的 HAR 模板，提供命令行工具、REST API、定时调度与 WebUI。它是一个**全新项目**，不导入旧 QD 数据库、不复用旧登录 Cookie、不承诺兼容旧 URL / 旧 API / 任意 Python 动态表达式——但对核心兼容契约（旧 QD HAR）做了完整对齐，并补齐了 Jinja2 表达式与 `api://util/*` 内置工具。

---

## 介绍

### 它解决什么问题

把浏览器里抓到的 HTTP 请求（HAR）存成模板，按 cron 或固定间隔自动执行，用于签到、监控、API 轮询等定时任务；执行结果可通过 11 种渠道推送通知。

### 核心特性

- **旧 QD HAR 兼容**：导入、校验、保留原始 HAR JSON，并重放执行。
- **可视化 HAR 编辑器**：在 WebUI 里直接编辑请求、变量与断言，无需手写 JSON。
- **双后端数据库**：SQLite（开箱即用）与 MySQL（按 `DATABASE_URL` 自动选择）。
- **完整通知体系**：Webhook + Email + 自定义 HTTP + 8 种推送渠道，共 **11 种**；支持批量绑定、失败次数阈值、仅自动执行触发，以及自定义标题 / 正文模板（见 [推送 / 通知](docs/notifications.md)）。
- **模板表达式**：26 个 Jinja2 过滤器 + 38 个表达式函数，以及 `api://util/*` 内置工具（时间 / 编码 / 哈希 / 正则 / JSON / RSA / OCR，见 [模板表达式与内置工具](docs/expressions.md)）。
- **无头浏览器插件**（可选）：`api://browser/*` 通过 CDP 驱动远程无头浏览器，补齐"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 做不了的一步。支持跨步骤存活的会话复用与 `type`/`click` DOM 操作，结果可提取成变量回填到后续请求（见 [浏览器插件](docs/browser-plugin.md)）。
- **任务调度**：分组、批量操作、可视化调度器（含随机延迟）、模板变量预填。任务以**模板**为请求源（多页模板常混合 GET/POST），任务级不携带请求方法 / URL / 请求头 / 请求体。
- **运行可观测**：QD 风格运行日志、按任务查看运行历史、WebSocket 实时步骤流。
- **安全与多租户**：开放注册 / 忘记密码 / 邮箱验证 / CSRF 轮换；所有用户资源均做服务端归属校验。
- **运维友好**：Docker 镜像（amd64 / arm64）、管理员备份恢复、运行时配置热更新、可选 Redis 会话缓存。

### 与 QD 的刻意差异

- **单进程异步模型**（`axum::serve` + Tokio），而非 Tornado 多进程；横向扩展靠 Docker 副本 + 反向代理。
- **非持久化本地队列**基于 SQLite 事务（`claim_run` + 租约恢复），保证单实例正确性与多实例不重复执行。

---

## 组成

- `crates/qdrust-core`：旧 QD HAR 解析、编译、变量求值和 HTTP 执行核心。
- `crates/qdrust-cli`：无需启动服务即可校验和执行 HAR。
- `crates/qdrust-server`：基于 Axum、SQLx 和 SQLite/MySQL 的 API、认证、调度与运行管理。
- `webui`：基于 Vue 3、TypeScript 和 Vite 的 WebUI。

服务端覆盖：用户认证与会话（开放注册、忘记/重置密码、邮箱验证、CSRF 轮换）、管理员 API（用户管理、站点设置、日志清理、备份/恢复）、模板（搜索/分组/分页、可视化 HAR 编辑器、公共发布审批 PushRequest、订阅仓库自动导入）、任务（分组/批量操作、可视化调度器含随机延迟、模板变量预填）、运行记录与步骤（QD 风格运行日志、运行历史与清空）、WebSocket 实时步骤流（运行 + 订阅进度）、租约恢复、插件系统、通知、记事本、可选 Redis 会话缓存、GA 注入、运行时配置热更新以及 OpenAPI。所有用户资源均在服务端执行归属校验。

---

## 快速开始

### 用 Docker（推荐）

```powershell
docker run -d --name qdrust -p 8923:8923 -v qdrust-data:/data ghcr.io/callmelins/qdrust:latest
```

打开 `http://localhost:8923/`，按引导创建初始管理员账号即可。生产环境建议改用 Compose（命名卷 + 健康检查 + 开机自启），完整配置见 [部署](docs/deployment.md)。

### 从源码运行

```powershell
npm --prefix webui ci --cache .npm-cache
npm --prefix webui run generate:api
npm --prefix webui run build
Copy-Item .env.example .env
cargo run -p qdrust-server
```

服务默认监听 `http://localhost:8923`，运行数据默认写入 `data/qd.db`。前后端分开开发时再起一个 `npm --prefix webui run dev`（Vite 在 `5173`，已把 API 代理到 Rust 服务）。

---

## 文档

| 文档 | 内容 |
|---|---|
| [部署](docs/deployment.md) | 环境要求、本地开发、Docker / Compose、反向代理到二级目录、环境变量、数据库、升级与回滚 |
| [账号与第三方登录](docs/authentication.md) | 初始管理员、开放注册，OIDC（Authorization Code + PKCE）与反向代理 Header 认证 |
| [使用](docs/usage.md) | 导入 QD HAR、模板变量怎么算、创建与运行任务、HAR 编辑器、调度与时区、运行日志、CLI |
| [推送 / 通知](docs/notifications.md) | 11 种渠道的配置项、批量绑定、失败阈值、自定义标题/正文模板 |
| [浏览器插件](docs/browser-plugin.md) | `api://browser/*` 的 action 一览、部署启用、一次性与会话用法、生命周期 |
| [模板表达式与内置工具](docs/expressions.md) | Jinja2 过滤器 / 函数清单与 `api://util/*` 工具表 |
| [常见问题](docs/faq.md) | 部署、调度、通知、认证等高频问题 |
| [原生模板 schema v1](docs/template-schema-v1.md) | `native_v1` 模板的 JSON 结构与安全限制 |
| [API 错误码](docs/api-error-codes.md) / [OpenAPI 契约](docs/openapi-v1.json) | 服务端错误码约定与完整 API 定义（运行时可取 `GET /api/v1/openapi.json`） |
| [运维手册](docs/operations.md) | SQLite 备份 / 恢复、升级与回滚操作规范 |
| [发布检查清单](docs/release-checklist.md) | 本地门禁、浏览器验收与发布门禁 |
| [架构决策记录](docs/adr/) | ADR-0001 ~ 0008：workspace、QD HAR 契约、数据库、运行时状态、表达式、插件、WebUI、SSRF/DNS |
| [迁移计划](docs/migration-plan.md) | 从 QD 迁移的阶段范围、决策与风险控制 |
| [认证改造设计](docs/EXTERNAL_IDP_PLAN.md) | OIDC / Header 认证的详细设计 |
| [威胁模型](docs/threat-model.md) / [功能矩阵](docs/feature-matrix.md) / [兼容清单](docs/compatibility-inventory.md) | 安全边界、QD 功能对照与兼容范围 |

---

## 兼容范围

旧 QD HAR 是核心兼容契约，兼容样本位于 `tests/fixtures`。WebUI 可导入、校验并保留原始 HAR JSON。

qdrust 不导入旧 QD 数据库，不复用旧登录 Cookie，也不承诺兼容旧 URL、旧 API 或任意 Python 动态表达式。详细决策、阶段范围和风险控制见 [迁移计划](docs/migration-plan.md)。

---

## 项目状态

Phase 0-8 代码与容器已实现完成（Phase 8 状态为 Implementation Complete）；发布前仍待完成浏览器人工验收与首个镜像（amd64 / arm64）发布检查。详见 [发布检查清单](docs/release-checklist.md)。

---

## 验证

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
npm --prefix webui run lint
npm --prefix webui run test
npm --prefix webui run build
```

完整发布验收项目见 [发布检查清单](docs/release-checklist.md)。

---

## 许可与致谢

### 许可

[MIT 许可证](LICENSE)。Copyright (c) 2026 CallmeLins。

### 致谢

- 本项目在设计与协议层面参考了 [QD（qd-today/qd）](https://github.com/qd-today/qd)——一个优秀的 HTTP 定时任务自动执行框架。qdrust 是其**独立、干净重写的 Rust 实现**：不继承其代码、不导入其数据库、不复用其 Cookie，仅对齐核心 HAR 兼容契约与 `api://util` 工具语义。
- 感谢 QD 社区与所有贡献者提供的思路与协议参考。
