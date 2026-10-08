# 参考

> 返回 [README](../README.md)

API 错误码约定、与旧 QD 的兼容范围，以及立项期规划范围。

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
- **提取行可以没有名字**：QD 的编辑器按一次 ADD 就插入 `{name: '', re: '', from: 'content'}`（`web/tpl/har/entry_editor.html`），它的 `run_rule` 又只把结果写进 `variables[name]`、从不读这个名字，所以导出的模板带一条「有模式、没名字」的行是常态。这里照样接受：这类行不落进任何变量（模板也读不到空名字），但**整行原样保留**，作者把名字补上就生效。曾按必填校验过，结果是整个模板导不进来 —— gmgard 模板（`tests/fixtures/gmgard.punch-in.har`）里正是这样一条。
- **`u` 是 Python 的默认值**：Python 3 里 `re.U` 是 no-op，Unicode 本来就是默认，所以 `u` 不可能表示"关掉 Unicode"；`\w`/`\d`/`\b` 一律按 Unicode 解释，与 `u` 无关。
- **断言不读 flags**：旧 QD 把断言的 `re` 原样交给 `re.search`，斜杠形式在那里只可能匹配它自己。仓库里 3456 条断言没有一条写成斜杠形式，所以这里对断言也按上面的规则解析 —— 是超集，不是差异。
- **方言差异里已抹平的**：编译前按 Python 的读法改写 pattern，共六处 ——
  1. 省略的重复下界：Python 的 `{,n}` 是 `{0,n}`、`{,}` 是 `{0,}`，把零写出来就是全部差别。
  2. 不成形的 `{…}` 当字面量：Python 只在后面跟成形重复（`{m}` `{m,}` `{m,n}` `{,n}` `{,}`）时才把 `{` 当量词，所以 `{"total":(\d+)` 这类从 JSON 取值的写法在 QD 里正常，Rust 却拒绝整个模式（语料里 27 条）；把这类 `{` 转义成 `\{`。
  3. `\Z` 改写为 Rust 的 `\z`，两者都是「字符串末尾」。类内的 `\Z` Python 也拒绝，所以原样留下让两个引擎都拒绝。
  4. 反斜杠 + 标点还原成标点本身：Python 里 `\<` 就是 `<`，而 Rust 引擎拒绝这种转义、回溯引擎还会把它读成 Oniguruma 的「词首」。yunyaokz.har 的 `(?<=累计已签到:  \<b\>)` 就靠这一条才能按原意匹配 `<b>`。
  5. 类内的 `[` 转义为 `\[`：Python 没有 POSIX 类，`[[:alpha:]]` 是 `[ : a l p h` 六个字符再加一个字面 `]`，而 Rust 会把它读成 POSIX 字母类 —— 编译通过、匹配错文本，最安静的一种差异。
  6. 反斜杠 + 数字：Python 只在两种情况下读成字符 —— 首位是 `0`（其后最多再跟两个八进制位，所以 `\0377` 是 `\x1f` 加一个字面 `7`），或整三个八进制位（`\123` 是 `S`）；其余是反向引用，而组号最多两位（`\128` 是「第 12 组 + 字面 8」，因为第三位必须也是八进制位才凑成那三个）。类内一律读成字符，`[\1]` 是 `\x01`、`[\12]` 是换行。两个引擎只有反向引用这一种读法，所以这类转义改写成等值的 `\xNN`；超过 `\377` 的值 Python 也拒绝（`\400`），照拒。
- **刻意拒绝的**（翻译只能靠猜，而猜错会匹配到错的文本，所以宁可报错）：
  - `{m,n}` 里 `m > n`：Python 是语法错，回溯引擎却会当成能匹配。
  - `\N{...}` 命名字符：Python 读成 `•`，回溯引擎读成「非换行 + 字面 `{BULLET}`」，会编译通过却匹到错的文本；按 Python 读法要引整张 Unicode 名字表（`unicode_names2`，另需在 `deny.toml` 放行 `Unicode-DFS-2016`），而 6182 条 `re` 与 387 个模板全文里一条都没用到，所以先拒绝。
  - 反斜杠 + Python 不认的字母（`knows_escape` 的白名单是 `abfnrtvdDsSwWABZNxuU`）：一次堵掉 `\h` `\R` `\G` `\X` `\p{…}` `\K` 这批回溯引擎认、Python 不认的词。
  - `(?a)` ASCII 内联 flag（含 `(?a:…)` 作用域形式）：它把 `\w \d \s \b` 及其否定收窄到 ASCII，并且**连带把不区分大小写也收窄成 ASCII** —— `(?ai)é` 在 Python 里不匹配 `É`。两个引擎都没有它：线性引擎的 `(?-u:…)` 会顺手让 `.` 变成「任意字节」，回溯引擎直接拒绝内联改 Unicode 模式。只翻那几个简写会留下 `\b` 与大小写折叠按错的方式匹配，所以整条拒掉。
  - 反向的三条，是**这里会接受而 Python 拒绝**的写法，不拒就会「在这能跑、在 QD 报错」：`(?<name>…)` 这种 PCRE 写法的具名组（Python 只认 `(?P<name>…)`）、`(*FAIL)` 这组回溯动词（两个引擎都实现），以及 `(?-i)` 这种「组外关掉 flag」（Python 要求写成 `(?-i:…)`，两个引擎照收）。
