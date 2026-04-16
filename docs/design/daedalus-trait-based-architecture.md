# 设计决策：Trait 抽象 + 依赖注入架构

> 最后更新：2026-04-16
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代 + 记忆系统重构 + Skill 功能实现 + Workspace 系统实现 + 架构审查优化 + **五策略互斥记忆架构**
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
|## MCP 配置搜索链重构决策

**背景**：架构审查发现 `McpConfig::load()` 和 `McpConfig::load_with_workspace()` 之间有约 20 行重复代码（env var check + local path check + legacy home path check）。

**决策**：提取两个私有方法：
- `try_common_paths()` — 检查 env var 和 `./mcp.json`
- `try_legacy_home_path()` — 检查 `~/.config/daedalus/mcp.json`

两个公共方法都委托给这些辅助方法，`load_with_workspace()` 仅在中间插入 workspace 特有的搜索步骤。

**权衡**：
- ✅ 消除了 ~20 行重复代码，搜索链的每一步都有语义化方法名
- ✅ `load_with_workspace()` 的搜索链一目了然：`try_common_paths` → workspace → `try_legacy_home_path`
- ✅ 新增搜索步骤时只需修改一个辅助方法，不会忬记同步两个公共方法
- ✅ `load_with_workspace()` 的 CC 从 ~5 降到 ~3

## ToolInfo re-export 清理决策

**背景**：`llm/mod.rs` 中的 `ToolInfo` re-export 标记了 `#[deprecated]`，但项目没有外部消费者，deprecated 警告只是噪音。

**决策**：移除 `#[deprecated]` 属性，保留简洁的 re-export 以维持内部兼容性。

**权衡**：
- ✅ 消除了无意义的 deprecated 警告噪音
- ✅ 保留了 re-export，现有的 `use crate::llm::ToolInfo` 仍然有效
- ⚠️ 如果未来有外部 crate 依赖，可能需要重新添加 deprecated 标记

------|------|------|
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

## Memory::persist() 消除 shutdown downcast

**背景**：架构审查发现 `ChatAgent::shutdown()` 中通过 `session.memory_as::<SlidingWindowMemory>()` downcast 到具体类型来调用 `save_to_workspace()`，这打破了 Memory trait 的抽象——如果未来有第二种支持持久化的 Memory 实现，shutdown 代码需要修改。

**决策**：在 `Memory` trait 上新增 `persist(&self, workspace: &Workspace) -> Result<()>` 方法（默认 no-op），`SlidingWindowMemory` 实现它委托给 `save_to_workspace()`。`ChatAgent::shutdown()` 改为调用 `self.session.memory().persist(workspace)`，无需 downcast。

**权衡**：
- ✅ shutdown 不再依赖具体 Memory 类型，新增 Memory 实现时无需修改 shutdown 代码
- ✅ 默认 no-op 实现确保不支持持久化的策略无需实现空方法
- ✅ `ChatAgent` 不再 import `SlidingWindowMemory`，解除了对具体类型的编译时依赖
- ⚠️ Memory trait 新增了一个方法，但有默认实现，不影响现有实现者

## 原子写入持久化决策

**背景**：架构审查发现记忆持久化使用 `std::fs::write` 直接写入，如果进程在写入过程中崩溃，目标文件可能处于部分写入状态，导致数据损坏。

**决策**：在 `MemoryPersistence` 模块中新增 `atomic_write()` 工具函数，实现 write-to-temp-then-rename 模式：
1. 写入数据到 `<path>.tmp`
2. 原子重命名 `<path>.tmp` → `<path>`

`LongTermMemory::save()` 和 `HistoryEntry::save_all()` 均改用 `atomic_write()`。

**权衡**：
- ✅ 进程崩溃时目标文件要么是旧版本（重命名前）要么是新版本（重命名后），不会出现半写入状态
- ✅ 在大多数文件系统上 rename 是原子操作
- ⚠️ 如果 tmp 文件和目标文件不在同一文件系统，rename 可能失败（当前场景不会发生）

## 优雅关闭流程决策

**背景**：架构审查发现两个问题：(1) MCP 子进程在应用退出时未被关闭，可能产生孤儿进程；(2) `AgentMode::shutdown()` 是同步方法，无法 await MCP 的异步关闭。

**决策**：
1. 将 `AgentMode::shutdown()` 从同步改为 `async fn`
2. `ToolRouter` 新增 `shutdown()` 方法，委托给 `McpManager::shutdown()`
3. `ChatAgent::shutdown()` 依次执行：记忆持久化 → MCP 关闭
4. REPL 退出时调用 `agent.shutdown().await`

**权衡**：
- ✅ MCP 子进程不会成为孤儿进程
- ✅ shutdown 顺序明确：先持久化数据，再关闭外部进程
- ⚠️ `AgentMode` trait 的 `shutdown` 从同步变为异步，是破坏性变更（但当前只有一个实现者）

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

## Skill LLM 路由决策

**背景**：需要为 Agent 添加可扩展的"技能"能力——用户通过 Markdown 文件定义领域专用指令，Agent 在运行时按需使用。业界有三种主流方案：

