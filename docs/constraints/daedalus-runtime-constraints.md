# 运行时约束

> 最后更新：2026-04-17
> 来源：存量代码分析 + 代码审查改进 + 并行化迭代 + 记忆系统重构 + Skill 功能实现 + Workspace 系统实现 + 架构审查优化 + Bash 工具 + **六策略互斥记忆架构** + **MemPalace Memory**
> 置信度：高

## 硬编码常量

| 常量 | 值 | 位置 | 说明 |
|------|-----|------|------|
| `MAX_TOOL_ROUNDS` | 10 | `src/agent/chat.rs:25` | 每条用户消息的最大工具调用轮次，防止 LLM 无限循环 |
| `MCP_REQUEST_TIMEOUT` | 30s | `src/mcp/client.rs:13` | MCP 普通请求超时 |
| `MCP_INIT_TIMEOUT` | 60s | `src/mcp/client.rs:16` | MCP 初始化握手超时（服务器可能需要启动时间） |
| `DEFAULT_AGENT_NAME` | "Daedalus" | `src/prompt/sections/role.rs:4` | 默认 Agent 名称 |
| `DEFAULT_SYSTEM_PROMPT` | "You are Daedalus..." | `src/config.rs:11-13` | 默认系统提示词（自定义检测的基准值） |
| `IGNORED_DIRS` | `["node_modules", "target", "__pycache__", ".git"]` | `src/tools/fs.rs` | 文件搜索时跳过的噪声目录 |
| `consolidation_threshold` (default) | 100 | `src/memory/sliding_window/config.rs` | 触发记忆整合的未整合消息数阈值 |
| `retention_window` (default) | 50 | `src/memory/sliding_window/config.rs` | 整合时保留的最近消息数（不被整合） |
| `DEFAULT_SIMILARITY_THRESHOLD` | 0.5 | `src/memory/agentic/store.rs` | A-MEM 链接候选的最低余弦相似度 |
| `DEFAULT_MAX_LINK_CANDIDATES` | 5 | `src/memory/agentic/store.rs` | A-MEM 每次链接生成检索的最大候选数 |
| `DEFAULT_RETRIEVAL_LIMIT` | 5 | `src/memory/agentic/store.rs` | A-MEM 上下文检索返回的最大 note 数 |
| `SKILL_FILENAME` | "SKILL.md" | `src/skill/loader.rs:8` | Skill 子目录中的入口文件名 |
| `SKILL_TOOL_NAME` | "use_skill" | `src/skill/registry.rs` | LLM 调用 skill 时使用的工具名 |
| `SUBAGENT_TOOL_NAME` | "spawn_subagent" | `src/subagent/tool.rs` | LLM 调用单个 subagent 时使用的工具名 |
| `TEAM_TOOL_NAME` | "spawn_team" | `src/subagent/tool.rs` | LLM 调用并行多 agent 时使用的工具名 |
| `DEFAULT_MAX_TOOL_ROUNDS` (subagent) | 10 | `src/subagent/runner.rs` | Subagent 默认最大工具调用轮数（可通过 `maxTurns` 覆盖） |
| `EXCLUDED_TOOLS` | `["spawn_subagent", "spawn_team", "use_skill"]` | `src/subagent/runner.rs` | Subagent 工具集中永远排除的工具（防递归） |
| `DEFAULT_TIMEOUT_SECS` (bash) | 30 | `src/tools/bash.rs:16` | Bash 工具默认命令超时时间（秒） |
| `MAX_OUTPUT_BYTES` (bash) | 256 KB | `src/tools/bash.rs:19` | Bash 工具最大输出截断大小，防止无界内存使用 |
| `CHARS_PER_TOKEN` | 4 | `src/memory/mod.rs` | 共享的 token 预算字符/token 比例（Playbook + DynamicCheatsheet 共用） |
| `max_entries` (DC default) | 50 | `src/memory/dynamic_cheatsheet/config.rs` | Dynamic Cheatsheet 最大条目数 |
| `max_token_budget` (DC default) | 2000 | `src/memory/dynamic_cheatsheet/config.rs` | DC 渲染为 Markdown 时的最大 token 预算 |
| `min_reinforcement_for_retention` (DC default) | 1 | `src/memory/dynamic_cheatsheet/config.rs` | DC 淘汰时的最低强化次数阈值 |
| `DEFAULT_MAX_MESSAGES` | 100 | `src/memory/mod.rs` | CheatsheetMemory、AgenticMemory、WikiMemory、AceMemory 和 MemPalaceMemory 的消息窗口大小，防止长对话 token 超限 |
| `TOOL_OUTPUT_MAX_LINES` | 10 | `src/cli/render.rs` | 工具输出显示的最大行数，超过则截断 |
| `TOOL_OUTPUT_HEAD_LINES` | 5 | `src/cli/render.rs` | 截断时保留的头部行数 |
| `TOOL_OUTPUT_TAIL_LINES` | 3 | `src/cli/render.rs` | 截断时保留的尾部行数 |
| `DEFAULT_LINT_INTERVAL` | 10 | `src/memory/wiki/store.rs` | Wiki Lint 检查间隔（每 N 轮对话触发一次） |
| `DEFAULT_MAX_RETRIEVAL_PAGES` | 5 | `src/memory/wiki/store.rs` | Wiki 检索返回的最大页面数 |
| `KEYWORD_MATCH_THRESHOLD` | 0.1 | `src/memory/wiki/retriever.rs` | Wiki 关键词匹配的最低分数阈值 |
| `LINK_EXPANSION_SCORE` | 0.3 | `src/memory/wiki/retriever.rs` | Wiki wikilink 扩展页面的基础分数 |
| `MAX_SEED_PAGES` | 3 | `src/memory/wiki/retriever.rs` | Wiki wikilink 扩展的种子页面数 |
| `max_sections` (ACE default) | 10 | `src/memory/ace/config.rs` | ACE Playbook 最大 section 数 |
| `max_bullets_per_section` (ACE default) | 15 | `src/memory/ace/config.rs` | ACE 每个 section 最大 bullet 数 |
| `max_token_budget` (ACE default) | 4000 | `src/memory/ace/config.rs` | ACE Playbook 渲染为 Markdown 时的最大 token 预算 |
| `min_reinforcement_for_retention` (ACE default) | 2 | `src/memory/ace/config.rs` | ACE 淘汰时的最低强化次数阈值 |
| `CHARS_PER_TOKEN` | 4 | `src/memory/mod.rs` | 共享的 token 预算字符/token 比例（ACE + DC 共用） |
| `retrieval_limit` (MemPalace default) | 5 | `src/memory/mempalace/config.rs` | MemPalace 每次检索返回的最大 Hall 条目数 |
| `similarity_threshold` (MemPalace default) | 0.3 | `src/memory/mempalace/config.rs` | MemPalace Embedding 检索的最低相似度阈值 |
| `closet_threshold` (MemPalace default) | 20 | `src/memory/mempalace/config.rs` | MemPalace 触发 Closet 摘要生成的 Drawer 数量阈值 |
| `chroma_url` (MemPalace default) | `http://localhost:8000` | `src/memory/mempalace/config.rs` | MemPalace ChromaDB 服务器 URL |
| `bm25_weight` (MemPalace default) | 0.4 | `src/memory/mempalace/config.rs` | MemPalace BM25 在混合排序中的权重 |
| `vector_weight` (MemPalace default) | 0.6 | `src/memory/mempalace/config.rs` | MemPalace 向量相似度在混合排序中的权重 |
| `dedup_threshold` (MemPalace default) | 0.15 | `src/memory/mempalace/config.rs` | MemPalace Jaccard 去重的距离阈值 |

