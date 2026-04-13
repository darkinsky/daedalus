# 设计决策：Trait 抽象 + 依赖注入架构

> 最后更新：2026-04-13
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代 + 记忆系统重构
> 置信度：高

## 决策概述

Daedalus 在三个核心抽象点采用了 Trait + Trait Object（`Box<dyn T>`）的设计，通过依赖注入实现模块解耦。

## 三大核心 Trait

### 1. AgentMode — Agent 模式抽象

> 📍 **代码位置**：`src/agent/mod.rs`

**Why**：预留未来扩展更多 Agent 模式（如带规划能力的 agent mode）。CLI 层通过 `&mut dyn AgentMode` 交互，完全不关心具体实现。

**当前实现**：仅 `ChatAgent`（多轮对话 + 工具调用）。

### 2. LlmApi — LLM Provider 抽象

> 📍 **代码位置**：`src/llm/mod.rs`

**Why**：支持多种 LLM Provider（GenAI 库适配器 vs Venus 原始 HTTP），且可能随时新增 Provider。所有 Provider 特定类型（genai 类型、reqwest 类型）完全封装在 Provider 内部，外部代码只使用自有类型。

**当前实现**：`GenAiProvider`（genai 库）、`VenusProvider`（reqwest HTTP）。

**工厂模式**：`llm::create_provider()` 根据配置自动选择 Provider。

### 3. Memory — 记忆策略抽象

> 📍 **代码位置**：`src/memory/mod.rs`

**Why**：记忆策略是高度可变的维度（全量、滑动窗口、摘要、RAG），需要在不修改 Agent 代码的情况下切换策略。

**当前实现**：`SlidingWindowMemory`（双层记忆架构：热数据层 LongTermMemory + 冷数据层 HistoryLog + 滑动窗口 + 自动整合）。

**MemoryFactory 模式**：`ChatAgent` 持有 `Box<dyn Fn(&str) -> Box<dyn Memory>>` 工厂函数，而非泛型参数，允许运行时动态创建不同实现。

**`as_any` downcast 模式**：Memory trait 提供 `as_any()` / `as_any_mut()` 方法，使 Agent 层可以 downcast 到具体类型访问策略特定功能（整合、历史搜索、持久化迁移），而不污染基础 trait 接口。

## 设计权衡

| 方面 | 选择 | 原因 |
|------|------|------|
| Trait Object vs 泛型 | Trait Object | 需要运行时多态（Provider 选择取决于配置），泛型会导致类型膨胀 |
| MemoryFactory | `Box<dyn Fn>` | 比泛型更灵活，允许在运行时根据配置创建不同策略 |
| Memory 策略特定功能 | `as_any` downcast | 避免 trait 膨胀，不支持整合的策略无需实现大量空方法 |
| 工具定义格式 | `serde_json::Value` | OpenAI JSON 作为通用中间格式，各 Provider 各自转换 |
| 错误类型 | `anyhow::Result` | 项目规模不大，不需要自定义错误类型的精确匹配 |

## OpenAI JSON 作为中间格式

工具定义（`ToolDefinition`）和工具历史在模块间传递时使用 OpenAI function-calling JSON 格式：

```
MCP ToolDefinition → to_openai_json() → serde_json::Value → Provider 各自转换
```

这避免了创建额外的中间类型，同时保持了各 Provider 的实现自由度。

## ToolRouter 抽取决策

**背景**：引入内置工具（`BuiltinToolRegistry`）后，ChatAgent 需要同时管理内置工具和 MCP 工具。如果直接在 ChatAgent 中持有两个工具源，工具路由逻辑（内置优先 vs MCP 回退）和工具定义聚合逻辑会散落在 ChatAgent 的多个方法中，违反单一职责原则。

**决策**：提取 `ToolRouter` 作为独立组件（`src/agent/tool_router.rs`），封装所有工具源的管理和路由。

**权衡**：
- ✅ ChatAgent 只关心“有工具吗”和“执行工具”，不关心工具来源
- ✅ 新增工具源（如 HTTP API 工具）只需修改 ToolRouter，不触及 ChatAgent
- ✅ `execute_and_log()` 辅助方法消除了 Ok/Err 日志的重复模式
- ⚠️ 多了一层间接调用，但对于工具调用这种 IO 密集操作，开销可忽略

## BuiltinTool Trait 设计

