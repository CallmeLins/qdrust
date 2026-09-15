# 参考

> 返回 [README](../README.md)

API 错误码约定、与旧 QD 的兼容范围，以及立项期功能矩阵。

## API 错误码

API responses use the stable `code` field as their message key. Clients translate that key locally and may fall back to the server-provided English `message`. Adding a locale must not change HTTP status codes or error codes.

| Code | HTTP status | Default message | Meaning |
| --- | --- | --- | --- |
| `api_endpoint_not_found` | 404 | API endpoint not found | The requested API route does not exist |
| `task_not_found` | 404 | Task not found | The requested task does not exist |
| `run_not_found` | 404 | Run not found | The requested run does not exist or is not owned by the user |
| `template_not_found` | 404 | Template not found | The requested template does not exist |
| `validation_error` | 422 | Request-specific | JSON or domain validation failed |
| `internal_error` | 500 | An internal error occurred | An unexpected internal failure occurred |
| `authentication_required` | 401 | Authentication required | Session is absent, expired, revoked, or disabled |
| `invalid_credentials` | 401 | Invalid username or password | Login credentials did not validate |
| `csrf_validation_failed` | 403 | CSRF validation failed | CSRF cookie/header/session binding did not validate |
| `bootstrap_already_completed` | 409 | Initial administrator already exists | First-user initialization cannot run again |
| `login_rate_limited` | 429 | Too many login attempts | Login attempts for this username are temporarily throttled |

Every error response also includes an opaque `request_id` and `field_errors`. Internal error details are only written to server logs.

## 与旧 QD 的兼容范围

来源：旧 QD `libs/fetcher.py`、HAR 编辑器插入项及 about 文档。该清单只代表仓库内可确认能力，不能替代真实 HAR 样本。

### 控制与表达式

- `if/else/endif`
- `for <name> in <variable>`
- `for <name> in list(...)`
- `for <name> in range(...)`
- `while <condition>/endwhile`
- 变量真假、比较、`and/or/not`
- `int(loop_index0)` 及循环变量 `loop_index*`、`loop_first/last/length/depth*`

### api:// 内置路由

- `util/unicode`
- `util/urldecode`
- `util/gb2312`
- `util/regex`
- `util/string/replace`
- `util/timestamp`
- `util/rsa`
- `util/delay`
- `util/dddd/ocr`
- `util/dddd/det`
- `util/dddd/slide`

OCR/验证码能力允许作为可选插件，但导入诊断必须能识别其路由。未安装插件时执行应返回稳定的 `plugin_unavailable` 错误。

## 立项期功能矩阵

> 下表为 Phase 0 立项时编制的规划快照，状态列已滞后于实现（表达式、插件、通知、公共模板等均已落地）；当前整体状态见 [README · 项目状态](../README.md#项目状态)。完成标准不是页面或接口存在，而是领域测试、权限测试和主要工作流测试同时通过。

状态含义：`基础` 已有可运行骨架，`未开始` 尚未实现，`部分` 仅完成一部分契约。

| 模块 | 能力 | 优先级 | 立项期状态 |
| --- | --- | --- | --- |
| Core | QD HAR 1.2 与扩展字段无损解析 | P0 | 基础 |
| Core | QD 控制流编译 | P0 | 基础 |
| Core | HTTP、Cookie、Jinja、断言、提取 | P0 | 部分 |
| Core | QD 表达式兼容 | P0 | 未开始 |
| Core | `api://` 插件调用 | P0 | 未开始 |
| CLI | validate、inspect | P0 | 基础 |
| CLI | run、变量输入、JSON 输出 | P0 | 部分 |
| Server | RESTful 模板/任务接口 | P0 | 部分 |
| Server | 用户、Session、CSRF、权限隔离 | P0 | 未开始 |
| Server | 调度、立即运行、取消、重试、lease | P0 | 部分 |
| Server | 运行与步骤日志、WebSocket | P0 | 部分 |
| Server | 公共模板 | P1 | 未开始 |
| Server | 插件注册、配置、权限与审计 | P1 | 未开始 |
| Server | 消息推送与动作 | P1 | 未开始 |
| Server | 记事本 | P1 | 未开始 |
| Web | 登录、概览、模板、任务、运行记录 | P0 | 部分 |
| Web | 公共模板、插件、推送、记事本 | P1 | 未开始 |
| 全局 | 中英文国际化 | P0 | 未开始 |
| 交付 | SQLite Docker 单机部署 | P0 | 部分 |
| 交付 | MySQL、Redis、多实例 | P1 | 未开始 |
