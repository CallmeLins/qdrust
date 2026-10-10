# 模板表达式与内置工具

> 返回 [README](../README.md)

模板变量与步骤请求支持两种动态求值方式，兼容旧 QD HAR：

- **Jinja2 表达式**：由 `minijinja` 驱动，内置 27 个过滤器（`upper`/`lower`/`replace`/`split`/`join`/`sort`/`unique`/`tojson`/`fromjson` 等）和 39 个表达式函数（`int`/`float`/`len`/`b64encode`/`md5`/`sha1`/`hash`/`timestamp`/`strftime`/`random_int`/`fake`/`regex_*`/`totp` 等）。Jinja2 内建而 minijinja 未带的过滤器也补齐了：`striptags`/`wordcount`/`truncate`/`wordwrap`/`center`/`filesizeformat`/`xmlattr`；常用的 Python 字符串 / 字典方法（`s.strip()`、`csv.split(',')`、`o.get('k', '默认')`）与 `"%s" % x` 格式化可直接写，见下文[Python 写法](#python-写法方法与--格式化)。
- **`api://util/*` 内置工具**：当模板步骤 URL 以 `api://` 开头时，executor 在进程内计算并返回结果，不发起真实 HTTP 请求。时间、编码、哈希、正则、JSON 类工具已与 QD 对齐：

  | 工具 | 说明 |
  |---|---|
  | `delay` | 固定 / 随机延迟（`?seconds=N`，或 QD 路径式 `api://util/delay/N`），响应文本与 QD 一致：`delay N second.`；负数按 0 处理，非法值回 `Error, delay 0.0 second.` |
  | `timestamp` | `?ts=<秒>` 或 `?dt=<时间>&form=<strftime 格式>`（都不给则用当前时间），返回 QD 同款中文键 JSON：完整时间戳 / 时间戳 / 16位时间戳 / 周 / 日 / 北京时间 / GMT格式 / ISO格式 / 状态 |
  | `unicode` / `urldecode` / `urlencode` | 编码转换 |
  | `gb2312` | GB2312 百分号编码（urllib.quote 语义） |
  | `base64` (encode / decode) | Base64 编解码 |
  | `hash` (md5 / sha1 / sha256 / sha512) | 哈希 |
  | `totp` | TOTP（RFC 6238）：`?secret=<base32>&digits=6&period=30&algo=sha1`。在本地算码，2FA 密钥不出网；表达式侧同样可用 `{{ totp(secret) }}`（`secret` 会被识别为输入变量） |
  | `uuid` / `random` (float) | 随机值 |
  | `regex` | `?data=<原文>&p=<正则>`，等价 QD 的 `re.findall(p, data, re.IGNORECASE)`，返回 `{"数据": {"1": …, "2": …}, "状态": "OK"}`；正则非法时只回 `状态` |
  | `string/replace` | 正则替换，支持组引用与文本模式 |
  | `rsa` (encode / decode) | PKCS1 v1.5 加解密 |
  | `json` (parse / stringify / pretty) | JSON 处理 |
  | `dddd/*` | OCR / 验证码识别，转发到外部 DdddOCR 服务（目标由 `_server` 参数或 `QDRUST_DDDDOCR_SERVER` 指定）。这是唯一会出网的 `util` 动作，因此和模板请求走同一道闸门：默认拒绝内网 / 回环地址，见[出站请求访问内网地址](usage.md#出站请求访问内网地址高风险开关) |

  外部插件二进制（带 manifest、API 版本校验与能力声明 network / read_file / write_file / environment）通过子进程 JSON 协议调用，WebUI 提供插件管理页。

- **`api://browser/*` 浏览器插件（可选）**：进程内持有 chromiumoxide 驱动远程无头浏览器，处理"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 步骤做不了的一步。配置 `QDRUST_BROWSER_URL` 即可启用（无需在插件管理页新建条目），`content`/`eval`/`screenshot`/`start`/`end`/`type`/`click`/`keepalive` 等 action 见 [浏览器插件（无头浏览器签到）](browser-plugin.md)。

## 响应体编码

QD 模板的正则都写在中文页面上，读错编码就等于读错文本，所以响应体的解码顺序和 QD 的 `utils.decode` 一致：

1. 响应头 `Content-Type` 声明的 `charset`。其中 `ISO-8859-1` 视为**未声明**——那是 HTTP 栈在没人决定时写的默认值，QD 也把它丢掉；
2. 页面自带的 `<meta charset=…>` / `<meta content="…;charset=…">` / `<?xml … encoding=…?>`；
3. 都不说时按内容自动探测；
4. 最后兜底 Latin-1。

`gb2312` 一律按 `gb18030` 解码（站点声明的窄集，字节里往往有超出 1980 字符集的字），无法解码的字节变成 `U+FFFD` 而不是让步骤失败。

`from=content` 的断言与提取读到的就是这段文本；**响应的 `content-type` 是图片时**，`content` 是原始字节的 base64（QD `getdata` 的行为），验证码 / 图形接口这类步骤因此能直接把图取出来。

## Python 写法：方法与 `%` 格式化

QD 模板是 Python，qdrust 在渲染前把下面这些写法改写成等价的引擎调用，模板可原样迁移：

| 写法 | 说明 |
|---|---|
| `s.strip()` / `s.split(',')` / `s.replace(a, b)` / `s.upper()` / `s.startswith(x)` / `s.format(a, b)` | 常用 `str` 方法；`startswith`/`endswith` 支持元组参数，`find`/`index`/`count` 支持起止下标，`split` 的 `maxsplit`、`rsplit`、`splitlines` 同样可用 |
| `d.get('k', '默认')` / `d.keys()` / `d.values()` / `d.items()` | 字典读取；`{% for k, v in d.items() %}` 直接可用 |
| `items.index(x)` / `items.count(x)` | 列表读取（就地修改见上一节） |
| `"%s-%s" % (a, b)` / `"%d 分" % n` | printf 风格格式化：`%s`/`%r`/`%d`/`%f`/`%e`/`%g`/`%x`/`%o`/`%c`/`%%`，以及宽度、精度与 `-`/`0` 标志 |

改写只发生在 `{{ … }}` / `{% … %}` 内部，模板里的 HTML / JS 文本一字不动；不在支持列表里的方法名不做改写，照旧报 `has no method named …`，不会静默变成一次字典取值。

## 带参数的过滤器：`urlencode` 与 `default`

QD 把 `qdl.utils.urlencode_with_encoding` 作为过滤器暴露给模板，Jinja2 自己又内建了 `default`。两者的参数位在 qdrust 里同样保留，从 QD 抄来的写法不必删参数：

| 写法 | 语义 |
|---|---|
| `{{ x \| urlencode }}` | 百分号编码，等价 `urlencode(x, 'utf-8', false)` |
| `{{ x \| urlencode('gbk') }}` / `{{ x \| urlencode(encoding='gbk') }}` | 指定字符集。**只支持 `utf-8`**：QD 的 `url_quote` 能按任意 Python 编码逐字节编码，而这里没有那套编码表，与其悄悄按 UTF-8 编出发错的串，不如报 `urlencode only supports utf-8, got "gbk"` |
| `{{ x \| urlencode(for_qs=True) }}` | 表单 / 查询串模式：空格编成 `+` 而不是 `%20`（Python `urllib.parse.quote_plus` 的开关） |
| `{{ d \| urlencode }}`（`d` 是字典或键值对列表） | 拼成 `k=v&k=v`，两侧都编码、`/` 也编码（QD `urlencode_with_encoding` 对 dict / iterable 的分支） |
| `{{ x \| default('兜底') }}` | 值为**未定义**时替换（Jinja2 语义，空串与 `0` 不替换） |
| `{{ x \| default('兜底', true) }}` / `default('兜底', boolean=True)` | `boolean=True` 时**假值**也替换——空串、`0`、`[]`、`{}` 都会用兜底值 |
| `{{ x \| d('兜底') }}` | `default` 的别名，参数同上 |

多个参数一起写也可以（`{{ d | urlencode('utf-8', true) }}`）；传了不认识的关键字（例如对 `default` 写 `boolean` 之外的名字）会报出**可接受的名字列表**，而不是只说一句类型不对。

## 与 Python QD 的已知差异：列表的就地修改

`minijinja` 的值默认**不可变**，而 Python QD 的 Jinja2 直接操作 Python 对象。不过 qdrust 对最常见的**模板自建累加器**写法做了兼容：只要列表是在模板里用 `{% set items = [] %}`（或种子字面量 `{% set items = ['a'] %}`）声明的，`.append` / `.extend` / `.insert` / `.pop` / `.remove` / `.clear` 都可以直接用，无需任何改写（[#27](https://github.com/CallmeLins/qdrust/issues/27)）：

```jinja
{# 模板自建的列表：qdrust 与 Python QD 行为一致 #}
{% set parts = [] %}
{% for x in rows %}{% set _ = parts.append(x) %}{% endfor %}
{{ parts | join('; ') }}
```

实现上，渲染前会把模板内的字面量列表声明替换为内部可变对象（`__qd_list`，引擎全局函数，不会出现在任务的必需变量列表里）；该改写只在模板中出现了就地修改调用时发生。

仍然**不支持**的两类写法：

- 对**外部传入的变量**（提取到的数组、函数返回值）就地修改——它们是共享的不可变值：

  ```jinja
  {# cn 是提取变量，两边都不要这样写 #}
  {% set _ = cn.append('x') %}
  ```

  改用 `namespace` 模式即可，**改写后的模板在 Python QD 中同样合法**，两边通用：

  ```jinja
  {% set ns = namespace(items=[]) %}
  {% for x in rows %}{% set ns.items = ns.items + [x] %}{% endfor %}
  {{ ns.items | join(',') }}
  ```

- 字典的就地修改（`.update` 等）；拼接（`+`）、过滤（`| select` / `| map` / `| sort`）等纯函数写法不受影响。
