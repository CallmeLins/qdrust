# 旧 QD 兼容清单（初始语料）

来源：旧 QD `libs/fetcher.py`、HAR 编辑器插入项及 about 文档。该清单只代表仓库内可确认能力，不能替代真实 HAR 样本。

## 控制与表达式

- `if/else/endif`
- `for <name> in <variable>`
- `for <name> in list(...)`
- `for <name> in range(...)`
- `while <condition>/endwhile`
- 变量真假、比较、`and/or/not`
- `int(loop_index0)` 及循环变量 `loop_index*`、`loop_first/last/length/depth*`

## api:// 内置路由

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
