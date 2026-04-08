# CLI — 交互式 REPL 界面

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

CLI 模块提供 Claude Code 风格的终端交互界面，包含 REPL 主循环、斜杠命令解析、Markdown 渲染、Token 费用跟踪和 Tab 自动补全。

## 2. 模块结构

| 文件 | 职责 |
|------|------|
| `cli/mod.rs` | 门面模块，暴露 `run_interactive()` |
| `cli/repl.rs` | REPL 主循环 |
| `cli/commands.rs` | 斜杠命令定义与解析 |
| `cli/render.rs` | 终端输出渲染（Markdown、样式、spinner） |
| `cli/cost.rs` | Session 级 Token 用量累计 |
| `cli/completer.rs` | Tab 补全 + 内联幽灵提示 |

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
- **响应 footer**：`↑10 · ↓5 · 1.2s` 格式显示 token 用量和耗时
- **Spinner**：自定义 Unicode Braille 字符动画（`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`）

视觉风格大量使用 ANSI 颜色（Cyan 主色、DarkGrey 辅助、Yellow 警告、Red 错误），模仿 Claude Code 的终端体验。[置信度：高]

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

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
