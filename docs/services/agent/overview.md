# Agent — Agent 模式抽象、ChatAgent 与 ToolRouter

> 最后更新：2026-04-15
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代 + 记忆系统重构 + Skill 功能实现 + 模块化重构 + Bash 工具

## 1. 模块概述

Agent 模块定义了统一的 Agent 模式接口（`AgentMode` trait）和当前唯一的实现 `ChatAgent`。`ChatAgent` 负责多轮对话编排，包括消息管理、LLM 调用和工具调用循环。工具调用通过 `ToolRouter` 统一路由，支持内置工具和 MCP 外部工具。会话管理（`Session`）也归属于本模块。

## 2. AgentMode Trait

> 📍 **代码位置**：`src/agent/mod.rs`

### Session 管理

> 📍 **代码位置**：`src/agent/session.rs`（从 `src/session.rs` 迁移）

Session 是 agent 的内部概念，只被 agent 模块使用，因此在模块化重构中从 `src/` 外层移入 `agent/` 目录。通过 `agent/mod.rs` 中的 `pub(crate) use session::Session` 对 crate 内部可见。

```
Session {
    id: String,           // UUID v4
    title: String,        // "Session YYYY-MM-DD HH:MM:SS"
    request_count: u64,   // 自增请求计数器
    created_at: String,   // 预留给未来持久化
    memory: Box<dyn Memory>,  // 策略模式：trait object 持有记忆策略
}
```

**设计模式**：策略模式。Session 通过 `Box<dyn Memory>` 持有记忆策略，对外提供 `memory()` / `memory_mut()` 访问器而非代理方法，最大化灵活性。[置信度：高]

### AgentMode Trait 定义

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
| `memory_factory` | `MemoryFactory` | 记忆工厂函数（运行时创建双层记忆实例） |
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

### 会话迁移

`create_session_with_migration()` 是唯一的会话重建方法，负责 `take_persistent_state → memory_factory → restore_persistent_state` 的完整生命周期。`reset_with_updated_prompt()` 和 `new_session()` 都委托给它，避免了迁移逻辑重复。[置信度：高]

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

> 📍 **代码位置**：`src/tools/mod.rs` + `src/tools/fs.rs` + `src/tools/bash.rs`

内置工具通过 `BuiltinTool` trait 定义，与 MCP 工具使用相同的 OpenAI function-calling JSON 格式，对 LLM 完全透明。

当前内置工具：

| 工具名 | 功能 | 关键参数 |
|--------|------|--------|
| `read_file` | 读取文件内容，支持行号和分页 | `path`, `offset?`, `limit?` |
| `write_file` | 写入文件（自动创建父目录） | `path`, `content` |
| `list_directory` | 列出目录内容，支持递归 | `path`, `recursive?`, `max_entries?` |
| `search_files` | 按文件名模式搜索 | `path`, `pattern`, `max_results?` |
| `get_file_info` | 获取文件/目录元数据 | `path` |
| `bash` | 执行 bash 命令，返回 stdout/stderr | `command`, `working_directory?`, `timeout_secs?` |

---

## 5. Skill 系统 — LLM 路由的技能加载

> 📍 **代码位置**：`src/skill/`

### 概述

Skill 系统允许用户通过 Markdown 文件定义领域专用指令，由 LLM 在运行时自主决定何时调用。与传统的 prompt 注入方式不同，Skill 不会静态注入到 system prompt 中（避免浪费 token），而是作为 `use_skill` 内置工具暴露给 LLM。

### 目录结构约定

```text
skills/                      # 当前工作目录下
├── code-review/
│   └── SKILL.md             # 入口文件（必须）
├── sql-expert/
│   ├── SKILL.md
│   └── examples.sql         # 可选资源文件
└── skill-creator/
    └── SKILL.md
```

- 每个 skill 是一个**子目录**，目录名即 skill 名称（kebab-case）
- 子目录中必须包含 `SKILL.md` 作为入口文件
- `SKILL.md` 支持 YAML front-matter（`description:` 字段）或简单 heading 两种格式
- 没有 `SKILL.md` 的子目录被静默跳过

### 模块组成

| 文件 | 职责 |
|------|------|
| `skill/mod.rs` | 模块入口，定义 `SkillInfo`、`SkillDefinition` 数据结构 |
| `skill/loader.rs` | 从文件系统扫描子目录并加载 `SKILL.md` |
| `skill/registry.rs` | 管理所有 skill，生成 `use_skill` 工具定义，提供 `SkillTool` 适配器 |

### LLM 路由机制

