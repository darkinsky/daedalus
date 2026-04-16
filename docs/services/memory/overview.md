# Memory — 三策略互斥记忆系统

> 最后更新：2026-04-16
> 来源：存量代码分析 + 记忆系统重构 + 代码质量审查 + A-MEM 实现 + Workspace 持久化优化 + Dynamic Cheatsheet 实现 + **三策略互斥架构**

## 1. 模块概述

Memory 模块定义了会话记忆的统一接口（`Memory` trait）和三种**互斥**的记忆策略，用户通过 YAML 配置选择：

- **`SlidingWindowMemory`**（默认）：双层记忆架构，热数据层 + 冷数据层 + 滑动窗口 + 自动整合 + 可选 Dynamic Cheatsheet
- **`CheatsheetMemory`**：独立的 Dynamic Cheatsheet 策略，轻量级自适应记忆，每轮对话后 LLM 反思提取可复用洞察
- **`AgenticMemory`**：独立的 A-MEM 知识图谱策略，embedding 向量检索 + 记忆演化 + 上下文预缓存

### 策略选择配置

```yaml
# daedalus.yaml
memory:
  strategy: sliding_window  # sliding_window | dynamic_cheatsheet | agentic

# Embedding provider (top-level, only used by agentic strategy)
embedding:
  api_key: "sk-..."          # Falls back to OPENAI_API_KEY env var
  api_base: "https://..."    # Falls back to OPENAI_BASE_URL env var
  model: "text-embedding-3-small"
  dimensions: 1536
```

### 策略选择流程

```mermaid
graph TD
    YAML["daedalus.yaml<br/>memory.strategy"] --> CONFIG["AgentConfig<br/>memory_strategy: MemoryStrategy"]
    CONFIG --> FACTORY["ChatAgent::create_memory_factory()"]
    
    FACTORY -->|sliding_window| SW["SlidingWindowFactory<br/>(default, with cheatsheet)"]
    FACTORY -->|dynamic_cheatsheet| DC["CheatsheetFactory"]
    FACTORY -->|agentic| AG["AgenticFactory<br/>(needs EmbeddingConfig)"]
    
    AG -->|"create_provider() fails"| FALLBACK["fallback → SlidingWindowFactory"]
    
    SW --> MEM["Box&lt;dyn Memory&gt;"]
    DC --> MEM
    AG --> MEM
    FALLBACK --> MEM
```

**关键设计决策**：
- 三种策略完全互斥，每个都是独立的 `Memory` 实现
- `MemoryStrategy` 枚举定义在 `config/agent_config.rs`，通过 `#[serde(rename_all = "snake_case")]` 支持 YAML
- `EmbeddingConfig` 作为**顶层** YAML section（独立于 memory），因为 embedding provider 未来可能被多个功能共享
- Agentic 策略在 embedding provider 创建失败时**优雅降级**到 `sliding_window`
- 不配置 `memory.strategy` 时默认 `sliding_window`，向后兼容

此外，`src/embedding/` 模块提供了 `Embedding` trait 抽象和 OpenAI 实现，为 A-MEM 的向量检索提供基础设施。

## 2. Memory Trait

> 📍 **代码位置**：`src/memory/mod.rs`

```rust
pub trait Memory: Send + Sync {
    fn add_user_message(&mut self, content: &str);
    fn add_assistant_message(&mut self, content: &str);
    fn add_tool_context(&mut self, context: &str) { self.add_assistant_message(context); }
    fn build_messages(&self) -> Vec<ChatMessage>;
    fn clear(&mut self);
    fn should_consolidate(&self) -> bool { false }
    fn turn_count(&self) -> usize;
    fn strategy_name(&self) -> &str;
    fn take_persistent_state(&mut self) -> Option<PersistentState> { None }
    fn restore_persistent_state(&mut self, _state: PersistentState) { /* warn + discard */ }
    fn persist(&self, _workspace: &Workspace) -> Result<()> { Ok(()) }
    fn reflect_on_turn(&mut self, user_input: &str, assistant_response: &str, llm: &dyn LlmApi) -> Pin<Box<dyn Future<Output = ()>>>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
```

