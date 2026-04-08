# Memory — 会话记忆抽象与滑动窗口

> 最后更新：2026-04-08
> 来源：存量代码分析

## 1. 模块概述

Memory 模块定义了会话记忆的统一接口（`Memory` trait）和当前唯一的实现 `SlidingWindowMemory`。

## 2. Memory Trait

> 📍 **代码位置**：`src/memory/mod.rs`

```rust
pub trait Memory: Send + Sync {
    fn add_user_message(&mut self, content: &str);
    fn add_assistant_message(&mut self, content: &str);
    fn add_tool_context(&mut self, context: &str) { self.add_assistant_message(context); }
    fn build_messages(&self) -> Vec<ChatMessage>;
    fn clear(&mut self);
    fn turn_count(&self) -> usize;
    fn strategy_name(&self) -> &str;
}
```

**设计要点**：
- `add_tool_context()` 有默认实现（委托给 `add_assistant_message`），区分工具上下文和普通助手消息
- `clear()` 标记为 `#[allow(dead_code)]`，预留给未来的显式记忆重置命令
- 注释中提到未来可能增加 **Summary-based memory** 和 **RAG-based memory**

[置信度：高]

## 3. SlidingWindowMemory

> 📍 **代码位置**：`src/memory/sliding_window.rs`

### 结构

```
SlidingWindowMemory {
    system_message: ChatMessage,  // 系统提示词（始终包含）
    history: Vec<ChatMessage>,    // 完整历史（内部保留所有消息）
    max_turns: Option<usize>,     // None = 无限制，Some(n) = 最近 n 轮
}
```

### build_messages() 逻辑

1. 系统消息始终在首位
2. 如果 `max_turns` 为 None → 返回全部历史
3. 如果 `max_turns` 为 Some(n) → 取最后 n×2 条消息（每轮 = user + assistant = 2 条）
4. 未回复的 user 消息（pending）也会包含在窗口内

**重要**：滑动窗口只影响 `build_messages()` 的输出，内部始终保留完整历史。这意味着扩大窗口可以"恢复"之前的上下文。[置信度：高]

### 工厂构造

`ChatAgent::new()` 默认使用 `SlidingWindowMemory::unlimited()`（无限窗口），即保留全部对话历史。

### 测试覆盖

12 个单元测试覆盖：无限模式、窗口内/超窗口/边界条件、pending 消息、清除等场景。

---

*变更历史*
| 日期 | 变更 | 来源 |
|------|------|------|
| 2026-04-08 | 初始创建 | 存量代码分析 Phase A |
