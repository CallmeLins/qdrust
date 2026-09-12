# 模板表达式与内置工具

> 返回 [README](../README.md)

模板变量与步骤请求支持两种动态求值方式，兼容旧 QD HAR：

- **Jinja2 表达式**：由 `minijinja` 驱动，内置 26 个过滤器（`upper`/`lower`/`replace`/`split`/`join`/`sort`/`unique`/`tojson`/`fromjson` 等）和 38 个表达式函数（`int`/`float`/`len`/`b64encode`/`md5`/`sha1`/`hash`/`timestamp`/`strftime`/`random_int`/`fake`/`regex_*` 等）。
- **`api://util/*` 内置工具**：当模板步骤 URL 以 `api://` 开头时，executor 在进程内计算并返回结果，不发起真实 HTTP 请求。时间、编码、哈希、正则、JSON 类工具已与 QD 对齐：

  | 工具 | 说明 |
  |---|---|
  | `delay` | 固定 / 随机延迟（`?seconds=N`，或 QD 路径式 `api://util/delay/N`） |
  | `timestamp` | 当前时间戳（多种格式） |
  | `unicode` / `urldecode` / `urlencode` | 编码转换 |
  | `gb2312` | GB2312 百分号编码（urllib.quote 语义） |
  | `base64` (encode / decode) | Base64 编解码 |
  | `hash` (md5 / sha1 / sha256 / sha512) | 哈希 |
  | `uuid` / `random` (float) | 随机值 |
  | `regex` (findall / replace) | 正则提取与替换 |
  | `string/replace` | 正则替换，支持组引用与文本模式 |
  | `rsa` (encode / decode) | PKCS1 v1.5 加解密 |
  | `json` (parse / stringify / pretty) | JSON 处理 |
  | `dddd/*` | OCR / 验证码识别，转发到外部 DdddOCR 服务 |

  外部插件二进制（带 manifest、API 版本校验与能力声明 network / read_file / write_file / environment）通过子进程 JSON 协议调用，WebUI 提供插件管理页。

- **`api://browser/*` 浏览器插件（可选）**：进程内持有 chromiumoxide 驱动远程无头浏览器，处理"生成签名 / 过验证码 / 渲染 JS / 多步表单交互"这类纯 HTTP 步骤做不了的一步。配置 `QDRUST_BROWSER_URL` 即可启用（无需在插件管理页新建条目），`content`/`eval`/`screenshot`/`start`/`end`/`type`/`click`/`keepalive` 等 action 见 [浏览器插件（无头浏览器签到）](browser-plugin.md)。