| 方案 | 原理 | 优点 | 缺点 |
|------|------|------|------|
| **Prompt 注入** | 所有 skill 内容注入 system prompt | 简单直接 | 浪费 token，skill 越多越严重 |
| **关键词/语义匹配** | 根据用户输入自动匹配并激活 skill | 用户无感 | 需要额外的匹配逻辑，准确率不稳定 |
| **LLM 路由** | 将 skill 作为工具暴露，LLM 自主决定调用 | 零 token 浪费，复用现有工具调用机制 | 依赖 LLM 的工具调用能力 |

**决策**：选择 **LLM 路由**方案。将所有 skill 的名称和描述嵌入到 `use_skill` 工具的定义中（通过 `enum` 约束合法 skill 名称），LLM 根据用户请求自主决定是否调用以及调用哪个 skill。Skill 的完整 instructions 仅在被调用时才作为工具结果返回。

**权衡**：
- ✅ 零 token 浪费——skill instructions 不注入 system prompt，仅在 LLM 主动调用时展开
- ✅ 完全复用现有的 ToolRouter 路由机制，无需新增路由层
- ✅ Skill 数量增长时，只增加工具描述中的 skill 列表（几十字节/skill），不增加 instructions 的 token 消耗
- ✅ LLM 的工具调用能力已经非常成熟，路由准确率高
- ⚠️ 每次使用 skill 需要额外一轮工具调用（LLM 先调用 `use_skill`，再根据返回的 instructions 回复）
- ⚠️ 依赖 LLM 支持 function-calling（当前所有主流模型均支持）

## SkillTool 适配器模式决策

**背景**：Skill 系统最初在 `ToolRouter.execute()` 中通过硬编码的 `if tool_call.function_name == "use_skill"` 分支处理，打破了 BuiltinTool/MCP 的统一路由抽象。架构审查指出这违反了 ToolRouter 的设计初衷。

**决策**：将 `use_skill` 实现为 `BuiltinTool` trait 的适配器 `SkillTool`，通过 `Arc<SkillRegistry>` 共享 registry 引用，注册到 `BuiltinToolRegistry` 中。`ToolRouter.execute()` 恢复为纯粹的"内置优先 → MCP 回退"两级路由，无特殊分支。

**权衡**：
- ✅ ToolRouter 路由逻辑保持统一，无特殊分支
- ✅ `SkillTool` 覆写 `to_openai_json()` 生成包含 skill 目录的富描述，LLM 路由更准确
- ✅ `BuiltinToolRegistry` 新增 `register_tool()` 方法支持动态注册，为未来其他动态工具源预留扩展点
- ⚠️ 引入 `Arc<SkillRegistry>` 共享所有权，增加了少量间接层

## Skill 子目录约定决策

**背景**：Skill 文件组织方式有两种选择：扁平文件（`skills/code-review.md`）或子目录（`skills/code-review/SKILL.md`）。

**决策**：选择**子目录 + `SKILL.md`** 约定。每个 skill 是一个子目录，`SKILL.md` 是统一入口文件。

**权衡**：
- ✅ 每个 skill 可以包含多个资源文件（模板、示例代码、配置等），而非仅限于单个 Markdown
- ✅ `SKILL.md` 作为统一入口文件名，语义明确，便于工具链识别
- ✅ 目录名即 skill 名称，无需在文件内容中重复声明
- ⚠️ 比扁平文件多一层目录嵌套，创建新 skill 需要 `mkdir` + 创建文件两步

## 配置从环境变量迁移到 YAML 文件

**背景**：项目配置分散在 13+ 个环境变量中（`OPENAI_API_KEY`、`DAEDALUS_MODEL`、`DAEDALUS_LOG_*` 系列等），管理困难且不直观。`config.rs` 和 `logging.rs` 各自通过 `from_env()` 方法读取环境变量，代码重复且难以维护。

**决策**：将所有配置统一到 `<workspace>/config/daedalus.yaml` 文件中，使用 `serde_yaml` 反序列化。配置文件包含三个顶层段：`llm`（LLM Provider 配置）、`agent`（Agent 配置）、`logging`（日志配置）。

**权衡**：
- ✅ 所有配置集中在一个文件中，一目了然
- ✅ YAML 格式可读性好，支持注释，适合手动编辑
- ✅ 所有字段都有 `#[serde(default)]`，配置文件不存在时使用默认值正常启动
- ✅ `LlmConfig`、`VenusExtensions`、`ReasoningEffort` 等类型添加了 `Deserialize`，可直接从 YAML 反序列化
- ✅ 删除了所有 `from_env()` 方法和 `env_bool()` 辅助函数，减少了约 80 行环境变量解析代码
- ⚠️ 不再支持环境变量配置（MCP 配置除外，`DAEDALUS_MCP_CONFIG` 仍用于指定 mcp.json 路径）
- ⚠️ 新增了 `serde_yaml = "0.9"` 依赖

**YAML 格式选择原因**：用户明确要求使用 YAML 而非 TOML。YAML 在 DevOps 领域更常见，且 `serde_yaml` 与 `serde` 生态无缝集成。

## 模块化重构：config+logging 合并、session 移入 agent

