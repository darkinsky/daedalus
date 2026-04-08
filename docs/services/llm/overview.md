# LLM — Provider 抽象与双实现

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

LLM 模块是 Daedalus 的 LLM 接入层，定义了 Provider 无关的 `LlmApi` trait 和完整的类型系统，提供两个 Provider 实现：`GenAiProvider`（基于 genai 库）和 `VenusProvider`（基于原始 HTTP 请求）。

## 2. LlmApi Trait

> 📍 **代码位置**：`src/llm/mod.rs`

```rust
#[async_trait]
pub trait LlmApi: Send + Sync {
    async fn chat(&self, messages, options) -> Result<ChatResponse> { ... }  // 默认委托
    async fn chat_with_tools(&self, messages, tools, tool_history, options) -> Result<ChatResponse>;
    fn supports_tools(&self) -> bool { false }
    fn model_name(&self) -> &str;
    fn provider_name(&self) -> &str;
}
```

**设计决策**：
- `chat()` 的默认实现委托给 `chat_with_tools(messages, &[], &[], options)`，减少实现者负担
- 工具定义使用 `serde_json::Value`（OpenAI JSON 格式），而非强类型 — 保持灵活性
- 工具历史为 `&[(Vec<ToolCall>, Vec<ToolResponse>)]` 切片，让 Provider 自行转换格式

[置信度：高]

## 3. Provider 工厂

> 📍 **代码位置**：`src/llm/mod.rs:78-90`

```rust
pub fn create_provider(config: LlmConfig) -> Result<Box<dyn LlmApi>>
```

选择逻辑：
- `thinking_enabled` 或 `thinking_tokens` 有值 → **VenusProvider**
- 否则 → **GenAiProvider**

**原因**：genai 库的 OpenAI adapter 不支持 Venus 扩展参数（`thinking_enabled`/`thinking_tokens`），需要用原始 HTTP 请求才能完全控制请求体。[置信度：高]

## 4. GenAiProvider

> 📍 **代码位置**：`src/llm/genai_provider.rs`

基于 `genai` 库的适配器实现，支持多 Provider：

| 适配器 | 配置值 |
|--------|--------|
| OpenAI（默认） | `openai` |
| Anthropic | `anthropic` |
| Gemini | `gemini` 或 `google` |
| Groq | `groq` |
| Cohere | `cohere` |

### 类型转换层

所有 genai 类型转换完全封装在 Provider 内部，外部代码只看到自有类型：
- `convert_messages()` — 我们的 → genai（Tool 角色映射为 assistant）
- `to_genai_tool_call()` / `from_genai_tool_call()` — 双向 ToolCall 转换
- `json_to_genai_tool()` — OpenAI JSON → genai Tool
- `build_response()` — genai 响应 → 我们的 ChatResponse

### Reasoning Content 捕获

始终启用 `capture_reasoning_content` 和 `normalize_reasoning_content`。对非 reasoning 模型是 no-op，但确保 reasoning 模型的思考过程被捕获。[置信度：高]

## 5. VenusProvider

> 📍 **代码位置**：`src/llm/venus_provider.rs`

直接 HTTP 请求的 Venus API 代理实现，给予完全的请求体控制权。

### 与 GenAiProvider 的区别

| 特性 | GenAiProvider | VenusProvider |
|---|---|---|
| 实现方式 | genai 库适配器 | 原始 reqwest HTTP |
| Venus 扩展 | 仅 `reasoning_effort` | 全部支持 |
| Thinking 提取 | genai 内置 | `reasoning_content` 字段 + `<think>` 标签回退 |
| 工具历史格式 | genai ChatMessage 类型 | OpenAI 格式 JSON |

### Venus 扩展参数合并

支持两级参数：config 级默认 + request 级覆盖。通过 `VenusExtensions::merge_with_overrides()` 实现。

### Reasoning 提取双策略

1. 优先检查 `reasoning_content` 字段（Venus 代理标准）
2. 回退到 `<think>...</think>` 标签提取（DeepSeek-R1 风格）

[置信度：高]

## 6. 类型系统

> 📍 **代码位置**：`src/llm/types.rs`

| 类型 | 用途 |
|---|---|
| `ReasoningEffort` | Low/Medium/High 枚举 |
| `VenusExtensions` | Venus 扩展参数（thinking_enabled/tokens, reasoning_effort） |
| `LlmConfig` | Provider 配置（api_key, model, api_base, adapter_kind, venus） |
| `ChatMessage` / `ChatRole` | 会话消息（System/User/Assistant/Tool） |
| `ToolCall` / `ToolResponse` | 工具调用请求/响应 |
| `ChatResponse` | LLM 响应（content, reasoning_content, usage, tool_calls） |
| `TokenUsage` | Token 统计（全部 Option<u64>） |
| `ChatOptions` | 生成参数（temperature, max_tokens, top_p, venus） |
| `ToolInfo` | CLI 显示用工具描述 |

**TokenUsage::accumulate()** 智能处理 `None` 值：`None + Some(x) = Some(x)`，`None + None = None`。[置信度：高]

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