```
启动时: skills/ → SkillLoader → SkillRegistry → SkillTool(BuiltinTool) → BuiltinToolRegistry

运行时: 用户输入 → LLM 看到 use_skill 工具（含所有 skill 名称和描述）
         → LLM 自主决定调用 use_skill(name="code-review")
         → ToolRouter → BuiltinToolRegistry → SkillTool.execute()
         → SkillRegistry.execute_skill() → 返回 SKILL.md 的 instructions
         → LLM 根据 instructions 完成任务
```

**关键设计**：`SkillTool` 实现了 `BuiltinTool` trait，作为 `SkillRegistry` 的适配器注册到 `BuiltinToolRegistry` 中。这样 `ToolRouter.execute()` 无需任何特殊分支——skill 调用和普通内置工具调用走完全相同的路径。[置信度：高]

### SkillTool 适配器

> 📍 **代码位置**：`src/skill/registry.rs`

`SkillTool` 通过 `Arc<SkillRegistry>` 共享 registry 引用，实现 `BuiltinTool` trait 的 4 个方法。特别地，它覆写了 `to_openai_json()` 方法，委托给 `SkillRegistry::build_tool_definition()` 生成包含所有 skill 名称和描述的富描述文本，使 LLM 能做出准确的路由决策。

### AgentMode Trait 扩展

`AgentMode` trait 新增了两个默认方法：

```rust
fn skill_infos(&self) -> Vec<SkillInfo> { vec![] }
fn skill_count(&self) -> usize { 0 }
```

`ChatAgent` 覆写这两个方法，委托给 `ToolRouter.skill_registry()`。CLI 层通过这些方法渲染 `/skills` 命令输出和启动 banner 中的 skill 数量。

### 优雅降级

- `skills/` 目录不存在 → 静默跳过（debug 日志）
- `skills/` 不是目录 → warn 日志并跳过
- 子目录无 `SKILL.md` → 静默跳过
- `SKILL.md` 加载失败 → warn 日志并跳过该 skill，继续加载其他 skill
- 无 skill 加载成功 → 不注册 `use_skill` 工具，不影响正常使用

---

## 6. Subagent 系统 — 隔离上下文的任务委托

> 📍 **代码位置**：`src/subagent/`

### 概述

Subagent 系统允许主 Agent 将任务委托给运行在**完全隔离上下文**中的专用子代理。每个 subagent 拥有独立的系统提示、独立的 LLM provider（可指定不同模型）、独立的工具集（白名单/黑名单控制），以及独立的对话历史。参考了 Claude Code 的 Sub-agents 设计。

### 核心价值

| 价值 | 说明 |
|------|------|
| **上下文隔离** | 子任务在独立上下文中运行，不污染主对话 |
| **行为专业化** | 每个 subagent 有专用 system prompt，角色清晰 |
| **工具约束** | 通过白名单/黑名单限制工具访问（如只读分析 agent） |
| **成本控制** | 简单任务用轻量模型（haiku），复杂任务用强模型（opus） |
| **可复用** | 一次定义，跨项目/跨会话复用 |

### 模块组成

| 文件 | 职责 |
|------|------|
| `subagent/mod.rs` | 模块入口，re-export 核心类型 |
| `subagent/types.rs` | 核心数据结构（`SubagentDefinition`、`SubagentInfo`、`SubagentResult`、`TeamTask`、枚举类型） |
| `subagent/builtins.rs` | 3 个内置 subagent 定义（explore、code-reviewer、plan），硬编码在二进制中 |
| `subagent/loader.rs` | 从 `.md` 文件加载 subagent 定义（YAML frontmatter + Markdown body） |
| `subagent/registry.rs` | 注册表管理，处理优先级覆盖（Builtin < Global < Project） |
| `subagent/runner.rs` | 执行引擎——创建独立 LLM provider + 过滤工具集 + 工具调用循环 |
| `subagent/tool.rs` | `SubagentTool`（`spawn_subagent`）和 `TeamTool`（`spawn_team`）的 `BuiltinTool` 适配器 |
| `subagent/isolation.rs` | Git worktree 隔离和生命周期钩子（`onStart`/`onComplete`） |

### 配置文件格式

Subagent 定义为 `.md` 文件，放在 `agents/` 目录下：

```yaml
---
name: code-reviewer
description: Reviews code for quality and best practices.
tools: read_file, list_directory, search_files, get_file_info
model: sonnet
permissionMode: plan
maxTurns: 10
isolation: none
onStart: echo "Starting..."
onComplete: echo "Done."
---

You are a senior code reviewer.
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` | 否（默认取文件名） | 唯一标识名（kebab-case） |
| `description` | 是 | LLM 用来判断何时调用的描述 |
| `model` | 否（inherit） | 模型选择：`haiku`/`sonnet`/`opus`/完整 ID |
| `tools` | 否 | 工具白名单（逗号分隔） |
| `disallowedTools` | 否 | 工具黑名单（逗号分隔） |
| `permissionMode` | 否（default） | 权限模式 |
| `maxTurns` | 否 | 最大工具调用轮数 |
| `isolation` | 否（none） | 隔离模式：`none`/`worktree` |
| `onStart` | 否 | 启动前执行的 shell 命令 |
| `onComplete` | 否 | 完成后执行的 shell 命令 |