**背景**：需要一种方式定义内置工具，使其与 MCP 工具对 LLM 完全透明。

**决策**：定义 `BuiltinTool` trait（`src/tools/mod.rs`），每个工具实现 `name()`、`description()`、`input_schema()`、`execute()` 四个方法。工具定义通过 `to_openai_json()` 转换为与 MCP 相同的 OpenAI function-calling JSON 格式。

**权衡**：
- ✅ 与 MCP 工具使用相同的 JSON 格式，LLM 无法区分工具来源
- ✅ 新增内置工具只需实现 trait 并注册到 `BuiltinToolRegistry`
- ✅ 内置工具始终可用，无需外部 MCP 配置
- ⚠️ 当前工具注册是硬编码的（`BuiltinToolRegistry::new()` 中列举所有工具），未来可考虑动态注册

## ToolInfo 归属迁移决策

**背景**：架构审查发现 `ToolInfo` 定义在 `llm/types.rs` 中，但它描述的是“工具”而非“LLM”，导致 `tools`、`prompt`、`mcp` 等模块反向依赖 `llm` 模块。

**决策**：将 `ToolInfo` 的 canonical 定义迁移到 `tools/mod.rs`，在 `llm/mod.rs` 中通过 `pub use crate::tools::ToolInfo` 重新导出，保持向后兼容。

**权衡**：
- ✅ `tools/mod.rs` 不再反向依赖 `llm` 模块
- ✅ 所有现有的 `use crate::llm::ToolInfo` 仍然有效（通过 re-export）
- ✅ 语义上 `ToolInfo` 现在归属于它真正描述的领域（工具）
- ⚠️ Rust 模块系统允许跨模块 re-export，不存在循环依赖问题

## ToolEvent 回调机制决策

**背景**：工具执行过程对用户完全不可见——`chat_with_tools` 在内部循环执行工具调用，但 CLI 层只看到最终的 `ChatResponse`，中间的工具调用过程被 spinner 遮盖。

**决策**：在 `agent/mod.rs` 中定义 `ToolEvent` 枚举（4 种事件）和 `ToolEventCallback` 类型别名，通过 `AgentMode::chat()` 的可选参数传入回调。

**权衡**：
- ✅ 回调作为 `Option` 参数，不影响无工具场景的调用路径
- ✅ `Arc<dyn Fn(ToolEvent) + Send + Sync>` 跨 async 边界安全共享
- ✅ CLI 层在回调中协调 spinner 暂停/恢复，避免输出交错
- ⚠️ 回调参数污染了 `AgentMode` trait 签名（编排层被 UI 关注点污染）
- ⚠️ 未来如有多种前端（CLI、Web、API），应考虑改为 `tokio::sync::mpsc` channel 注入模式

## 工具调用并行化决策

**背景**：架构审查发现同一轮中多个工具调用串行执行，当 LLM 同时请求多个独立工具（如同时读取 3 个文件）时，总耗时 = sum(各工具耗时)。

**决策**：使用 `futures::future::join_all` 并行执行同一轮的所有工具调用。

**权衡**：
- ✅ 总耗时从 `sum(各工具耗时)` 降低为 `max(各工具耗时)`，对 I/O 密集型工具提升显著
- ✅ 选择 `futures::join_all` 而非 `tokio::task::JoinSet`，因为 `ToolRouter::execute` 需要 `&self` 引用，而 `ToolRouter` 不是 `'static`，无法直接 spawn
- ✅ 事件发射顺序保持一致：所有 Start 先发出，并行执行，所有 Complete 后发出
- ⚠️ 新增了 `futures = "0.3"` 依赖

## Memory 双层架构重构决策

**背景**：原有的 `SlidingWindowMemory` 仅支持简单的滑动窗口（保留最近 N 条消息），无法在长对话中保持关键上下文。用户偏好、项目背景等重要信息会随窗口滑动而丢失。

**决策**：将 Memory 从单层滑动窗口重构为双层架构：
- **热数据层（LongTermMemory）**：结构化关键事实（用户偏好、项目上下文、重要决策、笔记），自动注入 system prompt
- **冷数据层（HistoryLog）**：追加式事件摘要（时间戳 + 摘要 + 关键词），按需通过 `search_history()` 搜索
- **整合机制**：当未整合消息数超过阈值时触发，将旧消息压缩为 `ConsolidationResult`（热数据更新 + 冷数据追加）