**设计要点**：
- `add_tool_context()` 有默认实现（委托给 `add_assistant_message`），区分工具上下文和普通助手消息
- `should_consolidate()` 有默认实现（返回 `false`），不支持整合的策略无需重写
- `persist()` 有默认 no-op 实现，在 shutdown 时调用，无需 downcast 到具体类型即可持久化记忆状态
- `reflect_on_turn()` 在每轮对话后触发反思（默认 no-op），支持 Dynamic Cheatsheet 等自适应记忆模块提取可复用洞察
- `take_persistent_state()` / `restore_persistent_state()` 支持跨 session 迁移
- `as_any()` / `as_any_mut()` 提供 downcast 能力，使 Agent 层可以访问策略特定功能

**`as_any` downcast 设计决策**：

引入 `as_any` 的原因是双层记忆引入了大量策略特定方法（`search_history`、`messages_to_consolidate`、`take_persistent_state` 等），如果全部放入 `Memory` trait 会导致 trait 膨胀，且不支持整合的策略需要提供大量空实现。通过 downcast，基础 trait 保持精简（11 个方法），策略特定功能通过 `as_any_mut().downcast_mut::<SlidingWindowMemory>()` 按需访问。

**权衡**：
- ✅ 基础 trait 不被策略特定方法污染
- ✅ 新增 Memory 实现无需实现不相关的方法
- ⚠️ downcast 失去了编译时类型安全，调用方需要处理 `None` 情况
- ⚠️ 如果未来有第二种支持持久化的 Memory 实现，所有 downcast 点都需要扩展

[置信度：高]

## 3. SlidingWindowMemory — 双层记忆引擎

> 📍 **代码位置**：`src/memory/sliding_window.rs`

### 结构

```
SlidingWindowMemory {
    base_system_prompt: String,       // 原始系统提示词（不含记忆注入）
    messages: Vec<ChatMessage>,       // 完整对话消息历史
    persistent: PersistentComponents, // 聚合持久化组件
    consolidation_cursor: usize,      // 整合游标
    config: SlidingWindowConfig,      // 窗口与整合配置
}

PersistentComponents {
    long_term_memory: LongTermMemory,        // 热数据：结构化关键事实
    history_log: Vec<HistoryEntry>,          // 冷数据：事件摘要日志
    cheatsheet: Option<DynamicCheatsheet>,   // 可选：动态速查表
}
```

**`PersistentComponents` 聚合结构**：将所有跨 session 持久化的组件聚合到一个结构体中，简化 `take_persistent_state()` / `restore_persistent_state()` / `persist()` 的实现——它们都操作同一个结构体，新增持久化组件只需一行改动。

**命名说明**：字段 `messages`（而非 `history`）与 `build_messages()` 和 `windowed_messages()` 语义对齐，避免与 `history_log` 混淆。字段 `consolidation_cursor`（而非 `last_consolidated`）消除了"最后一条已合并"vs"第一条未合并"的歧义——它是一个游标，指向第一条未合并消息的索引。

### 双层数据流

```mermaid
graph TD
    subgraph "热数据层 (Hot)"
        LTM[LongTermMemory<br/>结构化关键事实]
    end
    subgraph "冷数据层 (Cold)"
        HL[HistoryLog<br/>事件摘要日志]
    end
    subgraph "对话消息"
        MSG[messages<br/>完整对话历史]
    end

    MSG -->|"整合触发"| CONSOLIDATE[ConsolidationResult]
    CONSOLIDATE -->|"memory_update"| LTM
    CONSOLIDATE -->|"history_entry"| HL

    LTM -->|"注入 system prompt"| BUILD[build_messages]
    MSG -->|"窗口裁剪"| BUILD

    HL -->|"按需搜索"| SEARCH[search_history]

    subgraph "自适应记忆 (Adaptive)"
        DC[DynamicCheatsheet<br/>可复用洞察]
    end

    MSG -->|"每轮反思"| REFLECT[reflect_on_turn]
    REFLECT -->|"LLM 提取洞察"| DC
    DC -->|"注入 system prompt"| BUILD
```

