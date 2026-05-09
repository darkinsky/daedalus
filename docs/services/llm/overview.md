# LLM — Unified Provider with Adapter Pattern

> 最后更新：2026-05-09
> 来源：重构 — 移除 genai 依赖，统一为 reqwest-based provider

## 1. 模块概述

LLM 模块是 Daedalus 的 LLM 接入层，定义了 Provider 无关的 `LlmApi` trait 和完整的类型系统。
使用统一的 `LlmProvider`（基于 reqwest HTTP）配合 `ApiAdapter` 策略模式支持多种 LLM API。

## 2. 架构

```
LlmProvider (HTTP transport, SSE stream parsing, error handling)
    └── ApiAdapter (format-specific logic)
        ├── OpenAiAdapter   — OpenAI, Venus proxy, DeepSeek, compatible APIs
        ├── AnthropicAdapter — Anthropic Messages API (direct)
        └── GeminiAdapter   — Google Gemini API (direct)
```

## 3. LlmApi Trait

> 📍 **代码位置**：`src/llm/mod.rs`

```rust
#[async_trait]
pub trait LlmApi: Send + Sync {
    async fn chat(&self, messages, options) -> Result<ChatResponse> { ... }
    async fn chat_with_tools(&self, messages, tools, tool_history, options) -> Result<ChatResponse>;
    async fn chat_with_tools_stream(&self, messages, tools, tool_history, options) -> Result<Receiver<StreamChunk>>;
    fn supports_tools(&self) -> bool { false }
    fn model_name(&self) -> &str;
    fn provider_name(&self) -> &str;
}
```

## 4. Provider 工厂

> 📍 **代码位置**：`src/llm/mod.rs`

```rust
pub fn create_provider(config: LlmConfig) -> Result<Box<dyn LlmApi>>
```

统一创建 `LlmProvider`，根据 `adapter_kind` 配置选择对应的 adapter。

## 5. ApiAdapter Trait

> 📍 **代码位置**：`src/llm/adapter/mod.rs`

```rust
pub trait ApiAdapter: Send + Sync {
    fn endpoint(&self, base_url: &str, model: &str) -> String;
    fn headers(&self, api_key: &str) -> HeaderMap;
    fn build_body(&self, model, messages, tools, tool_history, options, config_venus) -> Value;
    fn parse_response(&self, body: &Value) -> Result<ChatResponse>;
    fn parse_stream_event(&self, data: &str) -> Option<StreamChunk>;
    fn stream_done_signal(&self) -> &str;
    fn name(&self) -> &str;
}
```

## 6. Adapter 实现

### OpenAI Adapter (`adapter/openai.rs`)

默认 adapter，处理所有 OpenAI 兼容 API：
- OpenAI 原生 API
- Venus 代理（统一格式）
- DeepSeek（支持 `reasoning_content` 回传）
- 任何 OpenAI 兼容的第三方 API

特性：
- `cache_control` 支持（content block 格式）
- Venus 扩展参数（`thinking_enabled`/`thinking_tokens`/`reasoning_effort`）
- `<think>` 标签回退解析（DeepSeek-R1 风格）
- Prompt caching 优化（标记 stable round 为 cache boundary）

### Anthropic Adapter (`adapter/anthropic.rs`)

直连 Anthropic Messages API：
- System 消息作为顶层字段
- Tool use 通过 content blocks（`tool_use`/`tool_result`）
- `x-api-key` + `anthropic-version` 认证
- Extended thinking 支持（`thinking.budget_tokens`）

### Gemini Adapter (`adapter/gemini.rs`)

直连 Google Gemini API：
- `contents` 数组 + `parts` 格式
- `functionCall`/`functionResponse` 工具调用格式
- `systemInstruction` 顶层字段
- `x-goog-api-key` 认证
- `thinkingConfig` 支持

## 7. 类型系统

> 📍 **代码位置**：`src/llm/types.rs`

| 类型 | 用途 |
|---|---|
| `ReasoningEffort` | Low/Medium/High 枚举 |
| `VenusExtensions` | 扩展参数（thinking_enabled/tokens, reasoning_effort） |
| `LlmConfig` | Provider 配置（api_key, model, api_base, adapter_kind, venus） |
| `ChatMessage` / `ChatRole` | 会话消息（System/User/Assistant/Tool） |
| `ToolCall` / `ToolResponse` | 工具调用请求/响应 |
| `ToolRound` | 一轮工具调用（calls + responses + reasoning_content） |
| `ChatResponse` | LLM 响应（content, reasoning_content, usage, tool_calls） |
| `TokenUsage` | Token 统计（含 cached_tokens） |
| `ChatOptions` | 生成参数（temperature, max_tokens, top_p, venus） |
| `StreamChunk` / `StreamAccumulator` | 流式响应类型 |
| `CacheControl` | 缓存控制标记 |

## 8. 配置示例

```yaml
llm:
  api_key: "sk-..."
  model: "gpt-4o"
  api_base: "https://api.openai.com/v1"
  adapter_kind: "openai"  # "openai", "anthropic", "gemini", "deepseek"
  venus:
    thinking_enabled: true
    thinking_tokens: 4096
    reasoning_effort: "high"
```

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-05-09 | 移除 genai 依赖，统一为 reqwest-based LlmProvider + ApiAdapter 模式 | Provider 合并重构 |
| 2026-04-09 | ToolInfo 迁移到 `tools/mod.rs` | ToolInfo 归属优化 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
