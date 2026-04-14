# Core — 核心入口、Workspace、配置、日志

> 最后更新：2026-04-14
> 来源：存量代码分析 + Workspace 系统实现 + 架构审查优化 + YAML 配置迁移 + 模块化重构

## 1. 模块概述

Core 包含 Daedalus 的入口点和基础设施组件：启动编排（`main.rs`）、YAML 配置加载（`config/agent_config.rs`）、结构化日志（`config/logging.rs`）和 Workspace 路径管理（`workspace.rs`）。

> **注意**：`session.rs` 已在模块化重构中移入 `agent/` 模块，详见 [agent/overview.md](../agent/overview.md)。

## 2. workspace.rs — 统一路径管理

> 📍 **代码位置**：`src/workspace.rs`

### 设计动机

Workspace 解决了文件路径散落问题——配置、记忆、日志、技能等文件原先各自硬编码路径，现在统一通过 `Workspace` 获取。Workspace 是**纯路径管理器**，不持有业务逻辑。

### 三级优先级解析

```
Workspace::resolve()
  1. DAEDALUS_WORKSPACE 环境变量 → Custom workspace
  2. 从 cwd 向上查找 .daedalus/ 目录 → Project workspace
  3. ~/.daedalus/ 全局目录（自动创建）→ Global workspace
```

### 目录结构

```text
<workspace_root>/
├── config/
│   ├── mcp.json          # MCP 服务器配置
│   └── soul.md           # SOUL 人格文件
├── memory/
│   ├── long_term.json    # LongTermMemory 持久化
│   ├── history.jsonl     # HistoryLog 持久化（追加写入）
│   └── agentic/
│       └── notes.json    # A-MEM 知识图谱持久化
├── sessions/
│   └── last_session_id   # 上次会话 ID
├── skills/               # Skill 定义
│   └── <skill-name>/
│       └── SKILL.md
└── logs/                 # 滚动日志文件
    └── daedalus.<date>
```

### 路径访问器

所有路径通过语义化方法获取：`mcp_config_path()`、`soul_file_path()`、`long_term_memory_path()`、`history_log_path()`、`skills_dir()`、`logs_dir()` 等。配套 `has_*()` 方法检查文件是否存在。

### Pre-logging 约束

`Workspace::resolve()` 在 `logging::init()` 之前调用（因为日志目录依赖 workspace），因此 resolve 内部**不能使用 `tracing` 宏**。workspace 信息在 `main()` 中 logging 初始化后重新记录。[置信度：高]

## 3. main.rs — 启动编排

> 📍 **代码位置**：`src/main.rs`

启动流程严格按序执行 10 个阶段：

1. **Workspace 解析**：`Workspace::resolve()` 三级优先级解析
2. **日志初始化**：`config::LogConfig::from_workspace()` → `config::init_logging()` → 持有 `_log_guard`
3. **配置加载**：`config::AgentConfig::from_workspace()` 从 YAML 文件读取（SOUL 文件支持 workspace 回退）
4. **MCP 初始化**（可选）：`McpConfig::load_with_workspace()` → `McpManager::from_config()` 并行连接
5. **LLM Provider 创建**：`llm::create_provider()` 根据配置选择 GenAI 或 Venus
6. **Agent 创建**：`ChatAgent::new_with_workspace()` 创建 Agent + 从 workspace 加载持久化记忆
7. **MCP 附加**：`agent.attach_mcp()` 附加 MCP + 重建提示词
8. **Skill 加载**：从 workspace/skills/ + cwd/skills/ 加载技能
9. **REPL 启动**：`cli::run_interactive()` 进入主循环
10. **优雅关闭**：`agent.shutdown()` 持久化记忆 + 关闭 MCP 子进程

**设计决策**：
- MCP 初始化是 `Option<McpManager>`，缺少配置文件时自动跳过，不阻断启动
- 配置文件不存在时使用默认值，不阻断启动

[置信度：高]

## 4. config 模块 — Agent 配置 + 日志配置

> 📍 **代码位置**：`src/config.rs`（模块入口）→ `src/config/agent_config.rs` + `src/config/logging.rs`

### 模块结构

```
src/config.rs              # 模块入口（5 行，re-export）
src/config/
├── agent_config.rs        # AgentConfig + YAML 解析
└── logging.rs             # LogConfig + tracing 初始化
```

