# 文档索引

> 最后更新：2026-04-17
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代 + 记忆系统重构 + A-MEM 实现 + Skill 功能实现 + Workspace 系统实现 + YAML 配置迁移 + 模块化重构 + Subagent 功能实现 + 代码质量审查优化 + Dynamic Cheatsheet 实现 + **六策略互斥记忆架构** + **MemPalace Memory** + **对标 Claude Code 工具集**

## 服务/模块文档

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [core/overview.md](services/core/overview.md) | 核心入口、Workspace 统一路径管理、YAML 配置（含 **memory.strategy** 和 **embedding** section）、日志（config/ 模块） | 2026-04-16 |
| [agent/overview.md](services/agent/overview.md) | Agent 模式抽象 + ChatAgent + Session + ToolRouter + 内置工具（含 Bash、**grep_search、edit_file、multi_edit**） + ToolEvent 回调（**迁移到 tools 模块**） + 并行工具执行 + Skill 系统 + **Subagent 系统** + **对标 Claude Code 工具演进** + **记忆工厂提取到 memory 模块** | 2026-04-17 |
| [cli/overview.md](services/cli/overview.md) | REPL 交互界面、命令、渲染、工具事件渲染（**format_tool_event_lines 共享渲染**）、**非交互模式（Print Mode）**、**共享文本工具函数** | 2026-04-17 |
| [llm/overview.md](services/llm/overview.md) | LLM Provider 抽象 + 双 Provider 实现（ToolInfo 已迁移至 tools） | 2026-04-09 |
| [mcp/overview.md](services/mcp/overview.md) | MCP 协议客户端 + 工具管理 + Workspace 配置搜索链 + try_common_paths 重构 | 2026-04-14 |
| [memory/overview.md](services/memory/overview.md) | **六策略互斥记忆系统**（SlidingWindow/CheatsheetMemory/AgenticMemory/WikiMemory/AceMemory/**MemPalaceMemory**） + 双层记忆架构 + Dynamic Cheatsheet 自适应记忆 + A-MEM 知识图谱引擎 + Wiki Memory 知识编译引擎（Karpathy 模式） + ACE Memory 策略积累引擎 + **MemPalace Memory 空间化记忆宫殿引擎（ChromaDB + BM25 + KG + Tunnels + AAAK）** + Embedding trait + 整合机制 + 持久化迁移 + 原子写入 + Memory::persist() + reflect_on_turn() + 策略选择配置 + EmbeddingConfig + **共享工具函数（MessageBuffer、strip_directive_prefix、truncate_to_token_budget）** | 2026-04-17 |
| [prompt/overview.md](services/prompt/overview.md) | 系统提示词动态组装 | 2026-04-08 |

## 设计决策

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-trait-based-architecture.md](design/daedalus-trait-based-architecture.md) | Trait 抽象 + 依赖注入 + YAML 配置迁移 + 模块化重构 + ToolRouter/BuiltinTool + ToolInfo迁移 + ToolEvent回调（**迁移到 tools**） + 并行化 + Memory双层架构 + 持久化迁移 + 动态注入 + ToolRound + PersistentState封装 + Skill LLM路由 + SkillTool适配器 + Memory::persist() + 原子写入 + 优雅关闭 + MCP配置重构 + ToolInfo清理 + Subagent 设计决策 + Dynamic Cheatsheet 设计决策 + **六策略互斥记忆架构决策** + Wiki Memory 设计决策 + ACE Memory 设计决策 + **MemPalace Memory 设计决策** + **对标 Claude Code 工具集设计决策**（grep_search/edit_file/multi_edit/ToolEvent迁移/create_memory_factory提取/format_tool_event_lines/re-export清理） | 2026-04-17 |

## 技术约束

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-runtime-constraints.md](constraints/daedalus-runtime-constraints.md) | 运行时硬编码约束（超时、轮次限制、截断、并行执行、记忆整合、A-MEM 参数、DC 参数、DEFAULT_MAX_MESSAGES、Skill 加载、Workspace 解析、原子写入、优雅关闭、Subagent 加载/执行约束、Bash 工具约束、**grep_search 工具约束**、Wiki Memory 常量、ACE Memory 常量、**MemPalace Memory 常量**） | 2026-04-17 |

## 效果调优

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [code-review-trace-analysis-2026-04-25.md](tuning/code-review-trace-analysis-2026-04-25.md) | 代码审查任务 Trace 分析：subagent maxTurns 耗尽导致主 Agent 重复工作、循环犹豫反模式、context overflow、MaxRoundsExceeded 优雅降级、分批审查策略、成本分析与优化路线图 | 2026-04-25 |
| [code-review-trace-analysis-2026-05-17.md](tuning/code-review-trace-analysis-2026-05-17.md) | 多子代理并行编排 Trace 分析：编排器综合验证缺失、子代理通读 vs 扫描策略、take_note 使用率、严重程度校准、跨子代理边界信息传递、编译器辅助验证、负载均衡优化 | 2026-05-17 |

## 隐含规则

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-coding-conventions.md](rules/daedalus-coding-conventions.md) | 编码惯例、命名规范、模块组织规范（含就近原则、同领域合并、Rust 2024 模块模式、**God Object 拆分模式、Feature-complete 死代码注解、嵌套 if let 改循环、重复渲染提取纯函数、cfg(test) re-export**）、迭代器与副作用规则、Prompt模板分离、deprecated使用规则 | 2026-04-17 |