**背景**：`src/` 外层积累了 4 个散落的源文件（`config.rs`、`logging.rs`、`session.rs`、`workspace.rs`），随着项目增长，模块边界不够清晰。分析依赖关系后发现：
- `session.rs` 只被 `agent/chat.rs` 和 `agent/mod.rs` 引用，是 agent 的内部概念
- `logging.rs` 从同一个 YAML 文件读取配置，与 `config.rs` 属于同一领域
- `workspace.rs` 被 6 个文件广泛引用，是基础设施，保持为单文件模块

**决策**：
1. `session.rs` → `agent/session.rs`：Session 移入 agent 模块，通过 `pub(crate) use session::Session` 对 crate 内部可见
2. `config.rs` + `logging.rs` → `config/` 目录模块：`config.rs` 变为 5 行的模块入口（re-export），实际代码在 `config/agent_config.rs` 和 `config/logging.rs`
3. `workspace.rs` 保持不变：职责单一（纯路径管理），被广泛引用，不需要拆分

**Rust 2024 edition 模块模式**：使用 `config.rs` + `config/` 子目录的方式（而非 `config/mod.rs`），这是 Rust 2024 edition 推荐的模块组织方式。

**引用路径变更**：
| 旧路径 | 新路径 |
|--------|--------|
| `crate::session::Session` | `crate::agent::Session`（内部用 `super::Session`） |
| `logging::LogConfig` | `config::LogConfig` |
| `logging::init()` | `config::init_logging()` |

**权衡**：
- ✅ `src/` 外层只剩 `main.rs`、`config.rs`（5 行入口）、`workspace.rs` 三个文件，结构清晰
- ✅ Session 归属于它的唯一消费者（agent 模块），符合"就近原则"
- ✅ config 和 logging 合并后，配置相关代码集中管理
- ⚠️ `config::init_logging` 的命名比原来的 `logging::init` 稍长，但语义更明确

## Subagent 作为内置工具决策（而非独立 AgentMode）

**背景**：需要为 Agent 添加“子代理”能力——将任务委托给运行在隔离上下文中的专用 agent。参考 Claude Code 的 Sub-agents 设计。有两种方案：

| 方案 | 原理 | 优点 | 缺点 |
|------|------|------|------|
| **独立 AgentMode** | 新增 `SubagentMode` 实现 `AgentMode` trait | 完全解耦 | 需修改 trait、CLI、REPL，工作量大 |
| **BuiltinTool 适配器** | 将 subagent 作为 `spawn_subagent` 内置工具注册 | 复用现有工具调用机制 | subagent 执行在工具调用循环内 |

**决策**：选择 **BuiltinTool 适配器**方案。将 subagent 作为 `spawn_subagent` 内置工具注册到 `ToolRouter`。

**权衡**：
- ✅ 与现有的 `SkillTool` 模式一致（LLM 自主决定何时调用）
- ✅ 不需要修改 `AgentMode` trait
- ✅ 主 agent 的工具调用循环自然支持 subagent 调用
- ✅ LLM 可以在一轮中同时调用多个 subagent（并行执行）
- ⚠️ subagent 执行时间可能较长，但通过 ToolEvent 透传提供实时进度

## Subagent 独立执行环境决策

**背景**：每次 subagent 调用是创建全新的执行环境，还是复用主 agent 的 session？

**决策**：每次调用创建全新的执行环境（独立 LLM provider + 独立工具集 + 独立对话历史）。

**权衡**：
- ✅ 完全的上下文隔离（Claude Code 的核心设计）
- ✅ subagent 不会看到主对话的历史
- ✅ 不同 subagent 可以用不同模型
- ✅ 执行完毕后自动释放资源
- ⚠️ 每次调用都有 LLM provider 创建开销（可忽略）

## Subagent 防递归决策

**背景**：subagent 是否可以再创建 subagent？

**决策**：subagent 的 `ToolRouter` 中**不注册** `spawn_subagent`、`spawn_team`、`use_skill`，通过 `EXCLUDED_TOOLS` 常量硬编码排除。

**权衡**：
- ✅ Claude Code 明确规定“子代理不能再创建子代理”
- ✅ 防止无限递归和资源耗尽
- ✅ 如需嵌套逻辑，使用 Skills 替代

## Subagent 文件格式与 Skill 的区别

**背景**：Subagent 和 Skill 都是用户可定义的扩展，但设计目标不同。

| 对比项 | Skill | Subagent |
|--------|-------|----------|
| 文件位置 | `skills/<name>/SKILL.md` | `agents/<name>.md` |
| 执行方式 | 注入 instructions 到当前对话 | 创建独立执行环境 |
| 上下文 | 共享主对话上下文 | 完全隔离 |
| 工具访问 | 继承主 agent 全部工具 | 可白名单/黑名单限制 |
| 模型 | 继承主 agent | 可指定不同模型 |
| 工具名 | `use_skill` | `spawn_subagent` / `spawn_team` |

## SubagentToolContext 共享状态提取决策

**背景**：`SubagentTool` 和 `TeamTool` 持有相同的三个 `Arc` 字段（registry、runner、shared_callback），存在结构性重复。

**决策**：提取 `SubagentToolContext` 共享结构体（原名 `SubagentToolBase`，后因 OOP 风格命名不符合 Rust 组合模式惯例而重命名），封装三个 `Arc` 字段和 `read_callback()`、`emit_start()`、`emit_complete()` 方法。两个 Tool 通过 `ctx` 字段引用。

