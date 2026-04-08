# Prompt — 系统提示词动态组装

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

Prompt 模块通过 Builder 模式动态组装系统提示词，将提示词拆分为 7 个独立的 section，每个 section 用 XML 标签包裹，按刻意设计的顺序拼接。

## 2. PromptBuilder

> 📍 **代码位置**：`src/prompt/mod.rs`

### Builder 字段

| 字段 | 类型 | 用途 |
|---|---|---|
| `agent_name` | `Option<&str>` | 自定义名称（默认 "Daedalus"） |
| `tools` | `&[ToolInfo]` | MCP 工具描述 |
| `memory_context` | `Option<&str>` | 长期记忆注入（预留） |
| `soul` | `Option<&str>` | 人格/灵魂内容 |

### 组装顺序

```
1. <role>               — 身份与能力
2. <soul>               — 人格（可选，非空时才包含）
3. <thinking_style>     — 推理方法
4. <tool_system>        — 工具指导（仅有工具时包含）
5. <response_style>     — 输出格式
6. <context>            — 运行时上下文（日期、记忆）
7. <critical_reminders> — 不可违反的硬规则（最后=最高显著性）
```

**设计理念**：
- XML 标签包裹每个 section，提高 LLM 对结构化提示的理解
- Critical Reminders 放最后 — 利用 LLM 的**近因效应**（recency bias）确保硬规则最被重视
- 空 soul 自动跳过（`trim().is_empty()` 检查）
- 无工具时 `<tool_system>` 段完全不生成

[置信度：高]

## 3. Section 详解

### role.rs — 角色定义

> 📍 **代码位置**：`src/prompt/sections/role.rs`

- 默认名称 "Daedalus"，可通过 `DAEDALUS_AGENT_NAME` 自定义
- 有工具时自动添加 MCP 感知描述（"{count} external tool(s) via MCP"）
- 核心能力列表：问答、分析、编码、解释

### thinking.rs — 思考风格

> 📍 **代码位置**：`src/prompt/sections/thinking.rs`

- 步进式思考、任务分解、歧义时请求澄清
- 有工具时增加：调用前验证、结果评估、必要时重试
- 强调"思考是内部的，响应是外部的"

### tool_guidance.rs — 工具使用指导

> 📍 **代码位置**：`src/prompt/sections/tool_guidance.rs`

- **无工具时返回空字符串**（完全不生成此 section）
- 包含工具清单（name, server, description）
- 6 条使用准则：选对工具、验证参数、解释结果、处理错误、减少不必要调用、并行执行
- **关键规则**：不向用户暴露内部工具名或 MCP 协议细节

### response_style.rs — 响应风格

> 📍 **代码位置**：`src/prompt/sections/response_style.rs`

- 简洁清晰、自然语调、行动导向
- 代码必须用 fenced code block + 语言标识
- **语言一致性**：用户用什么语言就用什么语言回复

### context.rs — 动态上下文

> 📍 **代码位置**：`src/prompt/sections/context.rs`

- 当前日期（`YYYY-MM-DD, weekday` 格式）
- 可选的长期记忆注入（`<memory>` 块，目前预留）

### reminders.rs — 关键提醒

> 📍 **代码位置**：`src/prompt/sections/reminders.rs`

- 诚实 > 自信（不确定就说不确定）
- 安全第一、不编造引用/URL
- 有工具时：工具结果非最终答案、不暴露原始工具错误
- **必须始终给出可见响应**

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