## 4. Agentic Memory (A-MEM) — 知识图谱记忆引擎

> 📍 **代码位置**：`src/memory/agentic/`
> 📄 **论文**：A-MEM (arxiv:2502.12110)
> ✅ **状态**：已集成为独立记忆策略（`memory.strategy: agentic`）

### 设计动机

SlidingWindowMemory 的双层架构解决了"关键事实不丢失"的问题，但它的知识组织是**扁平的**——长期记忆只是分类列表，缺乏知识之间的关联。A-MEM 引入了**知识图谱**的概念：每条记忆是一个带有丰富元数据的节点（MemoryNote），节点之间通过语义相似性建立双向链接，形成可演化的知识网络。

### 三阶段生命周期

A-MEM 的核心是论文中定义的三阶段记忆生命周期：

```mermaid
graph LR
    INPUT[原始内容] --> P1[Phase 1: Note Construction]
    P1 -->|"LLM 提取元数据"| NOTE[MemoryNote]
    P1 -->|"Embedding 生成向量"| NOTE
    NOTE --> P2[Phase 2: Link Generation]
    P2 -->|"余弦相似度 + LLM 验证"| LINKS[双向链接]
    LINKS --> P3[Phase 3: Memory Evolution]
    P3 -->|"LLM 更新关联节点元数据"| EVOLVED[演化后的知识图谱]
```

1. **Note Construction**：原始内容 → LLM 提取 keywords/tags/context → Embedding 模型生成向量 → 创建 `MemoryNote`
2. **Link Generation**：余弦相似度检索候选节点 → LLM 验证语义关联 → 建立双向链接
3. **Memory Evolution**：新链接建立后 → LLM 重新分析关联节点的元数据 → 更新 keywords/tags/context 以反映高阶知识模式

### 模块结构

```
src/memory/agentic/
├── mod.rs          # 模块入口，re-export
├── note.rs         # MemoryNote — 原子知识单元（Zettelkasten 风格）
├── store.rs        # AgenticMemoryStore — 三阶段引擎 + 检索
├── memory.rs       # AgenticMemory — 独立 Memory trait 实现
└── factory.rs      # AgenticFactory — MemoryFactory 实现
src/embedding/
├── mod.rs          # Embedding trait + cosine_similarity()
└── openai.rs       # OpenAI text-embedding-3-small 实现
```

### MemoryNote 结构

每个 note 是一个自包含的知识单元：

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | `Uuid` | 唯一标识 |
| `content` | `String` | 原始内容 |
| `keywords` | `Vec<String>` | LLM 提取的关键词 |
| `tags` | `Vec<String>` | LLM 提取的分类标签 |
| `context` | `String` | LLM 生成的语义描述 |
| `embedding` | `Vec<f32>` | 向量表示（用于相似度检索） |
| `linked_notes` | `HashSet<Uuid>` | 双向链接（知识图谱边） |
| `created_at` / `updated_at` | `DateTime<Local>` | 时间戳 |

### AgenticMemoryStore 配置常量

| 常量 | 默认值 | 说明 |
|------|--------|------|
| `DEFAULT_SIMILARITY_THRESHOLD` | 0.5 | 链接候选的最低余弦相似度 |
| `DEFAULT_MAX_LINK_CANDIDATES` | 5 | 每次链接生成检索的最大候选数 |
| `DEFAULT_RETRIEVAL_LIMIT` | 5 | 上下文检索返回的最大 note 数 |

### Embedding Trait

> 📍 **代码位置**：`src/embedding/mod.rs`

```rust
#[async_trait]
pub trait Embedding: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> usize;
    fn model_name(&self) -> &str;
}
```