**权衡**：
- ✅ 关键上下文不再随窗口滑动丢失，长期记忆始终可见
- ✅ 冷数据不占用 token 预算，仅在需要时搜索
- ✅ 整合是渐进式的（游标推进），不需要一次性处理全部历史
- ⚠️ 整合需要额外的 LLM 调用（当前预留接口，尚未接入实际 LLM 整合）
- ⚠️ `as_any` downcast 失去编译时类型安全

## 持久化状态迁移模式

**背景**：Session 重建（MCP 附加、新会话命令）会创建新的 Memory 实例，但长期记忆和历史日志需要跨 session 保留。

**决策**：设计对称的 `take_persistent_state()` / `restore_persistent_state()` API 对，并在 `ChatAgent` 中提取 `create_session_with_migration()` 作为唯一的迁移执行点。

**权衡**：
- ✅ `reset_with_updated_prompt()` 和 `new_session()` 不再重复迁移逻辑（DRY）
- ✅ 对称 API 命名（take/restore）语义清晰
- ✅ 不支持持久化的 Memory 实现会静默跳过（downcast 返回 None）
- ⚠️ 迁移逻辑硬编码了对 `SlidingWindowMemory` 的 downcast

## `effective_system_prompt` 动态注入模式

**背景**：长期记忆需要对 LLM 可见，但不应修改原始 system prompt（因为整合可能随时更新长期记忆内容）。

**决策**：在 `build_messages()` 时通过 `effective_system_prompt()` 将 `LongTermMemory.to_markdown()` 动态拼接到 `base_system_prompt` 末尾。

**权衡**：
- ✅ 整合更新长期记忆后，下一次 LLM 调用自动看到最新内容，无需重建 session
- ✅ `base_system_prompt` 保持不变，便于调试和日志
- ⚠️ 每次 `build_messages()` 都会重新拼接字符串（性能影响可忽略）

## `ToolRound` 结构体引入决策

**背景**：代码审查发现 `chat_with_tools` 和 `LlmApi::chat_with_tools` 的 `tool_history` 参数使用裸元组 `(Vec<ToolCall>, Vec<ToolResponse>)`，语义不明确——读者需要查看调用处才能理解 `.0` 是 calls、`.1` 是 responses。

**决策**：引入命名结构体 `ToolRound { calls, responses }` 替代裸元组，定义在 `llm/types.rs` 中，通过 `llm/mod.rs` re-export。

**影响范围**：
- `LlmApi` trait 签名（`tool_history: &[ToolRound]`）
- `GenAiProvider::chat_with_tools`
- `VenusProvider::chat_with_tools` + `build_request_body`
- `ChatAgent::chat_with_tools` + `summarize_tool_history`

**权衡**：
- ✅ 调用处语义清晰：`round.calls` / `round.responses` 替代 `.0` / `.1`
- ✅ 未来可在 `ToolRound` 上扩展字段（如 `round_index`、`duration`）
- ⚠️ 跨 4 个文件的签名变更，但每处改动都是机械化的

## `PersistentState` 封装决策

**背景**：`PersistentState(pub Box<dyn Any + Send>)` 的 `pub` 字段暴露了内部实现，外部代码可以直接构造 `PersistentState(Box::new(anything))`，绕过类型安全。

**决策**：将字段改为 `pub(crate)`，新增 `PersistentState::new()` 和 `PersistentState::downcast()` 方法，提供类型安全的构造和解构 API。

**权衡**：
- ✅ 外部代码无法绕过类型安全直接构造
- ✅ `downcast()` 返回 `Result<T, Self>`，失败时可恢复原始状态
- ✅ `SlidingWindowMemory` 的 trait impl 更简洁：`PersistentState::new((ltm, log))` 替代 `PersistentState(Box::new((ltm, log)))`

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-13 | 新增 ToolRound 结构体引入、PersistentState 封装两个设计决策 | A-MEM 实现 + 代码审查 |
| 2026-04-13 | 新增 Memory 双层架构重构、持久化迁移模式、effective_system_prompt 动态注入三个设计决策；更新 Memory trait 描述 | 记忆系统重构 |
| 2026-04-09 | 新增 ToolInfo 迁移、ToolEvent 回调、工具并行化三个设计决策 | 工具事件/并行化迭代 |
| 2026-04-08 | 新增 ToolRouter 抽取决策和 BuiltinTool trait 设计 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
