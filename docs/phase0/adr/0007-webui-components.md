# ADR-0007：WebUI 组件库

- 状态：Accepted
- 日期：2026-08-17

## 决策

Vue 3 WebUI 使用 Element Plus 作为表单、表格、分页、对话框、提示和可访问交互的基础，Lucide Vue 继续提供图标。业务布局和视觉 token 保持项目自有 CSS，不用组件库拼装营销式卡片页面。

## 比较

Element Plus 与 Naive UI 都支持 Vue 3、TypeScript、按需引入和中英文 locale。Element Plus 在复杂表单、数据表格、日期时间和中文资料方面更符合 QD 的运维型界面；代价是默认视觉较重，因此通过 token 和按需导入控制样式与体积。

现有任务页面作为交互原型保留。完整 WebUI 阶段逐个迁移控件，不进行一次性视觉重写。