`embed_batch` 有默认实现（顺序调用 `embed`），支持批量 API 的 Provider 可覆盖以提升性能。`cosine_similarity()` 作为模块级函数提供，用于向量相似度计算。

### Prompt 模板分离

A-MEM 的三个 LLM 交互阶段各有独立的 prompt 模板，从业务逻辑中提取为模块级常量和构造函数：

| 常量/函数 | 用途 |
|-----------|------|
| `METADATA_SYSTEM_PROMPT` | 元数据提取的 system prompt |
| `LINK_VALIDATION_SYSTEM_PROMPT` | 链接验证的 system prompt |
| `EVOLUTION_SYSTEM_PROMPT` | 记忆演化的 system prompt |
| `metadata_extraction_prompt()` | 构造元数据提取的 user prompt |
| `link_validation_prompt()` | 构造链接验证的 user prompt |
| `evolution_prompt()` | 构造记忆演化的 user prompt |

**设计决策**：将 prompt 模板与业务逻辑分离，便于调整措辞、支持多语言或 A/B 测试不同 prompt，无需修改核心引擎代码。

### AgenticMemory — 独立 Memory 实现

> 📍 **代码位置**：`src/memory/agentic/memory.rs`

`AgenticMemory` 是 A-MEM 的独立 `Memory` trait 实现，管理自己的消息列表和 system prompt 注入。

**工作流程**（避免 async-in-sync 死锁）：
1. `add_user_message()` — 纯同步，只追加消息（不做 embedding 检索）
2. `build_messages()` — 注入上一轮预缓存的 `cached_context` 到 system prompt
3. `reflect_on_turn()` — **异步**，执行两步：
   - Step 1: 将 assistant 响应存储为新 memory note（触发 A-MEM 三阶段生命周期）
   - Step 2: 用当前 `user_input` 预检索相关记忆，缓存到 `cached_context` 供**下一轮**使用

**消息窗口**：`max_messages = 100`（`DEFAULT_MAX_MESSAGES`），防止长对话 token 超限。

**`Arc<dyn Embedding>` 设计**：embedding provider 通过 `Arc` 共享，因为 `AgenticFactory` 持有引用并分发 clone 给每个 memory 实例。

### 待完成工作

- [ ] 考虑将 LLM 编排逻辑从 `AgenticMemoryStore` 中拆分（存储 vs 编排职责分离）

[置信度：高]

## 5. Dynamic Cheatsheet — 自适应记忆模块