## 工具调用摘要截断约束

> 📍 **代码位置**：`src/agent/chat.rs`

工具调用历史存入会话记忆时，参数和结果会被截断以防止大工具输出膨胀 token 消耗：

| 内容 | 截断长度 | 原因 |
|------|---------|------|
| 工具参数 | 200 字符 | 参数通常较短，200 字符足够保留关键信息 |
| 工具结果 | 500 字符 | 结果可能很长（如文件内容），500 字符保留摘要 |

## 记忆整合约束

> 📍 **代码位置**：`src/memory/sliding_window.rs` + `src/memory/config.rs`

`SlidingWindowMemory` 的整合机制受以下参数控制：

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `consolidation_threshold` | 100 | 未整合消息数达到此值时触发整合 |
| `retention_window` | 50 | 整合时保留最近 50 条消息不被整合 |
| `max_messages` | `None`（无限） | 发送给 LLM 的消息窗口大小 |

**整合范围**：`messages[consolidation_cursor .. messages.len() - retention_window]`。整合后 `consolidation_cursor` 游标推进到 `messages.len() - retention_window`。

**注意**：当前整合接口已就绪（`should_consolidate()`、`messages_to_consolidate()`、`apply_consolidation()`），但尚未接入实际的 LLM 整合调用。Agent 层的整合触发逻辑已预留（`chat()` 方法中检查 `should_consolidate()`）。

## 环境变量依赖

- **必须**：`OPENAI_API_KEY`（缺失时启动失败）
- **可选但影响行为**：`DAEDALUS_MODEL`（默认 gpt-4o）、`OPENAI_BASE_URL`、`DAEDALUS_ADAPTER_KIND`
- **Venus 触发条件**：设置 `DAEDALUS_THINKING_ENABLED` 或 `DAEDALUS_THINKING_TOKENS` 后自动切换到 VenusProvider

