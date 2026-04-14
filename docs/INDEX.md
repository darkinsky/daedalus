# 文档索引

> 最后更新：2026-04-14
> 来源：存量代码分析 + 代码审查改进 + 工具事件/并行化迭代 + 记忆系统重构 + A-MEM 实现 + Skill 功能实现 + Workspace 系统实现 + YAML 配置迁移 + 模块化重构

## 服务/模块文档

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [core/overview.md](services/core/overview.md) | 核心入口、Workspace 统一路径管理、YAML 配置、日志（config/ 模块） | 2026-04-14 |
| [agent/overview.md](services/agent/overview.md) | Agent 模式抽象 + ChatAgent + Session + ToolRouter + 内置工具 + ToolEvent 回调 + 并行工具执行 + Skill 系统（LLM 路由） | 2026-04-14 |
| [cli/overview.md](services/cli/overview.md) | REPL 交互界面、命令、渲染、工具事件渲染 | 2026-04-09 |
| [llm/overview.md](services/llm/overview.md) | LLM Provider 抽象 + 双 Provider 实现（ToolInfo 已迁移至 tools） | 2026-04-09 |
| [mcp/overview.md](services/mcp/overview.md) | MCP 协议客户端 + 工具管理 + Workspace 配置搜索链 + try_common_paths 重构 | 2026-04-14 |
| [memory/overview.md](services/memory/overview.md) | 双层记忆架构 + A-MEM 知识图谱引擎 + Embedding trait + 整合机制 + 持久化迁移 + 原子写入 + Memory::persist() | 2026-04-14 |
| [prompt/overview.md](services/prompt/overview.md) | 系统提示词动态组装 | 2026-04-08 |

## 设计决策

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-trait-based-architecture.md](design/daedalus-trait-based-architecture.md) | Trait 抽象 + 依赖注入 + YAML 配置迁移 + 模块化重构 + ToolRouter/BuiltinTool + ToolInfo迁移 + ToolEvent回调 + 并行化 + Memory双层架构 + 持久化迁移 + 动态注入 + ToolRound + PersistentState封装 + Skill LLM路由 + SkillTool适配器 + Memory::persist() + 原子写入 + 优雅关闭 + MCP配置重构 + ToolInfo清理 | 2026-04-14 |

## 技术约束

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-runtime-constraints.md](constraints/daedalus-runtime-constraints.md) | 运行时硬编码约束（超时、轮次限制、截断、并行执行、记忆整合、A-MEM 参数、Skill 加载、Workspace 解析、原子写入、优雅关闭等） | 2026-04-14 |

## 隐含规则

| 文档 | 简述 | 最后更新 |
|------|------|---------|
| [daedalus-coding-conventions.md](rules/daedalus-coding-conventions.md) | 编码惯例、命名规范、模块组织规范（含就近原则、同领域合并、Rust 2024 模块模式）、迭代器与副作用规则、Prompt模板分离、deprecated使用规则 | 2026-04-14 |
