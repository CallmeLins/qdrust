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

### 正则（`success_asserts` / `failed_asserts` / `extract_variables` 的 `re`）

同上来源：旧 QD 在 `libs/fetcher.py` 的 `run_rule` 里用 `re.match(r"^/(.*?)/([gimsu]*)$", …)` 解析 `extract_variables` 的 `re` 字段。

- **两种写法**：裸模式（`\d+`）与斜杠形式（`/(\d+)/g`）。flags 只认 `g/i/m/s/u` —— `x` 不在其中，所以 `/(\d+)/x` 不是斜杠形式，而是字面量模式 `/(\d+)/x`；解析不出 flags 也不算错误，只是"没有 flags 的模式"。
- **`g` 是 QD 自己的开关**，不是引擎 flag：有它走 `findall`，没有走 `search`。返回形状跟着捕获组数走 —— 0 组给整段匹配、1 组给该组、多组给"每组一个"的元组（未参与匹配的组是空串），一条都没匹配给空列表。
- **非 `g`**：有组取第 1 组，无组取整段匹配。旧 QD 在第一组未参与匹配时会把 Python 的 `None` 存进变量（模板里渲染成文本 "None"），这里改为回退到整段匹配。
- **`u` 是 Python 的默认值**：Python 3 里 `re.U` 是 no-op，Unicode 本来就是默认，所以 `u` 不可能表示"关掉 Unicode"；`\w`/`\d`/`\b` 一律按 Unicode 解释，与 `u` 无关。
- **断言不读 flags**：旧 QD 把断言的 `re` 原样交给 `re.search`，斜杠形式在那里只可能匹配它自己。仓库里 3456 条断言没有一条写成斜杠形式，所以这里对断言也按上面的规则解析 —— 是超集，不是差异。
- **方言差异里已抹平的**：编译前按 Python 的读法改写 pattern，共五处 ——
  1. 省略的重复下界：Python 的 `{,n}` 是 `{0,n}`、`{,}` 是 `{0,}`，把零写出来就是全部差别。
  2. 不成形的 `{…}` 当字面量：Python 只在后面跟成形重复（`{m}` `{m,}` `{m,n}` `{,n}` `{,}`）时才把 `{` 当量词，所以 `{"total":(\d+)` 这类从 JSON 取值的写法在 QD 里正常，Rust 却拒绝整个模式（语料里 27 条）；把这类 `{` 转义成 `\{`。
  3. `\Z` 改写为 Rust 的 `\z`，两者都是「字符串末尾」。类内的 `\Z` Python 也拒绝，所以原样留下让两个引擎都拒绝。
  4. 反斜杠 + 标点还原成标点本身：Python 里 `\<` 就是 `<`，而 Rust 引擎拒绝这种转义、回溯引擎还会把它读成 Oniguruma 的「词首」。yunyaokz.har 的 `(?<=累计已签到:  \<b\>)` 就靠这一条才能按原意匹配 `<b>`。
  5. 类内的 `[` 转义为 `\[`：Python 没有 POSIX 类，`[[:alpha:]]` 是 `[ : a l p h` 六个字符再加一个字面 `]`，而 Rust 会把它读成 POSIX 字母类 —— 编译通过、匹配错文本，最安静的一种差异。
- **刻意拒绝的**（翻译只能靠猜，而猜错会匹配到错的文本，所以宁可报错）：
  - `{m,n}` 里 `m > n`：Python 是语法错，回溯引擎却会当成能匹配。
  - `\N{...}` 命名字符：Python 读成 `•`，回溯引擎读成「非换行 + 字面 `{BULLET}`」，会编译通过却匹到错的文本；按 Python 读法需要整张 Unicode 名字表，所以拒绝。
  - 反斜杠 + Python 不认的字母（`knows_escape` 的白名单是 `abfnrtvdDsSwWABZNxuU`）：一次堵掉 `\h` `\R` `\G` `\X` `\p{…}` `\K` 这批回溯引擎认、Python 不认的词。
- **两个引擎**：线性引擎（`regex`，不回溯，任何 pattern 都拖不进指数时间）先编；它拒了才用回溯引擎（`fancy-regex`），因为 Python 的 lookaround（`(?=` `(?!` `(?<=` `(?<!`）和反向引用（`\1`）线性引擎没有。回溯引擎带固定上限（1000000 步），超限**报错**而不是当作「不匹配」—— 引擎没走到的结论不能当成结论。
- **不命中时不能重扫**：上限管得住回溯，管不住「在每个位置都重扫一遍响应体」，而 `(?s)(?=.*…)` 这类断言正是这么写的。回溯引擎因此开了 `seek` 预过滤：先拿模式的一个保守近似跳过不可能的位置。没有它，Linux_SB.har 的断言对 100 KB 响应体要 63.8 s 才回「不匹配」，而这恰是调度中最常见的失效响应（会话过期的错误页）；开了预过滤是毫秒级。近似按上游说明是**保守**的 —— 只可能多给候选位置，不会漏掉可匹配的位置 —— 所以它不会把正确答案改错；能因它改变的只有「本来要耗尽上限」那一类，方向是变准：一个以字面量结尾的模式碰上不含该字面量的响应体，原来报「放弃」，现在直接给出正确的「不匹配」。全部 6280 条用例开/关 `seek` 逐条比对为 0 差异。
- **覆盖**：仓库里 387 个模板共 6182 条 `re`，**6181 条可编译**（这条线最初有 62 条被拒）。余下 1 条是 `class="c">[\s]+([^]+) </div>`，其中的 `[^]` 在 Python 里同样是「未闭合字符集」，两个引擎一致拒绝 —— 模板自身的笔误，不是差异。用 Python 的 `re` 实现同一套读写规则，对全部 6182 条逐条差分：两个方向都是 0 分歧，取值也逐条一致。
- **仍然不同的**（模板库里都不出现，且 Python 侧判为错误）：
  - **这里拒绝**：`\ooo` 八进制转义、`(?a)` ASCII 内联 flag。
  - **这里多接受**：`a**`、`a(?i)b`（Python 3.11+ 要求全局 flag 在表达式开头）、`(?<name>x)`，以及非法或重名的捕获组名。

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