> 📍 **代码位置**：`src/memory/dynamic_cheatsheet/`
> 📄 **论文**：[Dynamic Cheatsheet: Test-Time Learning with Adaptive Memory](https://arxiv.org/pdf/2504.07952) (Suzgun et al., 2025)
> ✅ **状态**：可作为 SlidingWindowMemory 的可选组件，也可作为独立记忆策略（`memory.strategy: dynamic_cheatsheet`）

### 设计动机

当前 LLM 在推理时是"无状态"的——每个查询独立处理，不会保留之前尝试中获得的洞察。模型会反复重新发现相同的解题策略，或反复犯同样的错误。Dynamic Cheatsheet（DC）通过在每轮对话后进行 LLM 反思，提取可复用的洞察（策略、错误模式、代码片段等），累积到结构化的速查表中，并在下一次 LLM 调用时注入 system prompt。

### 生命周期

```mermaid
graph LR
    INJECT[1. Inject<br/>注入 system prompt] --> LLM[LLM 响应]
    LLM --> REFLECT[2. Reflect<br/>反思提取洞察]
    REFLECT --> UPDATE[3. Update<br/>合并/更新/淘汰]
    UPDATE --> INJECT
```

1. **Inject**：`effective_system_prompt()` 将 cheatsheet 渲染为 Markdown 注入 system prompt
2. **Reflect**：`Memory::reflect_on_turn()` 调用 LLM 分析本轮对话，提取新洞察
3. **Update**：`apply_reflection_response()` 解析 LLM 响应，合并新条目、更新已有条目、淘汰低价值条目

### 模块结构

```
src/memory/dynamic_cheatsheet/
├── mod.rs          # 模块入口，re-export
├── entry.rs        # CheatsheetEntry — 单条洞察条目
├── config.rs       # CheatsheetConfig — 容量/淘汰/反思配置
├── cheatsheet.rs   # DynamicCheatsheet — 核心引擎（数据操作 + 共享反思方法）
├── prompts.rs      # LLM prompt 模板（system + user）
├── memory.rs       # CheatsheetMemory — 独立 Memory trait 实现
└── factory.rs      # CheatsheetFactory — MemoryFactory 实现
```

### CheatsheetEntry 结构

| 字段 | 类型 | 说明 |
|------|------|------|
| `category` | `String` | 分类（strategy / error_pattern / code_snippet / best_practice / domain_knowledge） |
| `content` | `String` | 简洁可操作的洞察描述 |
| `reinforcement_count` | `u32` | 被强化（使用/验证）的次数 |
| `created_at` / `updated_at` | `DateTime<Local>` | 时间戳 |

### CheatsheetConfig 配置

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `max_entries` | 50 | 最大条目数，超出时触发淘汰 |
| `max_token_budget` | 2000 | 渲染为 Markdown 时的最大 token 预算（~4 chars/token） |
| `auto_reflect` | true | 是否在每轮对话后自动反思 |
| `min_reinforcement_for_retention` | 1 | 淘汰时的最低强化次数阈值 |

### 淘汰策略（两阶段）

当条目数超过 `max_entries` 时触发淘汰：

1. **Phase 1**：优先淘汰 `reinforcement_count < min_reinforcement_for_retention` 的条目
2. **Phase 2**：如果仍超容量，按 `reinforcement_count ASC, updated_at ASC` 排序，从前端移除

### 反思协议

LLM 反思响应遵循结构化文本协议：

```
NEW: <category> | <content>           # 新增条目
UPDATE: <number> | <refined_content>  # 更新已有条目（1-based 编号）
NO_NEW_INSIGHTS                       # 无新洞察
```

解析由 `parse_reflection_response()` 完成，内部委托给 `parse_new_directive()` 和 `parse_update_directive()` 两个辅助方法。

### 集成方式

**作为 SlidingWindowMemory 组件**（`memory.strategy: sliding_window`）：
- DC 作为 `SlidingWindowMemory` 的 `PersistentComponents.cheatsheet` 字段（`Option<DynamicCheatsheet>`）
- `effective_system_prompt()` 同时注入 LongTermMemory 和 DynamicCheatsheet 的 Markdown
- `SlidingWindowFactory::with_workspace_and_cheatsheet()` 支持从 workspace 加载

**作为独立策略**（`memory.strategy: dynamic_cheatsheet`）：
- `CheatsheetMemory` 管理自己的消息列表（含 `max_messages = 100` 窗口限制）
- `CheatsheetFactory` 从 workspace 加载持久化的 cheatsheet
- 独立管理 `effective_system_prompt()` 注入

**共享反思机制**：
- `DynamicCheatsheet::reflect()` 是所有调用方的共享入口（消除 DRY 违反）
- `SlidingWindowMemory::reflect_on_turn()` 委托给 `cheatsheet.reflect()`
- `CheatsheetMemory::reflect_on_turn()` 委托给 `self.cheatsheet.reflect()`
- `ChatAgent` 通过 `Memory::reflect_on_turn()` trait 方法调用，无需 downcast
- 反思失败不阻断主对话流程（fire-and-forget + warn 日志）
- `apply_reflection_response()` 是 `reflect()` 的 data-only counterpart，用于测试

### Prompt 模板分离

| 常量/函数 | 用途 |
|-----------|------|
| `REFLECTION_SYSTEM_PROMPT` | 反思的 system prompt |
| `reflection_user_prompt()` | 构造反思的 user prompt（含当前 cheatsheet + 本轮对话） |

[置信度：高]

### build_messages() 逻辑

1. 通过 `effective_system_prompt()` 将 `LongTermMemory` 和 `DynamicCheatsheet` 动态注入到 `base_system_prompt` 末尾
2. 系统消息始终在首位
3. 如果 `max_messages` 为 None → 返回全部消息
4. 如果 `max_messages` 为 Some(n) → 取最后 n 条消息（`windowed_messages()`）

**重要**：长期记忆和 cheatsheet 注入发生在 `build_messages()` 时，而非修改 `base_system_prompt`。这意味着整合更新长期记忆或反思更新 cheatsheet 后，下一次 LLM 调用自动看到最新内容，无需重建 session。[置信度：高]

### 整合（Consolidation）机制

> 📍 **代码位置**：`src/memory/sliding_window.rs` + `src/memory/consolidation.rs`

**触发条件**：`unconsolidated_count() >= config.consolidation_threshold`

**整合流程**：
1. `messages_to_consolidate()` 返回需要整合的消息切片（从 `consolidation_cursor` 到 `messages.len() - retention_window`）
2. 外部（Agent 层）调用 LLM 生成 `ConsolidationResult`
3. `apply_consolidation()` 将结果应用：
   - `history_entry` 追加到 `history_log`（冷数据）
   - `memory_update` 替换 `long_term`（热数据）
   - 推进 `consolidation_cursor` 游标

**ConsolidationResult DTO**：
```rust
pub struct ConsolidationResult {
    pub history_entry: HistoryEntry,   // 2-5 句事件摘要
    pub memory_update: LongTermMemory, // 完整替换的长期记忆
}
```

### 持久化状态迁移

> 📍 **代码位置**：`src/memory/sliding_window.rs` + `src/agent/chat.rs`

当 session 重建时（MCP 附加、新会话），长期记忆和历史日志需要跨 session 迁移：

```
旧 session → take_persistent_state() → (LongTermMemory, Vec<HistoryEntry>)
                                              ↓
新 session ← restore_persistent_state(ltm, log) ← memory_factory(prompt)
```

**设计要点**：
- `take_persistent_state()` / `restore_persistent_state()` 是对称的 API 对
- `ChatAgent::create_session_with_migration()` 是唯一执行此流程的方法，`reset_with_updated_prompt()` 和 `new_session()` 都委托给它
- 迁移通过 `Memory` trait 的 `take_persistent_state()` / `restore_persistent_state()` 方法实现，不再依赖 downcast

[置信度：高]

### 磁盘持久化

> 📍 **代码位置**：`src/memory/persistence.rs` + `src/memory/sliding_window/mod.rs`

记忆状态通过 `MemoryPersistence` trait 持久化到 workspace：

| 数据 | 格式 | 路径 | 使用策略 |
|------|------|------|----------|
| LongTermMemory | JSON | `memory/long_term.json` | sliding_window |
| HistoryLog | JSONL | `memory/history.jsonl` | sliding_window |
| DynamicCheatsheet | JSON | `memory/cheatsheet.json` | sliding_window, dynamic_cheatsheet |
| AgenticMemoryStore | JSON | `memory/agentic/notes.json` | agentic |

**原子写入**：所有写入操作使用 `atomic_write()` 工具函数（write-to-temp-then-rename 模式），防止进程崩溃导致数据损坏。

**加载时机**：`SlidingWindowFactory::with_workspace()` / `with_workspace_and_cheatsheet()` 在创建 Memory 实例时自动加载。

**保存时机**：`Memory::persist()` 在 `agent.shutdown()` 时调用，无需 downcast 到具体类型。

[置信度：高]

### 历史搜索

```rust
pub fn search_history(&self, query: &str, limit: Option<usize>) -> Vec<&HistoryEntry>
```

- 大小写不敏感的关键词匹配（summary + keywords）
- `limit: Option<usize>` — `None` 返回全部匹配，`Some(n)` 返回至多 n 条

### 工厂构造

`ChatAgent::create_memory_factory()` 根据 `AgentConfig.memory_strategy` 选择对应的 factory：
- `SlidingWindow` → `SlidingWindowFactory::with_workspace_and_cheatsheet()`
- `DynamicCheatsheet` → `CheatsheetFactory::with_workspace()`
- `Agentic` → `AgenticFactory::with_workspace()` (需要 `EmbeddingConfig::create_provider()`)

`sliding_window_factory()` 辅助方法被 `SlidingWindow` 分支和 `Agentic` fallback 分支共用，消除重复。

### 共享常量

`DEFAULT_MAX_MESSAGES = 100`（定义在 `memory/mod.rs`，`pub(crate)`）：`CheatsheetMemory` 和 `AgenticMemory` 共享的消息窗口大小，防止长对话 token 超限。`SlidingWindowMemory` 有自己的 `SlidingWindowConfig.max_messages`。

### 测试覆盖

178 个单元测试覆盖：无限模式、窗口内/超窗口/边界条件、pending 消息、清除、长期记忆注入、历史搜索（大小写、关键词、限制）、整合触发/应用、持久化迁移、Dynamic Cheatsheet（条目创建/强化/更新、Markdown 渲染/截断、反思解析/应用、淘汰策略/阈值淘汰）等场景。

## 4. 支撑类型

### LongTermMemory（热数据）

> 📍 **代码位置**：`src/memory/long_term.rs`

结构化的关键事实，分为四个类别：
- `user_preferences` — 用户偏好（如 "prefers Rust"）
- `project_context` — 项目上下文（如 "working on Daedalus CLI agent"）
- `important_decisions` — 重要决策
- `important_notes` — 其他重要笔记

`to_markdown()` 将非空类别渲染为 Markdown 格式，用于注入 system prompt。

### HistoryEntry（冷数据）

> 📍 **代码位置**：`src/memory/history.rs`

追加式事件摘要，包含：
- `timestamp` — 创建时间
- `summary` — 2-5 句摘要
- `keywords` — 用于搜索的关键词列表

### SlidingWindowConfig

> 📍 **代码位置**：`src/memory/config.rs`

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `max_messages` | `None`（无限） | 发送给 LLM 的最大消息数 |
| `consolidation_threshold` | 100 | 触发整合的未整合消息数 |
| `retention_window` | 50 | 整合时保留的最近消息数 |

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-16 | 重写模块概述为三策略互斥架构；新增策略选择配置和流程图；更新 A-MEM 状态为已集成；新增 AgenticMemory 独立实现章节（预缓存模式、消息窗口）；新增 CheatsheetMemory 独立实现和 factory；更新 DC 集成方式（双模式 + 共享反思机制）；更新工厂构造（create_memory_factory 策略选择）；新增共享常量章节；更新持久化表格（策略归属列）；更新模块结构（新增 memory.rs + factory.rs） | 三策略互斥架构 + 代码质量审查 |
| 2026-04-15 | 新增 Dynamic Cheatsheet 章节（生命周期、模块结构、配置、淘汰策略、反思协议、集成方式）；更新 Memory trait 签名（新增 reflect_on_turn）；更新 SlidingWindowMemory 结构（PersistentComponents 聚合）；更新持久化表格和数据流图；更新测试覆盖数 | Dynamic Cheatsheet 实现 + 代码质量审查优化 |
| 2026-04-14 | 更新 Memory trait 签名（新增 persist/take/restore 方法）；新增磁盘持久化章节（原子写入、加载/保存时机）；更新持久化迁移描述 | Workspace 系统实现 + 架构审查优化 |
| 2026-04-13 | 重写：反映双层记忆架构重构（热/冷数据层、整合机制、持久化迁移、as_any downcast、代码质量改进） | 记忆系统重构 + 代码质量审查 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