**权衡**：
- ✅ 消除了字段重复和事件发射逻辑重复
- ✅ 工厂函数 `build_subagent_tool()` / `build_team_tool()` 从 registry 移到 tool.rs，消除了双向依赖
- ✅ `SubagentToolContext` 命名符合 Rust 组合模式惯例（`*Context` 而非 OOP 风格的 `*Base`）
- ⚠️ 引入了一层间接（`self.ctx.registry`），但可读性更好

## 内置 Subagent 硬编码决策

**背景**：内置 subagent（explore、code-reviewer、plan）最初以 `.md` 文件形式存在于 `.daedalus/agents/` 目录。但这意味着它们依赖文件系统，新用户首次使用时可能没有这些文件。

**决策**：将 3 个内置 subagent 硬编码在 `builtins.rs` 中，新增 `SubagentSource::Builtin` 变体（最低优先级）。`SubagentRegistry::register_builtins()` 在 `load_from_dir()` 之前调用，确保用户定义可覆盖内置。

**权衡**：
- ✅ 内置 subagent 始终可用，无需文件系统依赖
- ✅ 用户可通过同名 `.md` 文件覆盖内置行为
- ✅ 优先级链清晰：Builtin < Global < Project
- ⚠️ 修改内置 agent 需要重新编译

## 非交互模式（Print Mode）设计决策

**背景**：Daedalus 最初只有交互式 REPL 模式。参考 Claude Code 的 `--print` / `-p` 非交互模式，需要支持单次执行、管道输入、结构化输出等 CI/CD 和自动化场景。

**决策**：新增 `--print` / `-p` CLI 标志，通过 `clap` derive 解析命令行参数。非交互模式复用现有的 `AgentMode::chat()` 接口，不修改核心 trait。

**新增模块**：
- `cli/cli_args.rs` — CLI 参数定义（clap derive）
- `cli/output_format.rs` — 输出格式类型和序列化（text/json/stream-json）
- `cli/print_runner.rs` — 非交互模式执行器

**权衡**：
- ✅ `AgentMode` trait 不需要修改——非交互模式只是 `chat()` 的不同调用方式
- ✅ 输出格式化与 Agent 解耦，通过 `ToolEventCallback` 机制获取实时进度
- ✅ `text` 模式：结果到 stdout，进度到 stderr，符合 Unix 管道惯例
- ✅ `stream-json` 模式：NDJSON 格式，每行一个事件，易于解析
- ✅ `json` 模式：任务完成后输出单个 JSON 对象，适合脚本解析
- ⚠️ 新增了 `clap = "4"` 依赖

## CLI 参数解析库选择

**决策**：选择 `clap` (derive mode) 作为 CLI 参数解析库。

**权衡**：
- ✅ Rust 生态标准，类型安全，自动生成 `--help` / `--version`
- ✅ derive 模式代码简洁，参数定义即文档
- ✅ 支持 `ValueEnum` 自动解析枚举类型（如 `OutputFormat`）
- ⚠️ 增加了编译时间（clap 是较大的依赖）

## ToolFilter 工具过滤决策

**背景**：`--allowed-tools` 和 `--disallowed-tools` 需要在运行时过滤可用工具集。

**决策**：在 `ToolRouter` 中新增 `ToolFilter` 结构体和 `set_tool_filter()` 方法。过滤在三个层面生效：
1. `build_tool_definitions()` — LLM 看不到被过滤的工具
2. `tool_infos()` — CLI 显示和 prompt 构建不包含被过滤的工具
3. `execute()` — 即使 LLM 尝试调用被过滤的工具，也会返回错误

**过滤优先级**：allowlist > denylist。如果设置了 allowlist，只有列表中的工具可用；如果只设置了 denylist，列表中的工具被阻止。

**权衡**：
- ✅ 三层过滤确保安全性（LLM 看不到 + 执行时拦截）
- ✅ `ToolFilter` 是独立结构体，可在 ToolRouter 外部构造和测试
- ✅ 设置过滤器后自动重建 system prompt，确保 LLM 看到正确的工具集
- ⚠️ 过滤器是全局的，不支持按 round 动态调整

## Bare 模式决策

**背景**：CI/CD 场景下，skills、subagents、MCP 服务器的加载会增加启动时间。

**决策**：`--bare` 标志跳过所有自动发现（MCP 服务器连接、skills 加载、subagents 加载），只保留内置工具。

**权衡**：
- ✅ 大幅减少启动时间（跳过 MCP 连接、文件扫描）
- ✅ 适合简单查询和 CI/CD 场景
- ⚠️ 内置工具（read_file、write_file、bash 等）始终可用，不受 bare 模式影响

## max-turns 可配置决策

**背景**：`MAX_TOOL_ROUNDS` 原为硬编码常量 10，CI/CD 场景可能需要更多或更少的轮次。

**决策**：将 `MAX_TOOL_ROUNDS` 改为 `ChatAgent` 的可配置字段 `max_tool_rounds`，通过 `--max-turns` CLI 标志覆盖。值为 0 时使用内部默认值（10）。

**权衡**：
- ✅ 不修改 `AgentMode` trait，只在 `ChatAgent` 内部实现
- ✅ 默认行为不变（10 轮）
- ⚠️ 设置过大的值可能导致长时间运行和高 token 消耗

