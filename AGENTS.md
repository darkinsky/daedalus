# AGENTS.md — Agent 导航入口

> 这是 AI Agent 进入本仓库的唯一入口。

## 仓库用途

Daedalus 是一个用 Rust 编写的终端 AI 助手，提供类似 Claude Code 风格的交互式 REPL 界面，支持多轮对话、MCP 工具调用、会话管理和结构化日志。

## 导航地图

| 我要做什么 | 去哪里 |
|-----------|--------|
| 了解整体架构 | → [ARCHITECTURE.md](./ARCHITECTURE.md) |
| 了解 Agent 模块 | → [docs/services/agent/overview.md](./docs/services/agent/overview.md) |
| 了解 CLI 模块 | → [docs/services/cli/overview.md](./docs/services/cli/overview.md) |
| 了解 LLM 模块 | → [docs/services/llm/overview.md](./docs/services/llm/overview.md) |
| 了解 MCP 模块 | → [docs/services/mcp/overview.md](./docs/services/mcp/overview.md) |
| 了解 Memory 模块 | → [docs/services/memory/overview.md](./docs/services/memory/overview.md) |
| 了解 Prompt 模块 | → [docs/services/prompt/overview.md](./docs/services/prompt/overview.md) |
| 了解核心配置 | → [docs/services/core/overview.md](./docs/services/core/overview.md) |
| 查看文档索引 | → [docs/INDEX.md](./docs/INDEX.md) |
| 查看设计决策 | → [docs/design/](./docs/design/) |
| 查看技术约束 | → [docs/constraints/](./docs/constraints/) |
| 查看编码规则 | → [docs/rules/](./docs/rules/) |

## 文档分类指南

新知识进入 →
  ├─ 只和某个模块有关？ → `docs/services/<module>/`
  ├─ 是跨模块的设计方案？ → `docs/design/`
  ├─ 是技术限制/边界？ → `docs/constraints/`
  ├─ 是编码惯例/隐含规则？ → `docs/rules/`
  └─ 是领域知识/技术债？ → `docs/knowledge/`

## 文件命名规范

- 使用英文 kebab-case：`daedalus-mcp-protocol.md`
- 决策记录使用日期前缀：`2026-04-08-switch-to-venus.md`
- 服务入口文件统一命名：`overview.md`
