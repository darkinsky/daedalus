# Agent — Agent 模式抽象与 ChatAgent 实现

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

Agent 模块定义了统一的 Agent 模式接口（`AgentMode` trait）和当前唯一的实现 `ChatAgent`。`ChatAgent` 负责多轮对话编排，包括消息管理、LLM 调用和 MCP 工具调用循环。

## 2. AgentMode Trait

> 📍 **代码位置**：`src/agent/mod.rs`

```rust
#[async_trait]
pub trait AgentMode: Send + Sync {
    async fn chat(&mut self, user_input: &str) -> Result<ChatResponse>;
    fn attach_mcp(&mut self, _mcp: McpManager) {}  // 默认空实现
    fn has_tools(&self) -> bool { false }
    fn tool_count(&self) -> usize { 0 }
    fn tool_descriptions(&self) -> Vec<ToolInfo> { vec![] }
    fn new_session(&mut self);
    fn session(&self) -> &Session;
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;
    fn mode_name(&self) -> &str;
}
```

**设计模式**：模板方法 + 策略模式。`attach_mcp`、`has_tools` 等方法有默认实现，不支持工具的 Agent 模式无需重写。[置信度：高]

**扩展点**：注释中提到未来可能增加"full agent mode with planning and multi-step execution"。[置信度：中]

## 3. ChatAgent

> 📍 **代码位置**：`src/agent/chat.rs`

### 核心字段

| 字段 | 类型 | 用途 |
|---|---|---|
| `llm` | `Box<dyn LlmApi>` | LLM 提供商（trait object） |
| `session` | `Session` | 当前会话（含记忆） |
| `system_prompt` | `String` | 当前系统提示词 |
| `memory_factory` | `MemoryFactory` | 记忆工厂函数 |
| `mcp` | `Option<McpManager>` | 可选 MCP 管理器 |
| `custom_system_prompt` | `Option<String>` | 自定义覆盖提示词 |
| `agent_name` / `soul` | `Option<String>` | 个性化配置 |

### MemoryFactory 设计

```rust
type MemoryFactory = Box<dyn Fn(&str) -> Box<dyn Memory> + Send + Sync>;
```

使用工厂函数而非泛型参数，允许运行时动态创建不同的 Memory 实现。虽有轻微运行时开销，但使架构更灵活。默认工厂创建 `SlidingWindowMemory::unlimited()`。[置信度：高]

### 工具调用循环

> 📍 **代码位置**：`src/agent/chat.rs:267-333`

```
用户消息 → LLM 请求（带工具定义 + 历史）
  → LLM 返回 tool_calls?
    YES → 逐个执行 MCP 工具 → 收集 ToolResponse → tool_history.push()
        → 继续循环（最多 MAX_TOOL_ROUNDS = 10 轮）
    NO  → 返回最终文本响应（累计 token usage）
```

**关键约束**：
- `MAX_TOOL_ROUNDS = 10` — 防止 LLM 无限循环调用工具
- Token usage 跨轮次累加
- Reasoning content 从中间轮次保留到最终响应
- 工具调用是**串行执行**的（逐个遍历 `response.tool_calls`）

### 工具上下文存储

工具调用摘要通过 `memory.add_tool_context()` 存储，而非注入假的 user 消息。这避免了扭曲 `turn_count` 和对话历史。[置信度：高]

### 提示词重建

`attach_mcp()` 调用后会触发 `rebuild_prompt()`：重新组装系统提示词（含工具描述）并**重建 Session**。这通常发生在启动时、对话开始前。[置信度：高]

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