### 加载优先级

```
Builtin (最低) → Global (~/.daedalus/agents/) → Project (.daedalus/agents/) (最高)
```

同名 agent 按优先级覆盖。用户只需在 `.daedalus/agents/` 中放一个同名 `.md` 文件即可覆盖内置 agent。

### 3 个内置 Subagent

| 名称 | 用途 | 工具白名单 | maxTurns |
|------|------|-----------|----------|
| `explore` | 只读代码探索与分析 | 只读工具 | 8 |
| `code-reviewer` | 代码质量审查 | 只读工具 | 10 |
| `plan` | 架构分析与实现规划 | 只读工具 | 12 |

### LLM 路由机制

```
启动时: builtins + agents/*.md → SubagentLoader → SubagentRegistry
  → SubagentTool(spawn_subagent) + TeamTool(spawn_team) → BuiltinToolRegistry

运行时: 用户输入 → LLM 看到 spawn_subagent 工具（含所有 agent 名称和描述）
  → LLM 自主决定调用 spawn_subagent(agent_name="explore", task="...")
  → ToolRouter → SubagentTool.execute()
    → SubagentRunner.run()
      → 创建独立 LLM provider（可能不同模型）
      → 构建过滤后的工具集（无 spawn_subagent/spawn_team/use_skill）
      → 执行工具调用循环（独立上下文）
    → 返回 SubagentResult
  → 格式化结果返回给主 LLM
```

### Agent Teams（并行多 Agent）

`spawn_team` 工具允许 LLM 在一次调用中启动多个 subagent 并行执行：

```json
{
  "name": "spawn_team",
  "arguments": {
    "tasks": [
      {"agent_name": "explore", "task": "Find all error handling patterns"},
      {"agent_name": "code-reviewer", "task": "Review src/agent/chat.rs"}
    ]
  }
}
```

所有任务通过 `futures::future::join_all` 并行执行，结果汇总后返回。`spawn_team` 仅在 ≥2 个 subagent 可用时注册。

### ToolEvent 透传

Subagent 执行过程中的工具调用事件通过 `SharedEventCallback`（`Arc<RwLock<Option<ToolEventCallback>>>`）透传到 CLI 层实时渲染。REPL 在每次 chat 调用前设置回调，调用后清除。

新增的 ToolEvent 变体：

| 事件 | 时机 | 携带信息 |
|------|------|----------|
| `SubagentStart` | subagent 开始执行 | agent 名称、任务预览 |
| `SubagentComplete` | subagent 执行完成 | agent 名称、成功/失败、工具轮数、结果预览 |

### 防递归保护

Subagent 的工具集中**永远不包含** `spawn_subagent`、`spawn_team`、`use_skill`，通过 `EXCLUDED_TOOLS` 常量硬编码排除，防止无限递归。

### Worktree 隔离

当 `isolation: worktree` 时，`SubagentRunner` 通过 `git worktree add` 创建临时工作树，subagent 在隔离的分支上操作。`WorktreeGuard` 实现 RAII 模式，在 subagent 完成后自动清理 worktree 和临时分支。

### 优雅降级

- `agents/` 目录不存在 → 静默跳过（debug 日志）
- `.md` 文件加载失败 → warn 日志并跳过该 agent
- 无 subagent 加载成功 → 不注册 `spawn_subagent` 工具
- 内置 subagent 始终可用（即使无 `.md` 文件）

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-15 | 内置工具表格新增 `bash` 工具（shell 命令执行） | Bash 工具实现 |
| 2026-04-15 | 新增 Subagent 系统章节（隔离执行、Agent Teams、ToolEvent 透传、Worktree 隔离、内置 agent） | Subagent 功能实现 |
| 2026-04-14 | Session 从 `src/session.rs` 迁移至 `src/agent/session.rs`；新增 Session 管理章节 | 模块化重构 |
| 2026-04-13 | 新增 Skill 系统章节（LLM 路由、SkillTool 适配器、AgentMode 扩展、优雅降级） | Skill 功能实现 |
| 2026-04-09 | 工具调用改为并行执行（futures::join_all）；新增 ToolEvent 回调机制；AgentMode::chat() 签名增加 on_tool_event 参数 | 工具事件/并行化迭代 |
| 2026-04-08 | 新增 ToolRouter、BuiltinTool 架构；更新字段命名和工具调用流程 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
