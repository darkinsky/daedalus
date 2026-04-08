# 设计决策：Trait 抽象 + 依赖注入架构

> 最后更新：2026-04-08
> 来源：存量代码分析 + 代码审查改进
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

**当前实现**：`SlidingWindowMemory`（支持无限和有限窗口两种模式）。

**MemoryFactory 模式**：`ChatAgent` 持有 `Box<dyn Fn(&str) -> Box<dyn Memory>>` 工厂函数，而非泛型参数，允许运行时动态创建不同实现。

## 设计权衡

| 方面 | 选择 | 原因 |
|------|------|------|
| Trait Object vs 泛型 | Trait Object | 需要运行时多态（Provider 选择取决于配置），泛型会导致类型膨胀 |
| MemoryFactory | `Box<dyn Fn>` | 比泛型更灵活，允许在运行时根据配置创建不同策略 |
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

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 新增 ToolRouter 抽取决策和 BuiltinTool trait 设计 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
