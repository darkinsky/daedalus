# 编码惯例与隐含规则

> 最后更新：2026-04-13
> 来源：存量代码分析 + 代码审查改进 + 记忆系统重构代码审查
> 置信度：高

## 命名规范

1. **避免语言关键字前缀**：不使用 Rust 关键字作为字段名前缀。例如使用 `function_name` 而非 `fn_name`（`fn` 是 Rust 关键字），使用 `arguments` 而非 `fn_arguments`。
2. **不过度缩写**：变量名使用完整单词而非缩写。例如 `definitions` 而非 `defs`，`descriptions` 而非 `descs`。
3. **同一概念统一命名**：同一个概念在不同位置必须使用相同的名称。例如“自定义提示词覆盖”统一为 `prompt_override`，不在不同地方交替使用 `custom_system_prompt` 和 `custom_override`。
4. **方法名反映完整副作用**：如果方法有多个副作用，命名应反映全部行为。例如 `reset_with_updated_prompt()` 而非 `rebuild_prompt()`（因为它还会重置 session）。
5. **语义精确的方法名**：`has_tools()` 优于 `has_servers()`（有服务器不一定有工具）。方法名应精确表达调用者关心的语义。
6. **消除同义字段混淆**：当结构体中有语义相近的字段时，命名必须明确区分。例如 `messages`（对话消息列表）vs `history_log`（事件摘要日志），而非都叫 `history`。
7. **纯函数不作为关联方法**：不依赖 `self` 的纯函数应定义为模块级函数，而非 `impl` 块中的关联方法。放在 `impl` 中会误导读者以为它与实例状态有关。

## 魔法常量提取

1. **硬编码列表提取为常量**：当多个字符串在代码中以列表形式出现时，提取为命名常量。例如 `IGNORED_DIRS: &[&str] = &["node_modules", "target", "__pycache__", ".git"]`。
2. **截断阈值明确化**：工具调用摘要中的截断长度（参数 200 字符、结果 500 字符）通过独立函数 `truncate_at_char_boundary()` 实现，而非内联硬编码。
3. **`Option` 替代魔数**：当参数的某个特殊值表示"无限制"或"不适用"时，使用 `Option<T>` 而非魔数约定。例如 `limit: Option<usize>`（`None` = 不限制）优于 `limit: usize`（`0` = 不限制），后者需要读者记住魔数含义。

## 迭代器与副作用

1. **副作用不用 `map` + `collect`**：当迭代的目的是执行副作用（如发射事件、写日志）而非转换数据时，使用 `for` 循环而非 `map().collect()`。后者是函数式反模式，且常需要 `let _ = result;` 来抑制未使用警告。

## 注释与文档字符串准确性

1. **注释必须反映实际行为**：不得夸大功能。例如如果函数只是简单拼接路径，不应声称“preventing directory traversal attacks”或“canonicalized absolute path”。
2. **复杂模式加注释**：当使用不直观的模式（如 `[Option].into_iter().flatten()`）时，添加解释性注释降低认知负担。

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
| 2026-04-13 | 新增：消除同义字段混淆、纯函数不作为关联方法、Option 替代魔数、副作用不用 map+collect 规则；truncate_for_summary 更名为 truncate_at_char_boundary | 记忆系统重构代码审查 |
| 2026-04-08 | 新增命名规范、魔法常量提取、注释准确性规则 | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
