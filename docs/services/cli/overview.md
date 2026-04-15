# CLI — 交互式 REPL 界面

> 最后更新：2026-04-15
> 来源：存量代码分析 + 工具事件渲染迭代 + 非交互模式实现 + 代码质量审查优化

## 1. 模块概述

CLI 模块提供 Claude Code 风格的终端交互界面，包含 REPL 主循环、斜杠命令解析、Markdown 渲染、Token 费用跟踪和 Tab 自动补全。

## 2. 模块结构

| 文件 | 职责 |
|------|------|
| `cli/mod.rs` | 门面模块，暴露 `run_interactive()` / `run_print()` / `read_stdin_prompt()` |
| `cli/repl.rs` | REPL 主循环 |
| `cli/commands.rs` | 斜杠命令定义与解析 |
| `cli/render.rs` | 终端输出渲染（Markdown、样式、spinner）+ 共享文本工具函数 |
| `cli/cost.rs` | Session 级 Token 用量累计 |
| `cli/completer.rs` | Tab 补全 + 内联幽灵提示 |
| `cli/cli_args.rs` | CLI 参数定义（clap derive） |
| `cli/output_format.rs` | 输出格式类型和序列化（text/json/stream-json） |
| `cli/print_runner.rs` | 非交互模式执行器 |

## 3. REPL 主循环

> 📍 **代码位置**：`src/cli/repl.rs`

使用 `rustyline::Editor` 实现带行编辑的终端输入，配置：
- `CompletionType::List` — 列表式补全
- `EditMode::Emacs` — Emacs 键位
- `auto_add_history(true)` — 自动记录历史

**每轮循环**：
```
读取输入 → 判断斜杠命令？
  YES → handle_command() → 可能退出
  NO  → 判断 quit/exit？
    YES → 退出
    NO  → handle_chat() → spinner → agent.chat() → 渲染响应
```

**特殊键处理**：
- Ctrl-C → 继续（不退出）
- Ctrl-D → 优雅退出
- 直接输入 `quit`/`exit` → 退出

## 4. 斜杠命令

> 📍 **代码位置**：`src/cli/commands.rs`

| 命令 | 别名 | 功能 |
|------|------|------|
| `/help` | `/h`, `/?` | 显示帮助 |
| `/new` | `/compact` | 新建会话（清历史 + 重置费用） |
| `/clear` | — | 清屏（保留历史） |
| `/cost` | — | 显示 Token 用量 |
| `/model` | — | 显示模型信息 |
| `/tools` | — | 列出 MCP 工具 |
| `/exit` | `/quit` | 退出 |

解析不区分大小写，未知命令显示警告。

## 5. 终端渲染

> 📍 **代码位置**：`src/cli/render.rs`

- **Markdown 渲染**：使用 `termimad::MadSkin` 渲染 Assistant 响应
- **思考过程**：💭 标记 + dim 样式 + 竖线边框（`┊`）
- **工具执行进度**：实时展示工具调用过程（见下方 5.1 节）
- **响应 footer**：`↑10 · ↓5 · 1.2s` 格式显示 token 用量和耗时
- **Spinner**：自定义 Unicode Braille 字符动画（`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`），用 `Arc` 包装以便在工具事件回调中暂停/恢复

视觉风格大量使用 ANSI 颜色（Cyan 主色、DarkGrey 辅助、Yellow 警告、Red 错误），模仿 Claude Code 的终端体验。[置信度：高]

### 5.1 工具执行事件渲染

> 📍 **代码位置**：`src/cli/render.rs:tool_event()` + `src/cli/repl.rs:build_tool_event_callback()`

当 LLM 请求工具调用时，CLI 实时展示每一步的执行过程，而非只显示 spinner 等待最终结果。

**终端效果**：
```
  🔧 Tool round 1
  ▸  list_directory (built-in)
  ▸  read_file (built-in)
    ✓ Found 12 entries in /data/workspace/...
    ✓ File content (245 lines)...
    2 tool call(s) completed

  ⠋ Thinking…
```

**Spinner 协调机制**：
- `repl.rs` 中 spinner 用 `Arc<ProgressBar>` 包装
- `build_tool_event_callback()` 构建回调闭包，闭包中先 `finish_and_clear()` 暂停 spinner，输出事件后 `enable_steady_tick()` 恢复
- 这确保工具事件输出不与 spinner 动画交错

[置信度：高]

## 6. Tab 补全与内联提示

> 📍 **代码位置**：`src/cli/completer.rs`

`SlashCommandHelper` 实现了 rustyline 的四个 trait：
- **Completer**：输入 `/` 后列出匹配命令
- **Hinter**：唯一匹配时显示灰色幽灵后缀文本
- **Highlighter**：提示文本用 dim 样式
- **Validator**：空实现

## 7. SessionCost

> 📍 **代码位置**：`src/cli/cost.rs`

简单累加器，跟踪 session 级别的 prompt_tokens、completion_tokens 和 requests 计数。`/new` 命令时重置。

## 8. 非交互模式（Print Mode）

> 📍 **代码位置**：`src/cli/print_runner.rs` + `src/cli/output_format.rs`

`--print` / `-p` 标志启动非交互模式，执行单次 prompt 后退出。

**模块结构**：
- `print_runner.rs` — 主执行器 `run()` + 辅助函数 `emit_success()` / `emit_error()`
- `output_format.rs` — `StreamEvent` 枚举、`ResultPayload` 结构体、NDJSON 序列化

**输出格式**：

| 格式 | 说明 | 用途 |
|------|------|------|
| `text` | 结果到 stdout，进度到 stderr | Unix 管道 |
| `json` | 完成后输出单个 JSON 对象 | 脚本解析 |
| `stream-json` | NDJSON 事件流 | IDE 集成 |

**工具事件回调**：
- `stream-json` 模式：`build_stream_json_callback()` 将 `ToolEvent` 转换为 `StreamEvent` NDJSON
- `text` 模式：`build_text_stderr_callback()` 将工具进度输出到 stderr（复用 `render.rs` 的共享截断函数）
- `json` 模式：静默，无回调

### 8.1 共享文本工具函数

> 📍 **代码位置**：`src/cli/render.rs`

`render.rs` 中提供了两个 `pub(super)` 共享函数，供 `print_runner.rs` 复用：

| 函数 | 用途 |
|------|------|
| `truncate_chars(s, max_chars) -> String` | UTF-8 安全的字符级截断（多字节字符不会 panic） |
| `format_truncated_output(lines) -> Vec<String>` | 工具输出头尾保留截断（纯函数，返回格式化行） |

这消除了 `print_runner.rs` 中约 30 行与 `render.rs` 重复的截断逻辑，并修复了原有 `&s[..N]` 按字节截断导致多字节字符 panic 的问题。

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-15 | 新增非交互模式章节（8 节）；新增共享文本工具函数章节（8.1 节）；更新模块结构表格 | 非交互模式实现 + 代码质量审查优化 |
| 2026-04-09 | 新增工具执行事件渲染（5.1 节）；Spinner 改为 Arc 包装 | 工具事件渲染迭代 |
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