- **条件组 `(?(id)yes|no)` 已支持**，分支按「该组是否参与过匹配」走，与 Python 一致；分支目标有两处要守。条件写成组名时，那个名字必须是**此前已经读过**的组（`(?(n)a|b)(?P<n>x)` 在 Python 里同样是错误）；写成数字时，可以指向后面的组（`(?(2)a|b)(x)(y)` 合法，但分支永远走 `|no` 一侧，因为读条件时那组还没参与）。数字还可以用引擎自己的组数复核，所以 `(a)(?(2)b|c)` 这种「组号不存在」照拒。具名条件会被改写成数字形式：回溯引擎的具名条件查不到组（实测 `(?P<x>a)?(?(x)b|c)` 匹配 `bc` 而不匹配 `ab`，`yes` 分支根本走不到），数字形式正常；改写出的组号再用引擎的 `capture_names()` 复核，对不上就拒绝 —— 组号错等于分支错，比报错更糟。
- **两个引擎**：线性引擎（`regex`，不回溯，任何 pattern 都拖不进指数时间）先编；它拒了才用回溯引擎（`fancy-regex`），因为 Python 的 lookaround（`(?=` `(?!` `(?<=` `(?<!`）和反向引用（`\1`）线性引擎没有。回溯引擎带固定上限（1000000 步），超限**报错**而不是当作「不匹配」—— 引擎没走到的结论不能当成结论。
- **不命中时不能重扫**：上限管得住回溯，管不住「在每个位置都重扫一遍响应体」，而 `(?s)(?=.*…)` 这类断言正是这么写的。回溯引擎因此开了 `seek` 预过滤：先拿模式的一个保守近似跳过不可能的位置。没有它，Linux_SB.har 的断言对 100 KB 响应体要 63.8 s 才回「不匹配」，而这恰是调度中最常见的失效响应（会话过期的错误页）；开了预过滤是毫秒级。近似按上游说明是**保守**的 —— 只可能多给候选位置，不会漏掉可匹配的位置 —— 所以它不会把正确答案改错；能因它改变的只有「本来要耗尽上限」那一类，方向是变准：一个以字面量结尾的模式碰上不含该字面量的响应体，原来报「放弃」，现在直接给出正确的「不匹配」。全部 6280 条用例开/关 `seek` 逐条比对为 0 差异。
- **覆盖**：仓库里 387 个模板共 6182 条 `re`，**6181 条可编译**（这条线最初有 62 条被拒）。余下 1 条是 `class="c">[\s]+([^]+) </div>`，其中的 `[^]` 在 Python 里同样是「未闭合字符集」，两个引擎一致拒绝 —— 模板自身的笔误，不是差异。用 Python 的 `re` 实现同一套读写规则，对全部 6182 条逐条差分：两个方向都是 0 分歧，取值也逐条一致。
- **仍然不同的**（模板库里都不出现，逐条差分出来的清单）：
  - **这里拒绝**：`\N{...}`、`(?a)` / `(?a:…)`。前者是「换一张 Unicode 名字表」的距离，后者是「两个引擎都不支持内联改 Unicode 模式」的距离，都记在上面。
  - **这里多接受**：`a**`、`a(?i)b`（Python 3.11+ 要求全局 flag 必须写在表达式开头，Python 3.8–3.10 只警告），以及非法或重名的捕获组名 —— 这些交给引擎自己的判定。
  - 一张 58 条模式的能力矩阵（同一份清单同时喂给参考解释器与这里，逐条对判）：改动前是「Python 收、这里拒」5 条、「这里收、Python 拒」2 条；现在是 3 条对 0 条。剩下的 3 条就是上面「这里拒绝」的两类，`(?a)` 占两条（裸形式与 `(?a:…)`）。

### api:// 内置路由

- `util/unicode`
- `util/urldecode`
- `util/gb2312`
- `util/regex`
- `util/string/replace`
- `util/timestamp`
- `util/totp`
- `util/rsa`
- `util/delay`
- `util/dddd/ocr`
- `util/dddd/det`
- `util/dddd/slide`
- `util/toolbox/notepad`
- `util/toolbox/notepad/list`

