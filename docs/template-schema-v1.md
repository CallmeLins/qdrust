# Template Schema v1

qdrust 模板是版本化 JSON，不执行 Python。顶层必须包含 `version: 1`、非空 `name` 和 `steps`。

```json
{
  "version": 1,
  "name": "Health check",
  "variables": {
    "base_url": "https://example.com"
  },
  "steps": [
    {
      "type": "request",
      "name": "Fetch health",
      "method": "GET",
      "url": "{{base_url}}/health",
      "headers": {"accept": "application/json"}
    },
    {
      "type": "extract",
      "name": "Read status",
      "source": "status",
      "selector": "",
      "target": "status",
      "required": true
    }
  ]
}
```

v1 步骤类型：`request`、`extract`、`if`、`for_each`、`delay`。请求 body 支持 `json`、`text` 和 `form`。表达式及 `selector` 的完整语法将在对应 ADR 接受后冻结。

安全限制：最多 1000 个静态步骤、最多 16 层嵌套、单次 delay 最长 5 分钟。运行时还会独立限制请求数、循环次数、响应体和总时长。