## `main.rs` 提取 `bootstrap()` 降低圈复杂度决策

**背景**：代码质量审查发现 `main()` 函数圈复杂度约 8，包含参数解析、workspace 解析、配置加载、日志初始化、MCP 连接、provider 创建、agent 创建、skill/subagent 加载、模式分发等 8 个逻辑阶段。

**决策**：将 `main()` 拆分为三个函数：
- `main()` — 仅负责模式分发（交互 vs 非交互），CC ≈ 3
- `bootstrap()` — 参数解析 + 配置加载 + 日志初始化 + agent 构建，返回 `(ChatAgent, CliArgs, LogGuard)`
- `build_agent()` — MCP 连接 + provider 创建 + agent 创建 + 扩展加载（skills/subagents/filters）

**权衡**：
- ✅ `main()` CC 从 ~8 降到 ~3，只关心"交互还是非交互"
- ✅ `bootstrap()` 和 `build_agent()` 各自职责单一
- ✅ `LogGuard` 作为返回值传出，确保在 `main()` 作用域内持有（避免 `std::mem::forget`）
- ⚠️ 多了两层函数调用，但对于启动流程这种一次性操作，开销可忽略

## `print_runner::run()` 提取辅助函数降低圈复杂度决策

**背景**：代码质量审查发现 `print_runner::run()` 函数圈复杂度约 9，主要原因是 `match result { Ok => match format { 3 }, Err => match format { 3 } }` 的双层嵌套 match（result × format = 6 个分支）。

**决策**：提取 `emit_success()` 和 `emit_error()` 两个辅助函数，每个函数内部只有一层 `match format`。`run()` 主体缩短为：`match result { Ok => emit_success(), Err => emit_error() }`。

**权衡**：
- ✅ `run()` CC 从 ~9 降到 ~4
- ✅ 成功/失败的输出逻辑各自内聚
- ✅ 每个辅助函数只关心"如何格式化输出"，不关心"执行是否成功"

## 共享 UTF-8 安全截断函数决策

**背景**：代码质量审查发现三个问题：(1) `render.rs` 和 `print_runner.rs` 中使用 `&s[..100]` 按字节截断，多字节 UTF-8 字符（中文、emoji）会导致 panic；(2) 项目中已有两个截断函数（`chat.rs` 的 `truncate_at_char_boundary` 和 `tool.rs` 的 `truncate_preview`），但 render 层没有使用；(3) `print_runner.rs` 中的工具输出截断逻辑与 `render.rs` 完全重复。

**决策**：在 `render.rs` 中新增两个 `pub(super)` 共享函数：
- `truncate_chars(s, max_chars) -> String` — UTF-8 安全的字符级截断
- `format_truncated_output(lines) -> Vec<String>` — 工具输出的头尾保留截断（纯函数，返回格式化行）

`print_runner.rs` 通过 `use super::render::{truncate_chars, format_truncated_output}` 复用。

**权衡**：
- ✅ 消除了 `&s[..N]` 的 panic 风险
- ✅ 消除了 `print_runner.rs` 中约 30 行重复的截断逻辑
- ✅ `format_truncated_output` 是纯函数（输入行 → 输出行），消费者自行决定输出到 stdout 还是 stderr
- ⚠️ `render.rs` 中的函数从纯私有变为 `pub(super)`，但仅限 cli 模块内部可见

## Dynamic Cheatsheet 记忆模块决策

**背景**：当前 LLM 在推理时是"无状态"的——每个查询独立处理，不会保留之前尝试中获得的洞察。模型会反复重新发现相同的解题策略，或反复犯同样的错误。

