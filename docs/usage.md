# 使用

> 返回 [README](../README.md)

## 导入 QD HAR 模板

- 在 WebUI 的模板页点击导入，选择从旧 QD 导出的 `*.har.json`。
- 导入时会校验并**保留原始 HAR JSON**，兼容样本见 `tests/fixtures`。
- 也可用命令行校验：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

### 模板变量是怎么算出来的

基于模板新建任务时，WebUI 会按 QD `HARSave.get_variables` 的语义列出需要填写的变量（`GET /api/v1/templates` 返回的 `variables` 字段）：

- 只看请求实际读取的名字：method、URL、请求体，以及每个请求头 / Cookie 的名字与值；
- 用 Jinja 解析，所以 **过滤器和函数名不算变量**：`{{jpop_username|urlencode}}` 只产出 `jpop_username`，`{{md5(password)}}` 只产出 `password`；
- **按条目顺序判定**：只有被 `extract_variables` 提取**之后**的引用才不算输入。所以登录后的 `{{token}}` 不会被问，而先喂给 `api://util/gb2312` 再由该条目回写提取的 `{{username}}` 仍然是输入；
- 无法解析的片段（如 `{% while ... %}` 控制条目）不产生变量。

`native_v1` 模板则直接用定义里声明的变量名。

## 创建与运行任务

1. **模板必选**：新建任务时先选模板——任务的请求完全由模板决定（多页模板常混合 GET/POST），任务本身不携带请求方法 / URL / 请求头 / 请求体。
2. 填写变量（模板需要的变量会按 QD 语义自动列出）、配置 cron / 间隔调度。
3. 用「立即运行」手动触发，或在运行记录中查看每一步的请求 / 响应 / 提取变量；支持取消正在执行的运行。
4. 订阅仓库的任务会按仓库自动导入并同步；公共模板经 PushRequest 审批后发布。

> 任务列表里的请求方法与 URL 是**模板首条请求的镜像**，只用于展示；执行始终以模板为准。早于该规则的存量任务（未绑定模板、自带一条请求）编辑时仍会保留「（无模板）」选项与其原有请求。

## 可视化 HAR 编辑器

WebUI 提供可视化编辑器，可直接增删改请求、设置请求头 / 表单 / 鉴权、编写 `success_asserts` 与 `extract_variables`，无需手改 JSON。

## 调度与随机延迟

可视化调度器支持固定间隔与 cron；「随机延迟」可让同一任务的多次执行在时间上打散，避免被目标站点识别为固定节奏。模板变量支持在创建任务时**预填**默认值。

### 时区与夏令时

cron 按每个任务各自的 IANA 时区（`timezone` 字段）求值，**DST 感知**：设 `America/New_York` 的任务，`0 9 * * *` 会全年在当地 09:00 触发，无需为冬令/夏令手改 cron。任务未设 `timezone` 时使用服务端 `QDRUST_DEFAULT_TIMEZONE`（默认 `Asia/Shanghai`）。WebUI 中任务列表与运行历史的「上次运行/开始时间」也按该任务时区展示（未设时区则用查看者浏览器本地时区）。

## 运行日志与历史

每次运行会记录 QD 风格的文本日志；在任务详情页可查看该任务的运行历史，并支持**清空**历史记录。活跃运行的步骤通过 WebSocket 实时推送。

## 命令行（CLI）

校验旧 QD HAR：

```powershell
cargo run -p qdrust-cli -- validate .\template.har.json
```

执行 HAR，并传入变量和整体超时：

```powershell
cargo run -p qdrust-cli -- run .\template.har.json --var token=abc --timeout 60
```

CLI 默认拒绝私网、localhost 和无效 TLS 证书，不继承旧 QD 的宽松网络策略。