OCR/验证码能力允许作为可选插件，但导入诊断必须能识别其路由。未安装插件时执行应返回稳定的 `plugin_unavailable` 错误。

### 记事本（`util/toolbox/notepad`）

来源：旧 QD `web/handlers/util.py` 的 `ToolboxNotepadHandler` 与 `ToolboxNotepadListHandler`、`db/notepad.py`。Web 侧对应的页面见[记事本页面](#记事本页面)。

模板的跨运行状态：一张 `(账号, 槽位)` 的表，值是一整块文本。QD 模板靠它做 cookie 滑动续期、记上次跑到第几页、累计次数、去重 id 这类事——把这一圈接起来的是「**不管读写，响应体都是操作之后的当前值**」，所以存下新值的那一步同时就是把它 `extract_variables` 出来的那一步。

| 参数 | 说明 |
| --- | --- |
| `f` | `write` / `append` / 其它（读）。**按子串判断，且先判 `write`**，所以 `f=writeappend` 是写，`f=read` 是读 |
| `id_notepad` | 槽位号，默认 `1`（`list` 路由默认「未指定」，QD 记作 `-1`） |
| `data` | 要写入的文本，默认空串。GET query 或 POST 表单体都行（表单体会并入查询串） |

行为要点：

- **槽位 1 按需创建**，其它槽位缺失一律回 400「记事本不存在」——所以读一次 `util/toolbox/notepad` 就会建出槽位 1。
- **`append` 用 CRLF 拼接，且只在已有内容时才加分隔符**：QD 自己创建的槽位内容是 NULL，往它 append 不会以空行开头；显式写入空串则下次 append 会带分隔符。列表页的 `f=add` 把空 `data` 也存成 NULL。
- **`f=list` 之外还接受 `add` / `delete`**（`util/toolbox/notepad/list`），响应体是 QD 自己的 Python 列表字面量（`[1, 2]`）与「添加成功, id_notepad: N」这类文本。**该路由不能凭空建第一个槽位**：账号一个槽都没有时它回「无法获取该用户记事本编号」。首次创建的两条路是模板侧的首次读取（`util/toolbox/notepad`）与[记事本页面](#记事本页面)的「新建槽位」。
- **失败是 400 响应而不是中断**：QD 捕获异常、把消息写进响应体并置 400，模板照样拿到一个响应，由它自己的断言决定这意味着什么；这里照办，否则同一个模板会得到不同结果。

与 QD 的三处刻意不同：

1. **账号取自这次运行，不读 URL 里的 `email` / `pwd`**。QD 用它们做鉴权，那等于把账号密码放进模板；模板带着它们也能跑，只是不被使用，而没有它们的模板同样能跑。
2. **写入有上限**（5 MiB，与默认 `response_limit` 一致），超限回 400「记事本内容超过上限」。QD 不设上限；这里不设就等于模板可以无上限地写库。
3. **槽位上限 20**（QD 的 `NOTEPAD_LIMIT` 默认值）是常量而不是环境变量，理由和上面那条内容上限一样：20 个槽位已经远超模板所需，这不是一个部署需要改的数。模板侧的 `f=add` 与页面的「新建槽位」都从这一个值上取限。

`(owner_id, notepad_id)` 上还有 QD 没有的 UNIQUE 约束：QD 的 `mod` 按这两列更新，一旦出现两行同槽位就会两行一起被改。

**迁移提示**：QD 的 HAR 编辑器插入这两步时写的是 `email={{qd_email|urlencode}}&pwd={{md5(qd_pwd)|urlencode}}&id_notepad=1`（`web/static/coffee/har/entry_editor.coffee`）——槽位是字面量 `1`，账号那两项是 QD 提供给模板的内置变量。有的模板把槽位改成自己的变量（gmgard 那份写的是 `id_notepad={{id_notepad}}`），那就要在任务的变量表里填上它：渲染成空串时 `id_notepad=` 不是「按默认取 1」，而是「不是整数」（QD 的 `int('')` 同样抛错、同样回 400）。`qd_email` / `qd_pwd` 这里没有对应变量，它们渲染成空串并被本端口忽略，从模板里删掉不影响结果。

**并发**：`f=write` 是一条 upsert 语句，本身原子；`f=append` 是「读出来、拼上、写回去」，两次运行同时 append 同一个槽位会丢掉一次（QD 同样如此，它的 `mod` 也是读改写）。跨任务共享一个槽位时要么避开 `append`，要么接受这个窗口 —— 这个端口没有引入锁，因为它不该比 QD 更强，而加锁要决定的是「谁等谁」这种模板层没法表达的事。

**写入值会出现在运行记录的步骤名里**：步骤名是渲染后 URL 的前 200 字符（`scheduler.rs` 的 `truncate_name`），而 `data` 是 URL 的一部分，QD 也是这样。所以模板存进去的东西——如果一个模板存的是 cookie，那就是那截 cookie——开头一段会留在 `runs`/`run_steps` 与实时步骤流里。运行记录页面因此不该对外分享；真要在日志里藏掉这一段，是另一个改动（把 notepad 的 `data` 从记录的 URL 里替换掉），不影响模板拿到什么。**要读回一个槽位就[记事本页面](#记事本页面)**，那是完整内容，不必从被截断的步骤名里拼。

### 记事本页面

QD 的记事本是「工具箱」里的一个按钮；这里是一个侧边栏入口，背后是四个接口。能力对齐，形态按这一侧的设计走：

- **读写删都走会话身份，不填账号密码。** QD 的页面必须填邮箱与密码（红字写着「除查询以外, 必须输入账号密码验证！！！」），因为它页面的 POST 与模板调用**共用同一个路由**，而模板在 worker 里跑、没有浏览器会话，那个路由就不能依赖会话认证——`web/handlers/util.py` 里 `ToolboxNotepadHandler` 的 GET 有 `@authenticated`，POST 上那一行是被注释掉的。这里的模板侧走 executor 的动作臂，HTTP 面是完全独立的，所以页面直接用登录身份。
- **「新建槽位」是显式动作**，不像 QD 把首次创建藏在页面 `GET` 的副作用里。
- **列表给预览与大小**（前 120 字符、字节数、更新时间），完整内容在打开某个槽位时才取：一次拉回 20 个各 5 MiB 的槽位既没必要，也不是页面要显示的东西。

| 接口 | 说明 |
| --- | --- |
| `GET /api/v1/notepads` | 本账号的槽位，按槽位号升序；每项带 `preview` / `content_size` / `updated_at` |
| `GET /api/v1/notepads/{notepad_id}` | 一个槽位的完整内容；不存在回 404 |
| `PUT /api/v1/notepads/{notepad_id}` | 写入一个槽位；**不存在则创建**（201），已存在则覆盖（200） |
| `DELETE /api/v1/notepads/{notepad_id}` | 删除；不存在回 404 |

与模板侧的第二处刻意不同就在 `PUT` 上：模板写一个不存在的槽位，非 1 号一律回 400（和 QD 一样）；页面写一个不存在的槽位则创建它，因为页面就是用户建槽位的地方——否则用户得先"创建"再"保存"，而 QD 那边这一步在页面上的「新建」按钮里。槽位号必须 ≥ 1，上限仍是上面那个常量。

页面自己的写入上限是 256 KiB，比模板侧小，这一条值得说清它是怎么来的：axum 默认最多缓冲 2 MiB 请求体，而这组路由没有抬高它，所以 5 MiB 的写入在进入处理函数之前就被拒了，用户拿到的会是一句英文的 body 缓冲失败，而不是「超过上限」。模板写进槽位的大值，页面照样**读**得回来——只有页面自己的写入受这个小上限约束。

## 立项期规划范围

> 下表是 Phase 0 立项时定下的范围与优先级，作为历史快照保留：记的是「当初打算做什么」。**它不记实现状态** —— 这张表不随实现更新，带上状态列就会变成第二处会过期的说法。当前状态与已知缺口见 [README · 项目状态](../README.md#项目状态)。

判断某项能力是否算完成，看的不是页面或接口是否存在，而是领域测试、权限测试和主要工作流测试是否同时通过。

| 模块 | 能力 | 优先级 |
| --- | --- | --- |
| Core | QD HAR 1.2 与扩展字段无损解析 | P0 |
| Core | QD 控制流编译 | P0 |
| Core | HTTP、Cookie、Jinja、断言、提取 | P0 |
| Core | QD 表达式兼容 | P0 |
| Core | `api://` 插件调用 | P0 |
| CLI | validate、inspect | P0 |
| CLI | run、变量输入、JSON 输出 | P0 |
| Server | RESTful 模板/任务接口 | P0 |
| Server | 用户、Session、CSRF、权限隔离 | P0 |
| Server | 调度、立即运行、取消、重试、lease | P0 |
| Server | 运行与步骤日志、WebSocket | P0 |
| Server | 公共模板 | P1 |
| Server | 插件注册、配置、权限与审计 | P1 |
| Server | 消息推送与动作 | P1 |
| Server | 记事本 | P1 |
| Web | 登录、概览、模板、任务、运行记录 | P0 |
| Web | 公共模板、插件、推送、记事本 | P1 |
| 全局 | 中英文国际化 | P0 |
| 交付 | SQLite Docker 单机部署 | P0 |
| 交付 | MySQL、Redis、多实例 | P1 |
