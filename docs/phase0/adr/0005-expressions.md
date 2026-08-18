# ADR-0005：表达式双轨实现

- 状态：Accepted
- 日期：2026-08-17

## 决策

原生模板和 QD HAR 均以 MiniJinja 表达式编译器为安全基础，但使用不同的函数注册表。QD 兼容层提供 `int/float/bool/list/len/range` 及后续经 fixture 确认的兼容函数。两种格式共享 JSON 值模型和资源预算，但不自动互译。

QD 兼容候选语法：字面量、变量、索引、比较、布尔与算术运算、`in/not in`、`list()`、`range()`、`len()`。明确禁止属性反射、import、lambda、推导式、文件、网络、环境和进程访问。

## 实现与验收

选择 MiniJinja 而不是 Rhai 或自写解释器，因为请求渲染本身已经需要 Jinja 语义，共用解析和值模型能减少两套动态类型之间的偏差。任意 Python bytecode、属性反射、import、lambda、推导式和模块访问都不支持。

至少 50 份脱敏真实 HAR 的通过率是 Phase 3 的兼容验收门禁。发现新语法时必须先加入 fixture，再显式扩展兼容函数或诊断，不能调用 Python `eval` 兜底。

## 当前证据

MiniJinja PoC 已通过变量真假、比较、`and`、`int(...)`、`range(...)`、`list(...)`、`len(...)`、索引、成员判断、缺失变量、危险 `__import__` 拒绝、while 超限，以及 `if + for` 驱动本地 HTTP 请求测试。
