# Core — 核心入口、配置、会话、日志

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

Core 包含 Daedalus 的入口点和基础设施组件：启动编排（`main.rs`）、环境变量配置（`config.rs`）、会话管理（`session.rs`）和结构化日志（`logging.rs`）。

## 2. main.rs — 启动编排

> 📍 **代码位置**：`src/main.rs`

启动流程严格按序执行 5 个阶段：

1. **日志初始化**：`LogConfig::from_env()` → `logging::init()` → 持有 `_log_guard`
2. **配置加载**：`AgentConfig::from_env()` 从环境变量读取
3. **MCP 初始化**（可选）：`McpConfig::load()` → `McpManager::from_config()` 并行连接
4. **LLM Provider 创建**：`llm::create_provider()` 根据配置选择 GenAI 或 Venus
5. **REPL 启动**：`cli::run_interactive()` 进入主循环

**设计决策**：MCP 初始化是 `Option<McpManager>`，缺少配置文件时自动跳过，不阻断启动。[置信度：高]

## 3. config.rs — Agent 配置

> 📍 **代码位置**：`src/config.rs`

### 关键类型

- `const DEFAULT_SYSTEM_PROMPT` — 默认系统提示词的单一真实来源
- `struct AgentConfig` — 顶层配置聚合

### 环境变量映射

| 环境变量 | 必选 | 默认值 | 用途 |
|---|---|---|---|
| `OPENAI_API_KEY` | ✅ | — | API 密钥 |
| `DAEDALUS_MODEL` | | `gpt-4o` | 模型名 |
| `OPENAI_BASE_URL` | | — | 自定义 API 端点 |
| `DAEDALUS_ADAPTER_KIND` | | `openai` | 适配器类型 |
| `DAEDALUS_SYSTEM_PROMPT` | | — | 自定义提示词（覆盖 PromptBuilder） |
| `DAEDALUS_AGENT_NAME` | | `Daedalus` | Agent 名称 |
| `DAEDALUS_SOUL_FILE` | | — | SOUL.md 文件路径 |
| `DAEDALUS_THINKING_ENABLED` | | — | 启用思考模式 |
| `DAEDALUS_THINKING_TOKENS` | | — | 思考最大 token |
| `DAEDALUS_REASONING_EFFORT` | | — | 推理努力级别（low/medium/high） |

### 双轨提示词机制

`is_custom_prompt` 标志实现了两种提示词模式：
- **自定义模式**：`DAEDALUS_SYSTEM_PROMPT` 设置后直接使用，跳过 PromptBuilder
- **动态模式**：未设置时通过 PromptBuilder 动态组装（含 name + soul + tools）

### Soul 人格系统

从 `DAEDALUS_SOUL_FILE` 指定的文件读取"人格"内容，注入到系统提示的 `<soul>` 段落中。文件读取失败仅 warn 不 panic（优雅降级）。[置信度：高]

## 4. session.rs — 会话管理

> 📍 **代码位置**：`src/session.rs`

### Session 结构

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

## 5. logging.rs — 结构化日志

> 📍 **代码位置**：`src/logging.rs`

### 特性

- **双通道输出**：stderr（开发）和滚动文件（生产），但二者互斥
- **4 种格式**：Pretty（默认）、Compact、Json、Full
- **4 种轮转策略**：Minutely、Hourly、Daily（默认）、Never
- **时区检测**：优先本地时区，回退 UTC
- **非阻塞写入**：`tracing_appender::non_blocking` 避免 IO 阻塞
- **13 个环境变量**高度可配置

### LogGuard 生命周期约束

`LogGuard` 必须在 main 函数全生命周期内持有（`_log_guard`），drop 时会 flush 缓冲区。这是 `tracing-appender` 的 non-blocking writer 的要求。[置信度：高]

### 宏消除重复

`apply_display_opts!` 宏消除了 4 种格式变体之间重复的 `.with_file().with_line_number()...` 配置链。[置信度：高]

## 6. 构建与开发命令

> 📍 **构建配置位置**：`Cargo.toml`

| 用途 | 命令 | 说明 |
|------|------|------|
| 完整构建 | `cargo build --release` | 编译优化二进制 |
| 调试构建 | `cargo build` | 调试模式 |
| 运行 | `cargo run --release` | 启动 Daedalus |
| 单元测试 | `cargo test` | 运行所有测试 |
| 代码检查 | `cargo clippy` | 代码质量检查 |
| 调试运行 | `RUST_LOG=daedalus=debug cargo run` | 带调试日志 |
| 文件日志 | `DAEDALUS_LOG_DIR=./logs cargo run` | 启用文件日志 |

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
