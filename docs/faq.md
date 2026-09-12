# 常见问题（FAQ）

> 返回 [README](../README.md)

**Q：HTTPS 部署后登录会话不生效 / 一直被登出？**
A：生产环境必须把 `COOKIE_SECURE` 设为 `true`，否则安全 Cookie 不会被浏览器接受。

**Q：邮件相关功能（重置密码、邮箱验证、Email 渠道）用不了？**
A：需要先配置 `QDRUST_SMTP_*` 系列环境变量并确认 `QDRUST_BASE_URL` 指向对外可访问的地址。若启用了 `REQUIRE_EMAIL_VERIFICATION`，新用户必须验证邮箱才能登录。

**Q：数据库升级后还能回退到旧版本吗？**
A：迁移是**前向 only** 的。仅回退镜像而不恢复数据库不被支持；回退请同时用备份恢复数据库，再启动旧镜像 digest。

**Q：CLI 报网络被拒绝（私网 / localhost / 证书）？**
A：这是刻意的安全策略——CLI 默认拒绝私网、localhost 和无效 TLS 证书，不像旧 QD 那样宽松。服务端执行不受此限制。

**Q：调度好像没按我设的时间跑？**
A：`SCHEDULER_INTERVAL_SECONDS`（默认 15 秒）是调度器轮询间隔，任务触发精度受其影响；如需更精确请相应调小（会增加数据库轮询压力）。

**Q：运行记录越来越多，能自动清理吗？**
A：设置 `LOG_RETENTION_DAYS`（默认 `0` 永久保留），系统每小时清理超出天数的已完成运行。

**Q：多实例 / 横向扩展怎么弄？**
A：单实例基于 SQLite 事务 + 租约保证不重复执行；多实例共享数据库时同样由租约机制保证。推荐 Docker 副本 + 反向代理（`docker compose up --scale qdrust=N`）。Redis 仅用于会话缓存加速，非必需。

**Q：新建任务时为什么不用填请求方法 / URL / 请求头 / 请求体？**
A：因为任务的请求完全由**模板**决定——调度器执行时直接重放模板（多页模板常混合 GET/POST），任务自带的这几个字段从来不会被使用。所以新建任务必须先选模板，请求方法 / URL 只在任务列表里作为模板首条请求的镜像展示。唯一例外是**早于该改动的存量任务**（没有绑定模板、自带一条请求），编辑它们时模板下拉会保留「（无模板）」选项，其原有请求也不会被改动。

**Q：插件 / `api://util` 工具不全？**
A：`api://util/*` 已对齐 QD 的核心工具（时间 / 编码 / 哈希 / 正则 / JSON / RSA / GB2312 / 字符串替换 / OCR）， toolbox 与 notepad 属 Web 工具箱页面，不在模板 API 范围内，未移植。

**Q：站点要 JS 生成签名 / 过验证码，纯 HAR 跑不了怎么办？**
A：用 `api://browser/*` 无头浏览器插件。配置 `QDRUST_BROWSER_URL` 后启用，模板里插入 `api://browser/eval`（执行 JS 取 token/签名）、`api://browser/content`（抓渲染后 HTML）、或 `api://browser/screenshot`（截图），再用 `extract_variables` 把结果提成变量回填。详见 [浏览器插件（无头浏览器签到）](browser-plugin.md)。

**Q：浏览器插件能自动跑多步页面交互（点按钮、填表单）吗？**
A：可以。用 `start` 开一个会话，对同一 `session` 跨步骤执行 `type`（输入）/ `click`（点击）/ `content`（取渲染后状态），`extract_variables` 提出会话 id 后用 `{{var}}` 复用于后续步骤；流程结束用 `end` 关会话。会话在 server 内存中跨调用存活（空闲 30 分钟 / 最长 24 小时回收），所以中途可以停下等人——比如让人工过完验证码再继续。会话里的 `#` 等选择器要 URL 编码（`#id` → `%23id`）。

**Q：`QDRUST_AUTH_MODE=oidc` 但页面没有直接跳转 IdP / 只看到登录面板？**
A：纯 OIDC 模式下未登录访问应**自动跳转**到 IdP 发起 SSO。只有两种情况会停留并显示 SSO 兜底面板：(1) 当前已带 `?login_error=`（上次回调失败，给你一个可重试的入口，避免跳转死循环）；(2) 页面已登录。若你始终停留在面板，多半是回调带上了遗留的 `login_error` 参数——清掉 URL 参数刷新即可。

**Q：OIDC 登录了，但点登出又回到登录页，IdP 侧会话还在？**
A：默认只清除 qdrust 本地会话。要让提供方会话一起结束，配置 `QDRUST_OIDC_LOGOUT_URL` 指向 IdP 的 end-session 端点（authentik：`.../protocol/openid-connect/logout`；Keycloak：`.../realms/<realm>/protocol/openid-connect/logout`）。可选配 `QDRUST_OIDC_POST_LOGOUT_REDIRECT_URI` 让 IdP 登出后跳回应用——**仅当该地址已在 IdP 的 OIDC 客户端登记过**才配，否则多数 IdP 会拒绝未登记的回跳参数。