`config.rs` 作为模块入口，使用 Rust 2024 edition 的文件+目录模块模式，re-export 公共类型：
- `AgentConfig`、`DEFAULT_SYSTEM_PROMPT`（来自 `agent_config.rs`）
- `LogConfig`、`LogGuard`、`init_logging`（来自 `logging.rs`）

### 关键类型

- `const DEFAULT_SYSTEM_PROMPT` — 默认系统提示词的单一真实来源
- `struct DaedalusConfigFile` — 顶层 YAML 文件结构（内部类型）
- `struct AgentConfig` — Agent 配置聚合
- `struct LogConfig` — 日志配置

### YAML 配置文件

配置文件路径：`<workspace>/config/daedalus.yaml`

```yaml
llm:
  api_key: "sk-..."
  model: "gpt-4o"
  api_base: "https://your-proxy/v1"
  adapter_kind: "openai"
  venus:
    thinking_enabled: true
    thinking_tokens: 4096
    reasoning_effort: "high"

agent:
  name: "Daedalus"
  system_prompt: ""
  soul_file: "./SOUL.md"

logging:
  filter: "daedalus=debug"
  format: pretty
  rotation: daily
```

所有字段都有合理的默认值，配置文件不存在时使用默认配置正常启动。`LlmConfig`、`VenusExtensions`、`ReasoningEffort` 等类型均实现了 `serde::Deserialize` 以支持 YAML 反序列化。

### 双轨提示词机制

`is_custom_prompt` 标志实现了两种提示词模式：
- **自定义模式**：YAML 中 `agent.system_prompt` 设置后直接使用，跳过 PromptBuilder
- **动态模式**：未设置时通过 PromptBuilder 动态组装（含 name + soul + tools）

### Soul 人格系统

Soul 文件加载优先级：YAML 中 `agent.soul_file` 字段 > workspace `config/soul.md`。文件读取失败仅 warn 不 panic（优雅降级）。

**内部实现**：`read_trimmed_file()` 辅助函数统一处理文件读取 + trim + 空检查。`build_from_file()` 私有方法封装了从 YAML 解析结果构建 `AgentConfig` 的共享逻辑。[置信度：高]

## 5. session.rs — 会话管理

> 📍 **代码位置**：`src/agent/session.rs`（已从 `src/session.rs` 迁移至 agent 模块）
>
> Session 只被 agent 模块使用，是 agent 的内部概念，因此在模块化重构中移入 `agent/` 目录。
> 详见 [agent/overview.md](../agent/overview.md)。

[置信度：高]

## 6. logging — 结构化日志

> 📍 **代码位置**：`src/config/logging.rs`（已从 `src/logging.rs` 迁移至 config 模块）

### 特性

- **双通道输出**：stderr（开发）和滚动文件（生产），但二者互斥
- **4 种格式**：Pretty（默认）、Compact、Json、Full
- **4 种轮转策略**：Minutely、Hourly、Daily（默认）、Never
- **时区检测**：优先本地时区，回退 UTC
- **非阻塞写入**：`tracing_appender::non_blocking` 避免 IO 阻塞
- **YAML 配置**：通过 `config/daedalus.yaml` 的 `logging` 段配置

### LogGuard 生命周期约束

`LogGuard` 必须在 main 函数全生命周期内持有（`_log_guard`），drop 时会 flush 缓冲区。这是 `tracing-appender` 的 non-blocking writer 的要求。[置信度：高]

### 宏消除重复

`apply_display_opts!` 宏消除了 4 种格式变体之间重复的 `.with_file().with_line_number()...` 配置链。[置信度：高]

## 7. 构建与开发命令

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
| 2026-04-14 | 配置从环境变量迁移到 YAML 文件；config+logging 合并为 config/ 模块；session.rs 移入 agent/ 模块；更新所有代码位置和引用路径 | YAML 配置迁移 + 模块化重构 |
| 2026-04-14 | 新增 Workspace 章节（三级优先级、目录结构、pre-logging 约束）；更新启动流程为 11 阶段；更新 config.rs 章节（workspace 回退、build() 重构、read_trimmed_file 辅助函数） | Workspace 系统实现 + 架构审查优化 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
