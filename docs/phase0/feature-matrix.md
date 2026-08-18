# QD2 功能矩阵

状态含义：`基础` 已有可运行骨架，`未开始` 尚未实现，`部分` 仅完成一部分契约。

| 模块 | 能力 | 优先级 | 当前状态 |
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

完成标准不是页面或接口存在，而是领域测试、权限测试和主要工作流测试同时通过。
