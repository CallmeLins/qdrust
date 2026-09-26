# 模板表达式与内置工具

> 返回 [README](../README.md)

模板变量与步骤请求支持两种动态求值方式，兼容旧 QD HAR：

- **Jinja2 表达式**：由 `minijinja` 驱动，内置 27 个过滤器（`upper`/`lower`/`replace`/`split`/`join`/`sort`/`unique`/`tojson`/`fromjson` 等）和 39 个表达式函数（`int`/`float`/`len`/`b64encode`/`md5`/`sha1`/`hash`/`timestamp`/`strftime`/`random_int`/`fake`/`regex_*`/`totp` 等）。
- **`api://util/*` 内置工具**：当模板步骤 URL 以 `api://` 开头时，executor 在进程内计算并返回结果，不发起真实 HTTP 请求。时间、编码、哈希、正则、JSON 类工具已与 QD 对齐：

  | 工具 | 说明 |
  |---|---|
  | `delay` | 固定 / 随机延迟（`?seconds=N`，或 QD 路径式 `api://util/delay/N`） |
  | `timestamp` | 当前时间戳（多种格式） |
  | `unicode` / `urldecode` / `urlencode` | 编码转换 |
  | `gb2312` | GB2312 百分号编码（urllib.quote 语义） |
  | `base64` (encode / decode) | Base64 编解码 |
  | `hash` (md5 / sha1 / sha256 / sha512) | 哈希 |
  | `totp` | TOTP（RFC 6238）：`?secret=<base32>&digits=6&period=30&algo=sha1`。在本地算码，2FA 密钥不出网；表达式侧同样可用 `{{ totp(secret) }}`（`secret` 会被识别为输入变量） |
  | `uuid` / `random` (float) | 随机值 |
  | `regex` (findall / replace) | 正则提取与替换 |
  | `string/replace` | 正则替换，支持组引用与文本模式 |
  | `rsa` (encode / decode) | PKCS1 v1.5 加解密 |
  | `json` (parse / stringify / pretty) | JSON 处理 |
  | `dddd/*` | OCR / 验证码识别，转发到外部 DdddOCR 服务（目标由 `_server` 参数或 `QDRUST_DDDDOCR_SERVER` 指定）。这是唯一会出网的 `util` 动作，因此和模板请求走同一道闸门：默认拒绝内网 / 回环地址，见[出站请求访问内网地址](usage.md#出站请求访问内网地址高风险开关) |

  外部插件二进制（带 manifest、API 版本校验与能力声明 network / read_file / write_file / environment）通过子进程 JSON 协议调用，WebUI 提供插件管理页。

- **`api://browser/*` 浏览器插件（可选）**：进程内持有 chromiumoxide 驱动远程无头浏览器，处理"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 步骤做不了的一步。配置 `QDRUST_BROWSER_URL` 即可启用（无需在插件管理页新建条目），`content`/`eval`/`screenshot`/`start`/`end`/`type`/`click`/`keepalive` 等 action 见 [浏览器插件（无头浏览器签到）](browser-plugin.md)。

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
