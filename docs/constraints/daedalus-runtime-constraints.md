# 运行时约束

> 最后更新：2026-04-13
> 来源：存量代码分析 + 代码审查改进 + 并行化迭代 + 记忆系统重构
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
| `consolidation_threshold` (default) | 100 | `src/memory/config.rs` | 触发记忆整合的未整合消息数阈值 |
| `retention_window` (default) | 50 | `src/memory/config.rs` | 整合时保留的最近消息数（不被整合） |

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

**整合范围**：`messages[last_consolidated .. messages.len() - retention_window]`。整合后 `last_consolidated` 游标推进到 `messages.len() - retention_window`。

**注意**：当前整合接口已就绪（`should_consolidate()`、`messages_to_consolidate()`、`apply_consolidation()`），但尚未接入实际的 LLM 整合调用。Agent 层的整合触发逻辑已预留（`chat()` 方法中检查 `should_consolidate()`）。

## 环境变量依赖

- **必须**：`OPENAI_API_KEY`（缺失时启动失败）
- **可选但影响行为**：`DAEDALUS_MODEL`（默认 gpt-4o）、`OPENAI_BASE_URL`、`DAEDALUS_ADAPTER_KIND`
- **Venus 触发条件**：设置 `DAEDALUS_THINKING_ENABLED` 或 `DAEDALUS_THINKING_TOKENS` 后自动切换到 VenusProvider

## 日志互斥约束

> 📍 **代码位置**：`src/logging.rs:265-311`

当配置了 `DAEDALUS_LOG_DIR` 时，日志**仅输出到文件**，不再输出到 stderr。这是一个刻意的设计选择，但意味着无法同时查看 stderr 和文件日志。

## MCP 配置搜索约束

> 📍 **代码位置**：`src/mcp/config.rs:55-82`

配置搜索是**先到先得**的：如果 `DAEDALUS_MCP_CONFIG` 环境变量指向的文件存在，就不再检查 `./mcp.json` 和 `~/.config/daedalus/mcp.json`。

## 工具调用并行执行

> 📍 **代码位置**：`src/agent/chat.rs:320-340`

同一轮中的多个工具调用是**并行执行**的（`futures::future::join_all`）。总耗时 = max(各工具耗时)，而非 sum(各工具耗时)。

**技术选型决策**：选择 `futures::future::join_all` 而非 `tokio::task::JoinSet`，因为 `ToolRouter::execute` 需要 `&self` 引用，而 `ToolRouter` 不是 `'static`，无法直接 spawn。`join_all` 不需要 `'static` 约束，更适合此场景。

**事件发射顺序**：所有 `ToolCallStart` 事件先发出，然后并行执行，最后所有 `ToolCallComplete` 事件一起发出。

## LogGuard 生命周期约束

> 📍 **代码位置**：`src/logging.rs:242-244`

`LogGuard` 必须在整个应用生命周期内持有。提前 drop 会导致文件日志缓冲区可能未完全 flush。

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-13 | 新增记忆整合约束（consolidation_threshold、retention_window、整合范围） | 记忆系统重构 |
| 2026-04-09 | 工具调用从串行改为并行执行；新增 futures 0.3 依赖；补充技术选型决策 | 并行化迭代 |
| 2026-04-08 | 新增 IGNORED_DIRS 常量、工具摘要截断约束 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
