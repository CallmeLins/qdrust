# 推送 / 通知

> 返回 [README](../README.md)

任务成功或失败时，可通过通知渠道推送提醒。系统共支持 **11 种**渠道，在「通知」页新建渠道并绑定到任务即可。

| 渠道 | 所需配置 | 说明 |
|---|---|---|
| **Webhook** | `url` | 向该 URL `POST` JSON（含 `event` / `task_name` / `run_id` / `http_status` / `error`），仅接受 HTTPS |
| **自定义 HTTP** | `url`, `method`(默认 `POST`), `headers`(JSON, 可选), `body`(可选模板) | 完全自定义的请求，可指向自建的 ntfy、Gotify 等；**`http` 与 `https` 均可**（便于内网服务） |
| **Email** | `to` | 经 SMTP 发送，需先配置 `QDRUST_SMTP_*` 环境变量 |
| **Bark** | `url` | iOS 推送，设备地址形如 `https://api.day.app/你的Key` |
| **Server 酱** | `sendkey` | 微信推送 SendKey |
| **Telegram** | `token`, `chat_id`, `host`(可选) | 机器人 token 与 chat id；`host` 可指定自建 API 域名 |
| **钉钉机器人** | `access_token` | 自定义机器人 access_token |
| **WxPusher** | `app_token`, `uid` | 应用 token 与目标 UID |
| **WxPusher SPT** | `spt` | 主题推送码（逗号分隔可多个） |
| **企业微信应用** | `corpid`, `secret`, `agentid`, `to_user`(默认 `@all`), `proxy`(可选) | 自建应用消息；`proxy` 可指定 API 代理 / 自定义域名 |
| **企业微信群机器人** | `key` | 群机器人 webhook key |

通知标题与正文由调度器统一渲染，并在 Email、自定义 HTTP 与推送渠道间共享；单个渠道投递失败只记录日志，不会中断任务执行。

**通知动作**（把渠道绑定到任务）还支持：

- **批量绑定**：一次为多个任务创建同一个动作，不必逐个任务重复配置。
- **失败次数阈值**：连续失败达到阈值后才通知（统计「上次成功之后」的失败次数），过滤偶发网络抖动。
- **仅自动执行时通知**：手动点击「立即运行」不触发，只对定时 / 随机延迟 / 重试触发的运行生效。
- **自定义标题与正文模板**：可用变量 `{event}` `{task_id}` `{task}` `{run_id}` `{status}` `{error}` `{log}` `{t}` —— `{log}` 是 QD 模板的运行日志，`{t}` 是运行完成时间（按任务时区渲染，未设置时区时用 UTC）；未识别的 `{...}` 占位符会原样保留。
