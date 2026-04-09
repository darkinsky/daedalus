# Agent — Agent 模式抽象、ChatAgent 与 ToolRouter

> 最后更新：2026-04-09
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代

## 1. 模块概述

Agent 模块定义了统一的 Agent 模式接口（`AgentMode` trait）和当前唯一的实现 `ChatAgent`。`ChatAgent` 负责多轮对话编排，包括消息管理、LLM 调用和工具调用循环。工具调用通过 `ToolRouter` 统一路由，支持内置工具和 MCP 外部工具。

## 2. AgentMode Trait

> 📍 **代码位置**：`src/agent/mod.rs`

```rust
#[async_trait]
pub trait AgentMode: Send + Sync {
    async fn chat(
        &mut self,
        user_input: &str,
        on_tool_event: Option<&ToolEventCallback>,
    ) -> Result<ChatResponse>;
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
| `tool_router` | `ToolRouter` | 统一工具路由器（内置 + MCP） |
| `prompt_override` | `Option<String>` | 自定义覆盖提示词 |
| `agent_name` / `soul` | `Option<String>` | 个性化配置 |

### MemoryFactory 设计

```rust
type MemoryFactory = Box<dyn Fn(&str) -> Box<dyn Memory> + Send + Sync>;
```

使用工厂函数而非泛型参数，允许运行时动态创建不同的 Memory 实现。虽有轻微运行时开销，但使架构更灵活。默认工厂创建 `SlidingWindowMemory::unlimited()`。[置信度：高]

### 工具调用循环

> 📍 **代码位置**：`src/agent/chat.rs:267-360`

```
用户消息 → LLM 请求（带工具定义 + 历史）
  → LLM 返回 tool_calls?
    YES → 发送 RoundStart 事件
        → 发送所有 ToolCallStart 事件
        → 通过 ToolRouter 并行执行所有工具（futures::future::join_all）
        → 发送所有 ToolCallComplete 事件
        → 发送 RoundComplete 事件
        → 收集 ToolResponse → tool_history.push()
        → 继续循环（最多 MAX_TOOL_ROUNDS = 10 轮）
    NO  → 返回最终文本响应（累计 token usage）
```

**关键约束**：
- `MAX_TOOL_ROUNDS = 10` — 防止 LLM 无限循环调用工具
- Token usage 跨轮次累加
- Reasoning content 从中间轮次保留到最终响应
- 工具调用是**并行执行**的（`futures::future::join_all`），总耗时 = max(各工具耗时)
- 工具调用摘要存储时，参数截断到 200 字符，结果截断到 500 字符，防止大工具输出膨胀记忆

### ToolEvent 回调机制

> 📍 **代码位置**：`src/agent/mod.rs`

`ChatAgent` 在工具调用循环中通过回调通知 CLI 层实时渲染工具执行进度。

**事件类型**（4 种）：

| 事件 | 时机 | 携带信息 |
|------|------|----------|
| `RoundStart` | 新一轮工具调用开始 | 轮次号（1-based） |
| `ToolCallStart` | 单个工具即将执行 | 工具名、来源（built-in/mcp） |
| `ToolCallComplete` | 单个工具执行完成 | 工具名、成功/失败、结果预览（80 字符） |
| `RoundComplete` | 一轮所有工具执行完成 | 工具调用数量 |

**回调类型**：`Arc<dyn Fn(ToolEvent) + Send + Sync>` — 使用 `Arc` 包装以跨 async 边界共享。

**设计权衡**：
- ✅ 回调作为 `Option` 参数传入 `chat()`，不影响无工具场景
- ✅ CLI 层在回调中暂停 spinner → 输出事件 → 恢复 spinner，避免输出交错
- ⚠️ 回调参数污染了 `AgentMode` trait 签名（已知的架构权衡，未来可改为 channel 注入）

[置信度：高]

### 工具上下文存储

工具调用摘要通过 `memory.add_tool_context()` 存储，而非注入假的 user 消息。这避免了扭曲 `turn_count` 和对话历史。[置信度：高]

### 提示词重建

`attach_mcp()` 调用后会触发 `reset_with_updated_prompt()`：重新组装系统提示词（含工具描述）并**重建 Session**。这通常发生在启动时、对话开始前。[置信度：高]

## 4. ToolRouter — 统一工具路由器

> 📍 **代码位置**：`src/agent/tool_router.rs`

ToolRouter 是 ChatAgent 和工具执行之间的中间层，将工具调用路由职责从 ChatAgent 中抽离出来。

### 设计动机

引入 ToolRouter 的原因是 ChatAgent 原来直接持有 `Option<McpManager>` 和 `BuiltinToolRegistry`，导致工具路由逻辑（内置优先 vs MCP 回退）和工具定义聚合逻辑散落在 ChatAgent 的多个方法中。提取为独立组件后：
- ChatAgent 只关心“有工具吗”和“执行工具”，不关心工具来源
- 新增工具源（如 HTTP API 工具）只需修改 ToolRouter，不触及 ChatAgent

### 路由优先级

```
tool_call → ToolRouter.execute()
  ├─ 内置工具注册表中找到？ → 直接执行（无进程开销）
  ├─ MCP 服务器中找到？ → 通过 McpManager 执行
  └─ 都找不到 → 返回错误 ToolResponse
```

### 内置工具（BuiltinToolRegistry）

> 📍 **代码位置**：`src/tools/mod.rs` + `src/tools/fs.rs`

内置工具通过 `BuiltinTool` trait 定义，与 MCP 工具使用相同的 OpenAI function-calling JSON 格式，对 LLM 完全透明。

当前内置工具：

| 工具名 | 功能 | 关键参数 |
|--------|------|--------|
| `read_file` | 读取文件内容，支持行号和分页 | `path`, `offset?`, `limit?` |
| `write_file` | 写入文件（自动创建父目录） | `path`, `content` |
| `list_directory` | 列出目录内容，支持递归 | `path`, `recursive?`, `max_entries?` |
| `search_files` | 按文件名模式搜索 | `path`, `pattern`, `max_results?` |
| `get_file_info` | 获取文件/目录元数据 | `path` |

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-09 | 工具调用改为并行执行（futures::join_all）；新增 ToolEvent 回调机制；AgentMode::chat() 签名增加 on_tool_event 参数 | 工具事件/并行化迭代 |
| 2026-04-08 | 新增 ToolRouter、BuiltinTool 架构；更新字段命名和工具调用流程 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
