# MCP — Model Context Protocol 客户端与工具管理

> 最后更新：2026-04-14
> 来源：存量代码分析 + 代码审查改进 + Workspace 系统实现 + 架构审查优化

## 1. 模块概述

MCP 模块实现了 Model Context Protocol 客户端，通过 stdio 与 MCP 服务器进程通信。包含配置加载、客户端连接、工具发现和工具执行四个核心功能。

## 2. 模块结构

| 文件 | 职责 |
|------|------|
| `mcp/config.rs` | MCP 配置加载（JSON 文件） |
| `mcp/client.rs` | 单个 MCP 服务器的 stdio 客户端 |
| `mcp/manager.rs` | 管理所有 MCP 客户端，提供统一接口 |
| `mcp/types.rs` | JSON-RPC 2.0 和 MCP 协议类型定义 |

## 3. 配置加载

> 📍 **代码位置**：`src/mcp/config.rs`

配置搜索顺序（先到先得）：
1. `DAEDALUS_MCP_CONFIG` 环境变量
2. `./mcp.json`（当前目录）
3. Workspace `config/mcp.json`（仅 `load_with_workspace()` 模式）
4. `~/.config/daedalus/mcp.json`（用户配置目录，legacy 回退）
5. 都没有 → 返回空配置（无 MCP 服务器），**不报错**

**内部实现**：搜索链的公共步骤（env var + local file）提取为 `try_common_paths()` 私有方法，legacy home 路径提取为 `try_legacy_home_path()`。`load()` 和 `load_with_workspace()` 都委托给这些辅助方法，消除了代码重复。`load_with_workspace()` 仅在中间插入 workspace 特有的搜索步骤。

JSON 格式兼容 Claude Code / Cursor 的 MCP 配置（`mcpServers` 字段名）：

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": {}
    }
  }
}
```

[置信度：高]

## 4. McpClient — 单服务器客户端

> 📍 **代码位置**：`src/mcp/client.rs`

### 通信协议

JSON-RPC 2.0 over stdio，每行一个 JSON 消息。使用 `tokio::sync::Mutex` 保护 stdin/stdout 的并发安全。

### MCP 握手流程

```
Client → Server: initialize (protocolVersion: "2024-11-05", capabilities, clientInfo)
Client ← Server: InitializeResult (protocolVersion, capabilities, serverInfo)
Client → Server: notifications/initialized（通知，无需响应）
Client → Server: tools/list
Client ← Server: ToolsListResult（工具定义列表）
```

### 超时保护

| 常量 | 值 | 用途 |
|------|-----|------|
| `MCP_REQUEST_TIMEOUT` | 30s | 普通请求超时 |
| `MCP_INIT_TIMEOUT` | 60s | 初始化握手超时（服务器可能需要启动时间） |

### 生命周期

1. `new()` — 生成子进程 → 握手 → 发现工具
2. 运行时 — `call_tool()` 执行工具调用
3. 清理 — `Drop` 实现 best-effort kill 子进程

[置信度：高]

## 5. McpManager — 统一管理器

> 📍 **代码位置**：`src/mcp/manager.rs`

### 核心方法

| 方法 | 职责 |
|---|---|
| `from_config()` | 使用 `JoinSet` **并行**连接所有服务器 |
| `has_tools()` | 返回是否有可用工具（`tool_count() > 0`） |
| `build_tool_definitions()` | 生成 OpenAI 格式 JSON 工具定义列表 |
| `tool_infos()` | 生成 CLI 显示用 `ToolInfo` 列表 |
| `call_tool()` | 自动路由到正确服务器 → 执行 → 拼接文本结果 |

### 设计亮点

- **并行连接**：`tokio::task::JoinSet` 并行启动所有服务器，加速初始化
- **优雅降级**：连接失败的服务器仅 error 日志，不阻断其他服务器
- **自动路由**：`call_tool()` 通过 `find_server_for_tool()` 自动查找工具所属服务器
- **错误标记**：MCP 工具报错时加 `[Tool Error]` 前缀让 LLM 区分成功/失败

[置信度：高]

## 6. 协议类型

> 📍 **代码位置**：`src/mcp/types.rs`

### JSON-RPC 2.0 层

- `JsonRpcRequest` — Serialize（id, method, params）
- `JsonRpcResponse` — Deserialize（result, error）
- `JsonRpcError` — 实现 Display

### MCP 协议层

- `ToolDefinition` — 工具定义（name, description, inputSchema）
  - `to_openai_json()` 转换为 OpenAI function-calling 中间格式
- `ToolCallResult` — 工具调用结果（content[], isError）

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-14 | 更新配置搜索链为 5 级（含 workspace）；更新内部实现（try_common_paths/try_legacy_home_path 重构）；修正 tool_descriptions → tool_infos 命名 | Workspace 系统实现 + 架构审查优化 |
| 2026-04-08 | 新增 has_tools() 方法；显式 import 替代 glob import | 代码审查改进 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