## 日志互斥约束

> 📍 **代码位置**：`src/logging.rs:265-311`

当配置了 `DAEDALUS_LOG_DIR` 时，日志**仅输出到文件**，不再输出到 stderr。这是一个刻意的设计选择，但意味着无法同时查看 stderr 和文件日志。

## MCP 配置搜索约束

> 📍 **代码位置**：`src/mcp/config.rs`

配置搜索是**先到先得**的，通过 `try_common_paths()` 和 `try_legacy_home_path()` 两个私有方法实现搜索链复用。`load_with_workspace()` 在公共搜索步骤和 legacy 回退之间插入 workspace 特有的搜索步骤。

## 工具调用并行执行

> 📍 **代码位置**：`src/agent/chat.rs:320-340`

同一轮中的多个工具调用是**并行执行**的（`futures::future::join_all`）。总耗时 = max(各工具耗时)，而非 sum(各工具耗时)。

**技术选型决策**：选择 `futures::future::join_all` 而非 `tokio::task::JoinSet`，因为 `ToolRouter::execute` 需要 `&self` 引用，而 `ToolRouter` 不是 `'static`，无法直接 spawn。`join_all` 不需要 `'static` 约束，更适合此场景。

**事件发射顺序**：所有 `ToolCallStart` 事件先发出，然后并行执行，最后所有 `ToolCallComplete` 事件一起发出。

## LogGuard 生命周期约束

> 📍 **代码位置**：`src/logging.rs:242-244`

`LogGuard` 必须在整个应用生命周期内持有。提前 drop 会导致文件日志缓冲区可能未完全 flush。

## Workspace 解析约束

> 📍 **代码位置**：`src/workspace.rs`

`Workspace::resolve()` 在 `logging::init()` 之前执行（因为日志目录依赖 workspace），因此 resolve 内部**不能使用 `tracing` 宏**。workspace 信息在 `main()` 中 logging 初始化后通过 `tracing::info!` 重新记录。

## 记忆持久化原子写入约束

> 📍 **代码位置**：`src/memory/persistence.rs`

**原子写入**：所有记忆持久化操作使用 `atomic_write()` 工具函数，实现 write-to-temp-then-rename 模式：
1. 写入数据到 `<path>.tmp`
2. 原子重命名 `<path>.tmp` → `<path>`

这确保进程崩溃时目标文件不会处于部分写入状态。影响的文件：`long_term.json`、`history.jsonl`、`notes.json`（A-MEM）、`cheatsheet.json`（DC）、`playbook.json`（ACE）、Wiki 的每个 `.md` 文件和 `_meta.json`、MemPalace 的 `palace.json`、`drawers.jsonl`、`closets.json`、`identity.txt`。
## 优雅关闭约束

> 📍 **代码位置**：`src/agent/chat.rs` + `src/agent/tool_router.rs`

`agent.shutdown()` 是异步方法（`async fn`），依次执行：
1. 通过 `Memory::persist()` 持久化记忆状态到 workspace
2. 通过 `ToolRouter::shutdown()` → `McpManager::shutdown()` 关闭所有 MCP 子进程

如果持久化失败，会记录错误但仍然继续关闭 MCP。

## Skill 加载约束

> 📍 **代码位置**：`src/skill/loader.rs` + `src/main.rs`

Skill 从当前工作目录的 `skills/` 子目录加载，遵循子目录 + `SKILL.md` 约定：

| 约束 | 说明 |
|------|------|
| 加载路径 | `std::env::current_dir()/skills/` — 固定为当前工作目录 |
| 目录结构 | 每个 skill 是一个子目录，必须包含 `SKILL.md` |
| 文件格式 | 支持 YAML front-matter（`description:` 字段）或简单 heading |
| 加载顺序 | 子目录按字母序排序后加载（确定性） |
| 名称冲突 | 后加载的 skill 覆盖先加载的（warn 日志） |
| 降级策略 | 目录不存在/不是目录/文件加载失败均跳过，不阻断启动 |

## Subagent 加载约束

> 📍 **代码位置**：`src/subagent/loader.rs` + `src/subagent/builtins.rs` + `src/main.rs`

Subagent 从三个来源加载，按优先级从低到高：

| 来源 | 优先级 | 说明 |
|------|--------|------|
| `SubagentSource::Builtin` | 最低 | 硬编码在二进制中（3 个内置 agent） |
| `SubagentSource::Global` | 中 | `~/.daedalus/agents/*.md` |
| `SubagentSource::Project` | 最高 | `.daedalus/agents/*.md` |