**论文引用**：[Dynamic Cheatsheet: Test-Time Learning with Adaptive Memory](https://arxiv.org/pdf/2504.07952) (Suzgun et al., 2025)

**决策**：新增 `DynamicCheatsheet` 作为 `SlidingWindowMemory` 的可选组件（`Option<DynamicCheatsheet>` 字段），与 `LongTermMemory` 和 `HistoryLog` 并列。DC 在每轮对话后通过 LLM 反思提取可复用的洞察（策略、错误模式、代码片段等），累积到结构化的 cheatsheet 中，并在下一次 LLM 调用时注入 system prompt。

**模块结构**：
- `src/memory/dynamic_cheatsheet/entry.rs` — `CheatsheetEntry` 条目数据结构
- `src/memory/dynamic_cheatsheet/config.rs` — `CheatsheetConfig` 配置
- `src/memory/dynamic_cheatsheet/cheatsheet.rs` — `DynamicCheatsheet` 核心引擎
- `src/memory/dynamic_cheatsheet/prompts.rs` — LLM prompt 模板

**集成方式**：
- DC 作为 `SlidingWindowMemory` 的 `PersistentComponents.cheatsheet` 字段（`Option<DynamicCheatsheet>`）
- `effective_system_prompt()` 同时注入 LongTermMemory 和 DynamicCheatsheet
- `SlidingWindowFactory::with_workspace_and_cheatsheet()` 支持从 workspace 加载
- `Memory::reflect_on_turn()` trait 方法在每轮对话后触发反思，`ChatAgent` 通过 trait 调用，无需 downcast
- `PersistentComponents` 聚合结构统一管理持久化状态的迁移和序列化

**权衡**：
- ✅ 作为可选组件，不影响未启用 cheatsheet 的场景
- ✅ 复用现有的 `effective_system_prompt()` 注入机制和 `MemoryPersistence` 持久化框架
- ✅ 反思失败不阻断主对话流程（fire-and-forget + warn 日志）
- ✅ 容量控制：`max_entries` + `max_token_budget` 双重限制，eviction 按 reinforcement_count 排序
- ✅ `reflect_on_turn()` 通过 trait 方法调用，`ChatAgent` 不依赖具体 Memory 实现
- ✅ `DynamicCheatsheet` 是纯数据结构（SRP），LLM 调用逻辑在 `SlidingWindowMemory::reflect_on_turn()` 中
- ✅ `PersistentComponents` 聚合结构消除了 downcast 链，新增持久化组件只需一行改动
- ✅ `CheatsheetConfig` 支持 `serde::Deserialize`，可接入 YAML 配置系统
- ⚠️ 每轮对话后额外 1 次 LLM 调用（反思），增加延迟和 token 消耗

## 三策略互斥记忆架构决策

**背景**：DC 和 A-MEM 最初分别作为 `SlidingWindowMemory` 的可选组件和独立引擎存在，但用户无法选择使用哪种记忆策略。需要将三种记忆组件作为互斥选项提供给用户。

**决策**：新增 `MemoryStrategy` 枚举（`sliding_window` | `dynamic_cheatsheet` | `agentic`），用户通过 `memory.strategy` YAML 配置选择。每种策略都是独立的 `Memory` trait 实现，配有对应的 `MemoryFactory`。

**新增类型**：
- `MemoryStrategy` 枚举（`config/agent_config.rs`）
- `EmbeddingConfig` 配置（`config/agent_config.rs`，顶层 YAML section）
- `CheatsheetMemory` + `CheatsheetFactory`（`memory/dynamic_cheatsheet/`）
- `AgenticMemory` + `AgenticFactory`（`memory/agentic/`）

**关键设计权衡**：

| 方面 | 选择 | 原因 |
|------|------|------|
| DC 作为独立策略时保留 SW 中的 DC 组件 | 保留 | SW+DC 组合提供最丰富的记忆能力，DC 独立策略是轻量替代 |
| EmbeddingConfig 放置位置 | 顶层 YAML section | embedding provider 未来可能被多个功能共享，不应嵌套在 memory 下 |
| Agentic 策略 embedding 失败处理 | 优雅降级到 sliding_window | 不阻断启动，error 日志提示用户 |
| 消息窗口限制 | `DEFAULT_MAX_MESSAGES = 100` | DC 和 Agentic 策略原先无窗口限制，长对话会 token 超限 |
| `EmbeddingConfig::create_provider()` 职责归属 | 配置类型自身 | 配置解析逻辑（env var fallback）属于配置层，不应放在 ChatAgent 中 |
| AgenticMemory 的 embedding 检索时机 | `reflect_on_turn` 预缓存 | `add_user_message` 是 sync 方法，在 tokio runtime 内 `block_on` 会死锁 |
| DC 反思逻辑去重 | `DynamicCheatsheet::reflect()` 共享方法 | SW 和 DC 独立策略的 `reflect_on_turn` 原先各有 20 行重复反思逻辑 |
| `sliding_window_factory()` 辅助方法 | 提取为 ChatAgent 私有方法 | `SlidingWindow` 分支和 `Agentic` fallback 分支共用，消除 6 行重复 |

**权衡**：
- ✅ 三种策略完全互斥，每个都是独立的 `Memory` 实现，符合开闭原则
- ✅ 向后兼容——不配置时默认 `sliding_window`，行为与改动前完全一致
- ✅ `SlidingWindowMemory` 中的可选 DC 组件保留不变
- ✅ `DEFAULT_MAX_MESSAGES` 提升到 `memory/mod.rs` 作为 `pub(crate)` 共享常量
- ⚠️ 每种策略需要实现完整的 `Memory` trait（~15 个方法），存在一定的样板代码

## Wiki Memory 设计决策

### Karpathy LLM Wiki 模式选择

**背景**：需要一种新的记忆策略，能够将对话知识结构化编译为可浏览、可编辑的知识库。参考 Andrej Karpathy 提出的 LLM Wiki 模式。

**决策**：实现 Karpathy 的三层架构（Raw/Wiki/Schema）和四阶段工作流（Ingest+Compile/Query/Lint），作为第四种互斥记忆策略。

**权衡**：
- ✅ “编译器模式”知识管理——每次交互持续增值，而非临时检索
- ✅ 结构化互联的 Wiki 页面，支持跨主题关联
- ✅ Lint 自检机制主动检测矛盾和断链
- ⚠️ 每轮对话额外 1-2 次 LLM 调用（Compile + 可选 Lint）

### Markdown 持久化而非 JSON

**背景**：初始方案使用 `store.json` 单文件序列化所有 Wiki 页面，偏离了 Karpathy “人机共建”的核心理念。

**决策**：每个 Wiki 页面是一个 `.md` 文件 + YAML frontmatter，完全兼容 Obsidian。纯机器数据（embedding 向量、lint 状态）存储在单独的 `_meta.json` 中。

**权衡**：
- ✅ 用户可直接用 Obsidian/VS Code 浏览和编辑 Wiki
- ✅ 每页独立 diff，版本控制友好
- ⚠️ 实现复杂度稍高（需解析 YAML frontmatter）
- ⚠️ `MemoryPersistence` trait 的 `path` 参数语义变为目录路径（而非文件路径）

### Embedding 可选（方案C）

**背景**：初始实现要求 embedding 是必须的，没有 embedding 就降级到 sliding_window。但 Wiki 的核心价值在于结构化知识编译 + wikilinks 互联，而非向量检索。

**决策**：让 embedding 成为可选增强。无 embedding 时使用关键词匹配 + wikilinks 遍历，有 embedding 时额外启用向量检索。

**权衡**：
- ✅ 降低使用门槛——不需要配置 embedding API 就能使用
- ✅ Wiki 策略不会因 embedding 服务不可用而降级到其他策略
- ⚠️ 关键词匹配的语义模糊匹配能力弱于向量检索

### WikiRetriever 抽取决策

**背景**：架构审查发现 `WikiStore` 承担了 6 种职责（God Object 倾向）。

**决策**：将检索逻辑提取为独立的 `WikiRetriever` 模块。`WikiStore` 回归存储 + 持久化职责。

**权衡**：
- ✅ `WikiStore` 从 1029 行降到 709 行，职责单一
- ✅ 检索策略可独立演进和测试
- ✅ 停用词表通过 `LazyLock` 缓存

### Ingest + Compile 合并为单次 LLM 调用

**背景**：Karpathy 原方案中 Ingest 和 Compile 是两个独立阶段。

**决策**：合并为单次 LLM 调用，LLM 同时提取知识并决定如何更新 Wiki。

**权衡**：
- ✅ 每轮对话只需 1 次 LLM 调用（而非 2 次）
- ⚠️ 单次调用的 prompt 较长，但总 token 消耗仍低于两次调用

## ACE Memory 设计决策

### ACE（Agentic Context Engineering）策略选择

**背景**：Dynamic Cheatsheet 的扁平条目列表在长期使用中存在两个问题：(1) LLM 倾向于生成简洁摘要，丢弃领域洞察（Brevity Bias）；(2) 迭代重写导致细节逐渐侵蚀（Context Collapse）。

**论文引用**：[Agentic Context Engineering: Evolving Contexts for Self-Improving Language Models](https://arxiv.org/abs/2510.04618) (Stanford/SambaNova/UC Berkeley)

**决策**：实现 ACE 论文的 Online 模式——每轮对话后实时运行 Reflect→Curate，增量更新 Playbook 注入下一轮。核心创新是 **Reflector/Curator 分离**：LLM 只产出小的 delta entries，确定性 Curator 负责合并，LLM 永远不会重写整个 Playbook。

**权衡**：
- ✅ 层次化 Playbook（Section→Bullet）比 DC 的扁平列表更有组织性
- ✅ 确定性 Curator 防止上下文坍塌——LLM 不重写全文
- ✅ 不需要 embedding，是纯 LLM 反思策略
- ✅ 每轮仅 1 次 LLM 调用（与 DC 相同）
- ⚠️ 每轮对话后额外 1 次 LLM 调用（反思），增加延迟和 token 消耗

### Reflector/Curator 分离模式

**背景**：Dynamic Cheatsheet 的 `apply_reflection_response()` 混合了 LLM 响应解析和数据操作。ACE 论文提出将反思过程拆分为两个独立组件。

**决策**：
- **Reflector**（LLM）：调用 LLM 分析对话，产出结构化 `DeltaEntry` 枚举值
- **Curator**（确定性）：接收 `Vec<DeltaEntry>`，用纯确定性逻辑合并到 Playbook

`DeltaEntry` 枚举作为两者之间的契约（IR），将 LLM 输出解析和数据操作完全解耦。

**权衡**：
- ✅ Reflector 和 Curator 可独立测试（34 个单元测试）
- ✅ Curator 的确定性逻辑保证了合并行为的可预测性
- ✅ 未来可替换 Reflector 的 LLM 调用方式而不影响 Curator
- ✅ 这种模式可作为未来重构 DynamicCheatsheet 的参考

### MessageBuffer 组合模式

**背景**：架构审查发现 4 个策略（CheatsheetMemory、AgenticMemory、WikiMemory、AceMemory）都有几乎相同的消息管理代码（~30 行/策略）：`messages: Vec<ChatMessage>` + `max_messages: usize` + `windowed_messages()` + `add_user/assistant_message()` + `build_messages()` + `turn_count()` + `clear()`。

**决策**：提取 `MessageBuffer` 结构体到 `memory/mod.rs`，使用组合（embedding as field）而非继承。所有 4 个策略迁移为 `buffer: MessageBuffer` 字段。

**权衡**：
- ✅ 消除了 ~120 行跨 4 个策略的重复代码
- ✅ 组合模式保持了每个策略 `Memory` impl 的完全控制权
- ✅ `build_messages_with_system()` 封装了 system prompt 拼接逻辑
- ⚠️ 引入了一层间接（`self.buffer.add_user()` 而非 `self.messages.push()`），但可读性更好

### 共享工具函数提取

**背景**：架构审查发现多处跨模块代码重复：
1. `strip_directive_prefix()` 在 `cheatsheet.rs` 和 `reflector.rs` 中完全相同
2. `CHARS_PER_TOKEN` 常量在 `playbook.rs` 和 `cheatsheet.rs` 中重复定义
3. `to_markdown()` 的 token budget 截断逻辑在两个文件中几乎相同

**决策**：提取到 `memory/mod.rs` 作为 `pub(crate)` 共享工具：
- `strip_directive_prefix()` — 大小写不敏感的指令前缀匹配
- `CHARS_PER_TOKEN` — 字符/token 比例常量
- `truncate_to_token_budget()` — 按 token 预算在行边界截断文本

**权衡**：
- ✅ 消除了 3 处跨模块代码重复
- ✅ 截断逻辑集中维护，修改一处即可
- ✅ `pub(crate)` 可见性限制了使用范围

### Curator `with_bullet_mut` 辅助方法

**背景**：代码设计审查发现 `update_bullet`、`reinforce_bullet`、`remove_bullet` 三个方法有几乎相同的 `find_section_mut` → `bullet_by_index_mut` → 操作/warn 嵌套结构（~45 行重复代码）。

**决策**：提取 `with_bullet_mut(playbook, section, index, op_name, action)` 辅助方法，接受 `impl FnOnce(&mut Bullet)` 闭包。UPDATE 和 REINFORCE 改为在 `apply_deltas` 的 match 分支中直接调用 `with_bullet_mut`。REMOVE 因操作对象不同（`Vec::remove` 而非 `&mut Bullet`）保留为独立方法。

**权衡**：
- ✅ 消除了 ~30 行结构性重复代码
- ✅ 日志消息通过 `op_name` 参数统一，减少了硬编码字符串
- ⚠️ REMOVE 无法使用此模式（操作对象是 Vec 而非 Bullet）

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-16 | 新增 ACE Memory 设计决策（Agentic Context Engineering 策略选择、Reflector/Curator 分离模式、MessageBuffer 组合模式、共享工具函数提取、with_bullet_mut 辅助方法） | ACE Memory 实现 + 代码质量审查优化 |
| 2026-04-16 | 新增 Wiki Memory 设计决策（Karpathy LLM Wiki 模式、Markdown 持久化、Embedding 可选、WikiRetriever 抽取、Ingest+Compile 合并） | Wiki Memory 实现 |
| 2026-04-16 | 新增三策略互斥记忆架构决策（MemoryStrategy 枚举、EmbeddingConfig 独立配置、CheatsheetMemory/AgenticMemory 独立实现、block_on 死锁修复、DynamicCheatsheet::reflect 共享方法、DEFAULT_MAX_MESSAGES 提升、sliding_window_factory 辅助方法） | 三策略互斥架构 + 代码质量审查 |
| 2026-04-15 | 更新 Dynamic Cheatsheet 设计决策：Memory trait 新增 reflect_on_turn 方法消除 downcast、提取 PersistentComponents 聚合结构、DynamicCheatsheet 拆分为纯数据结构、CheatsheetConfig 添加 Deserialize | DC 架构审查优化 |
| 2026-04-15 | 新增 Dynamic Cheatsheet 记忆模块设计决策 | Dynamic Cheatsheet 实现 |
| 2026-04-15 | 新增 main.rs bootstrap 提取、print_runner 辅助函数提取、共享 UTF-8 安全截断函数三个设计决策；更新 SubagentToolBase 重命名为 SubagentToolContext | 代码质量审查优化 |
| 2026-04-15 | 新增非交互模式（Print Mode）、CLI 参数解析、ToolFilter、Bare 模式、max-turns 可配置五个设计决策 | 非交互模式实现 |
| 2026-04-15 | 新增 Subagent 相关设计决策（BuiltinTool 适配器、独立执行环境、防递归、Skill 对比、SubagentToolBase、SharedEventCallback、内置硬编码） | Subagent 功能实现 |
| 2026-04-14 | 新增 YAML 配置迁移、模块化重构两个设计决策 | YAML 配置迁移 + 模块化重构 |
| 2026-04-14 | 新增 MCP 配置搜索链重构、ToolInfo re-export 清理两个设计决策；更新原子写入覆盖范围（新增 AgenticMemoryStore） | 架构审查优化 |
| 2026-04-14 | 新增 Memory::persist() 消除 shutdown downcast、原子写入持久化、优雅关闭流程三个设计决策 | Workspace 系统实现 + 架构审查优化 |
| 2026-04-13 | 新增 Skill LLM 路由、SkillTool 适配器模式、Skill 子目录约定三个设计决策 | Skill 功能实现 |
| 2026-04-13 | 新增 ToolRound 结构体引入、PersistentState 封装两个设计决策 | A-MEM 实现 + 代码审查 |
| 2026-04-13 | 新增 Memory 双层架构重构、持久化迁移模式、effective_system_prompt 动态注入三个设计决策；更新 Memory trait 描述 | 记忆系统重构 |
| 2026-04-09 | 新增 ToolInfo 迁移、ToolEvent 回调、工具并行化三个设计决策 | 工具事件/并行化迭代 |
| 2026-04-08 | 新增 ToolRouter 抽取决策和 BuiltinTool trait 设计 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
