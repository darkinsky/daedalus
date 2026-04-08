# 编码惯例与隐含规则

> 最后更新：2026-04-08
> 来源：存量代码分析
> 置信度：高

## 错误处理模式

1. **统一使用 `anyhow::Result`**：所有公共函数返回 `anyhow::Result`
2. **上下文链**：大量使用 `.context()` / `.with_context()` 提供错误上下文
3. **优雅降级**：
   - MCP 服务器连接失败 → 跳过该服务器（不阻断其他）
   - SOUL 文件读取失败 → warn + 跳过
   - MCP 配置文件不存在 → 空配置（无 MCP）
   - 日志 filter 解析失败 → 回退默认 filter
4. **无 panic**：整个代码库没有 `unwrap()` 或 `expect()` 用于可能失败的操作
5. **工具错误标记**：MCP 工具报错时加 `[Tool Error]` 前缀让 LLM 区分成功/失败

## 预留功能标记

使用 `#[allow(dead_code)]` + 注释标记预留功能（共 16 处），而非 TODO。这些预留包括：

| 位置 | 预留功能 |
|------|---------|
| `Session::created_at` | 会话持久化/UI 显示 |
| `Memory::clear()` | 显式记忆重置命令 |
| `ChatRole::Tool` / `ChatMessage::tool()` | 记忆中区分工具消息 |
| `PromptBuilder::memory_context()` | 长期记忆功能 |
| `McpClient::server_info()` | 服务器能力协商 |
| `McpClient::shutdown()` / `McpManager::shutdown()` | 应用关闭时优雅退出 |
| `McpManager::empty()` | 测试用 |

**规则**：代码库中没有 TODO/FIXME/HACK 标记，所有预留功能通过 `#[allow(dead_code)]` + 注释明确标记。

## 模块组织规范

1. **门面模式**：每个模块的 `mod.rs` 保持极简，仅做 re-export 和门面函数
2. **显式 re-export**：使用 `pub use xxx::Yyy` 而非 `pub use xxx::*`，防止命名空间污染
3. **类型封装**：Provider 特定类型（genai 类型、reqwest 类型）完全封装在各自 Provider 内部

## 异步模式

1. **运行时**：`tokio` full features，`#[tokio::main]` 入口
2. **Trait 异步**：所有异步 trait 使用 `#[async_trait]`
3. **并发**：MCP 服务器连接使用 `tokio::task::JoinSet` 并行启动
4. **同步原语**：MCP 客户端 stdin/stdout 使用 `tokio::sync::Mutex`
5. **超时**：`tokio::time::timeout` 防止 MCP 请求挂起

## 日志规范

1. 使用 `tracing` 宏（`info!`、`warn!`、`error!`、`debug!`）
2. 结构化日志字段：`session_id`, `request_id`, `provider`, `model`, `tool`, `server` 等
3. 请求/响应均有对应的 info 级别日志
4. 错误日志使用 error 级别，降级使用 warn 级别

## Trait 设计规范

1. **默认实现**：非必需方法提供默认实现（如 `AgentMode::attach_mcp()`、`LlmApi::chat()`）
2. **Send + Sync 约束**：所有 trait 要求 `Send + Sync`（tokio 异步环境需要）
3. **借用优先**：`PromptBuilder<'a>` 使用生命周期参数借用数据，避免不必要的 clone

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