| 约束 | 说明 |
|------|------|
| 加载顺序 | `register_builtins()` → `load_from_dir(project)` → `load_from_dir(global)` |
| 名称冲突 | 后加载的覆盖先加载的（warn 日志） |
| 文件格式 | YAML frontmatter + Markdown body（同 SkillLoader 模式） |
| 加载顺序 | `.md` 文件按字母序排序后加载（确定性） |
| 降级策略 | 目录不存在/文件加载失败均跳过，不阻断启动 |
| `spawn_team` 注册条件 | 仅在 ≥2 个 subagent 可用时注册 |

## Subagent 执行约束

> 📍 **代码位置**：`src/subagent/runner.rs` + `src/subagent/isolation.rs`

| 约束 | 说明 |
|------|------|
| 上下文隔离 | 每次调用创建全新的 LLM provider + 工具集，不共享主 agent 的 session/memory |
| 防递归 | `EXCLUDED_TOOLS` 硬编码排除 `spawn_subagent`、`spawn_team`、`use_skill` |
| 模型简写 | 支持 `haiku`、`sonnet`、`opus` 简写，映射到完整模型 ID |
| Worktree 隔离 | `isolation: worktree` 时通过 `git worktree add` 创建临时工作树，`WorktreeGuard` RAII 自动清理 |
| 生命周期钩子 | `onStart`/`onComplete` 通过 `tokio::process::Command` 异步执行 shell 命令，任务/结果通过 stdin 传递 |
| 并行团队 | `run_team()` 通过 `futures::future::join_all` 并行执行多个 subagent |

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-17 | 新增 MemPalace Memory 运行时常量（retrieval_limit、similarity_threshold、closet_threshold、chroma_url、bm25_weight、vector_weight、dedup_threshold）；更新 DEFAULT_MAX_MESSAGES 说明（新增 MemPalaceMemory）；更新原子写入影响文件列表 | MemPalace Memory 实现 |
| 2026-04-16 | 新增 ACE Memory 运行时常量（max_sections、max_bullets_per_section、max_token_budget、min_reinforcement_for_retention）；更新 CHARS_PER_TOKEN 位置为共享常量；更新 DEFAULT_MAX_MESSAGES 说明（新增 AceMemory）；更新原子写入影响文件列表（新增 playbook.json） | ACE Memory 实现 |
| 2026-04-16 | 新增 Wiki Memory 运行时常量（DEFAULT_LINT_INTERVAL、DEFAULT_MAX_RETRIEVAL_PAGES、KEYWORD_MATCH_THRESHOLD、LINK_EXPANSION_SCORE、MAX_SEED_PAGES）；更新原子写入影响文件列表（新增 Wiki .md + _meta.json）；更新 DEFAULT_MAX_MESSAGES 说明（新增 WikiMemory） | Wiki Memory 实现 |
| 2026-04-16 | 新增 DEFAULT_MAX_MESSAGES 常量（CheatsheetMemory/AgenticMemory 消息窗口） | 三策略互斥架构 |
| 2026-04-15 | 新增 Dynamic Cheatsheet 运行时常量（CHARS_PER_TOKEN、max_entries、max_token_budget、min_reinforcement_for_retention）；更新原子写入影响文件列表（新增 cheatsheet.json） | Dynamic Cheatsheet 实现 |
| 2026-04-15 | 新增工具输出截断常量（TOOL_OUTPUT_MAX_LINES/HEAD_LINES/TAIL_LINES） | 代码质量审查优化 |
| 2026-04-15 | 新增 Bash 工具运行时常量（DEFAULT_TIMEOUT_SECS、MAX_OUTPUT_BYTES） | Bash 工具实现 |
| 2026-04-15 | 新增 Subagent 运行时常量和约束（加载优先级、执行隔离、防递归、Worktree、生命周期钩子） | Subagent 功能实现 |
| 2026-04-14 | 更新原子写入影响文件列表（新增 notes.json）；更新 MCP 配置搜索约束（try_common_paths 重构） | 架构审查优化 |
| 2026-04-14 | 新增 Workspace 解析约束（pre-logging）、记忆持久化原子写入约束、优雅关闭约束 | Workspace 系统实现 + 架构审查优化 |
| 2026-04-13 | 新增 SKILL_FILENAME、SKILL_TOOL_NAME 常量；新增 Skill 加载约束章节 | Skill 功能实现 |
| 2026-04-13 | 新增 A-MEM 运行时常量（相似度阈值、候选数、检索限制）；更新 consolidation 字段命名和代码位置 | A-MEM 实现 + 代码审查 |
| 2026-04-13 | 新增记忆整合约束（consolidation_threshold、retention_window、整合范围） | 记忆系统重构 |
| 2026-04-09 | 工具调用从串行改为并行执行；新增 futures 0.3 依赖；补充技术选型决策 | 并行化迭代 |
| 2026-04-08 | 新增 IGNORED_DIRS 常量、工具摘要截断约束 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
