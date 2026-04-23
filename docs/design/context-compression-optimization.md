# Daedalus 上下文压缩优化方案

> 基于 Claude Code v2.1.88 泄露源码、Cursor Agent、OpenAI Codex CLI 的对比分析
>
> 分析日期：2026-04-23

---

## 目录

1. [背景与问题](#1-背景与问题)
2. [Daedalus 当前实现](#2-daedalus-当前实现)
3. [Claude Code 6 层压缩体系](#3-claude-code-6-层压缩体系)
4. [业界横向对比](#4-业界横向对比)
5. [差距分析](#5-差距分析)
6. [优化方案](#6-优化方案)
7. [实施路线图](#7-实施路线图)

---

## 1. 背景与问题

### 1.1 核心痛点

在长对话 + 多轮工具调用的 coding agent 场景中，上下文会快速膨胀：

- **System prompt**：~10K tokens（identity + tools + rules + memory）
- **工具调用结果**：每轮 grep/read_file/bash 输出可达数千 tokens
- **历史对话**：多轮 Q&A 累积

实际 trace 数据显示，一个 20+ 轮工具调用的 turn 中，prompt tokens 从 35K 膨胀到 75K：

| 轮次 | prompt tokens | 说明 |
|------|:------------:|------|
| 第 1 轮 | 35,559 | 初始上下文 |
| 第 3 轮 | 62,332 | 工具结果累积 |
| 第 7 轮 | 68,513 | 持续膨胀 |
| 最后一轮 | 75,905 | 接近预算 |

### 1.2 引发的问题

1. **语言漂移**：中文信号被大量英文工具输出稀释，模型输出日语（CJK 漂移）
2. **成本浪费**：大量 token 用于传输早期轮次的过时工具输出
3. **延迟增加**：prompt 越长，首 token 延迟越高
4. **质量下降**：上下文过长导致模型注意力分散

---

## 2. Daedalus 当前实现

### 2.1 架构概览

Daedalus 的上下文管理分为三个层面：

```
┌─────────────────────────────────────────────────────────┐
│  Layer 1: Memory Middleware (跨 turn)                    │
│  ├── add_user_message() → build_messages()              │
│  ├── add_tool_context() → summarize_tool_history()      │
│  ├── maybe_consolidate() → 提取长期记忆                  │
│  └── maybe_compact() → 压缩上下文                        │
├─────────────────────────────────────────────────────────┤
│  Layer 2: Tool Loop (同一 turn 内)                       │
│  ├── tool_history: Vec<ToolRound> → 完整工具输出          │
│  └── truncate_tool_history() → 截断早期轮次输出           │
├─────────────────────────────────────────────────────────┤
│  Layer 3: Sliding Window (消息窗口)                      │
│  └── max_messages: Some(100) → 最多保留 100 条消息        │
└─────────────────────────────────────────────────────────┘
```

### 2.2 已实现的功能

#### 2.2.1 Auto-Compact（自动上下文压缩）

**触发条件**：每轮对话结束后，`MemoryTurnMiddleware` 检查 `should_compact()`。

**文件**：`src/middleware/builtin/memory.rs`

```rust
// 触发顺序：consolidation 优先，compact 延后
let consolidation_ran = mem.should_consolidate();
mem.maybe_consolidate(&*self.llm).await;

// 如果 consolidation 刚运行，跳过 compact（避免双重 cache 失效）
if !consolidation_ran {
    mem.maybe_compact(&*self.llm).await;
}
```

**Cache-aware 阈值判断**：`src/memory/sliding_window/memory.rs`

```rust
pub fn should_compact(&self) -> bool {
    let threshold = (self.config.context_budget as f64
        * self.config.compact_threshold_ratio) as usize;

    let (system_tokens, message_tokens) = self.estimate_token_breakdown();

    // System prompt 打 75% 折扣（大部分被 prompt cache 命中）
    let cache_adjusted = system_tokens / 4 + message_tokens;

    cache_adjusted > threshold
}
```

**设计要点**：
- System prompt tokens 打 75% 折扣，因为静态前缀（identity/tools/rules）几乎总是被 prompt cache 命中
- 只有 conversation messages 的增长才是 compact 的真正触发因素
- 避免 system prompt 很大但 cache 命中率高时过早触发

#### 2.2.2 Compact 核心算法

**文件**：`src/memory/sliding_window/memory.rs` → `compact()`

```
压缩前：[msg_1] [msg_2] ... [msg_N-10] [msg_N-9] ... [msg_N]
                                         ↑ preserve_recent = 10
压缩后：[compact_summary(user)] [msg_N-9] ... [msg_N]
```

**算法步骤**：

1. **分割消息**：
   - 待压缩：`messages[0..compress_count]`
   - 保留原文：`messages[compress_count..]`（最近 `compact_preserve_recent` 条）

2. **构建压缩 prompt**：
   - System prompt：`COMPACT_SYSTEM_PROMPT`（结构化摘要指令）
   - User prompt：待压缩消息的文本表示 + 可选的自定义 focus 指令

3. **LLM 生成摘要**：
   - 输出格式：`<compact_summary>` 标签包裹
   - 包含：Task Context / Completed Actions / Current State / Key Details / Pending Items

4. **替换消息列表**：
   - 用一条 `user` 角色的摘要消息替换所有被压缩的消息
   - 使用 `user` 角色（而非 `assistant`）避免连续 assistant 消息导致 API 拒绝
   - 重置 `consolidation_cursor = 1`（跳过摘要消息）

5. **错误处理**：
   - LLM 调用失败时，原始消息**不被修改**（原子性保证）
   - 失败只记录 warning，不中断对话流

#### 2.2.3 Compact Prompt 模板

**文件**：`src/memory/sliding_window/prompts.rs`

```
COMPACT_SYSTEM_PROMPT:
  "You are a conversation compressor..."

  CRITICAL REQUIREMENTS:
  1. Preserve ALL technical details: file paths, function names, ...
  2. Preserve the current state of any ongoing task
  3. Preserve user preferences and constraints
  4. Preserve any decisions made and their rationale
  5. Do NOT include pleasantries, acknowledgments, or filler text
  6. Use a structured format with clear sections
  7. Be as concise as possible while retaining all actionable information

  Output format:
  <compact_summary>
  ## Conversation Summary
  ### Task Context
  ### Completed Actions
  ### Current State
  ### Key Details
  ### Pending Items
  </compact_summary>
```

#### 2.2.4 Consolidation/Compact 互斥

**文件**：`src/middleware/builtin/memory.rs`

**设计原因**：
- Consolidation 更新 `long_term_memory` → system prompt 动态后缀变化 → **system prompt cache 失效**
- Compact 压缩 messages → messages 前缀变化 → **message-level cache 失效**
- 同轮触发 = 双重 cache 失效 = 下一次请求完全 miss

**解决方案**：如果 consolidation 在本轮触发，跳过 compact，延迟到下一轮。两次 cache 失效被分散到两轮。

#### 2.2.5 Tool Loop 内工具历史截断

**文件**：`src/agent/tool_loop.rs`

在 tool_loop 内部，每轮 LLM 调用前，对 `tool_history` 进行截断：

```rust
const FULL_RESULT_RECENT_ROUNDS: usize = 3;
const TRUNCATED_RESULT_MAX_CHARS: usize = 500;
```

- **最近 3 轮**：工具输出保持完整（LLM 需要最新结果做决策）
- **更早轮次**：工具输出截断到 500 字符 + `"...(truncated, N bytes total)"`
- **工具调用参数**：始终保持完整（体积小，提供结构上下文）
- **原始 `tool_history` 不受影响**：tracing 和 memory 仍记录完整内容

#### 2.2.6 Memory 层工具摘要截断

**文件**：`src/middleware/builtin/memory.rs` → `summarize_tool_history()`

跨 turn 存入 memory 的工具摘要已经做了截断：
- 工具参数：截断到 200 字符
- 工具结果：截断到 500 字符

#### 2.2.7 手动 `/compact` 命令

**文件**：`src/cli/commands.rs` + `src/cli/repl.rs`

- `/compact` — 无参数，使用默认压缩
- `/compact 聚焦在认证重构上` — 带自定义 focus 指令

#### 2.2.8 配置项

**文件**：`src/memory/sliding_window/config.rs`

```yaml
memory:
  sliding_window:
    max_messages: 100              # 消息窗口上限（默认 100）
    consolidation_threshold: 100   # 触发 consolidation 的消息数
    retention_window: 50           # consolidation 保留的最近消息数
    context_budget: 128000         # Token 预算（默认 128k）
    compact_threshold_ratio: 0.8   # Auto-compact 触发阈值（80%）
    compact_preserve_recent: 10    # Compact 保留的最近消息数
```

### 2.3 当前方案的优势

| 特性 | 评价 |
|------|------|
| Cache-aware `should_compact` | ✅ 优秀。system prompt 打 75% 折扣，避免过早触发 |
| Consolidation/Compact 互斥 | ✅ 优秀。避免同轮双重 cache 失效 |
| Compact summary 用 `user` 角色 | ✅ 正确。避免连续 assistant 消息 |
| 结构化 prompt 模板 | ✅ 良好。`<compact_summary>` 标签 + 分段输出 |
| 手动 `/compact` 支持 | ✅ 良好。支持自定义 focus 指令 |
| Tool loop 内截断 | ✅ 良好。零 LLM 成本，减少 turn 内上下文膨胀 |
| 原子性保证 | ✅ 良好。LLM 失败不修改原始消息 |

---

## 3. Claude Code 6 层压缩体系

基于 Claude Code v2.1.88 泄露源码（`src/services/compact/` 目录，6 个文件）分析。

### 3.1 分层架构

```
LLM 调用前
    │
    ▼
┌───────────────────────────────────────────┐
│  Layer 1: microCompact (每次调用前)        │
│  替换旧的 tool_result 为占位符             │
│  零 LLM 成本，纯字符串替换                 │
└───────────────────────────────────────────┘
    │
    ▼ token 超过阈值?
┌───────────────────────────────────────────┐
│  Layer 2: autoCompact (自动)              │
│  保存会话记录 + AI 生成摘要 + 重建上下文    │
└───────────────────────────────────────────┘
    │
    ▼ 模型调用 compact 工具?
┌───────────────────────────────────────────┐
│  Layer 3: manual compact (手动)           │
│  用户或模型主动触发压缩                     │
└───────────────────────────────────────────┘
```

### 3.2 各层详解

#### Layer 1: microCompact ⭐⭐⭐⭐⭐

**来源**：`src/services/compact/microCompact.ts`

**核心机制**：每次 LLM 调用前，扫描消息列表，将**非最近 N 条**的 `tool_result` 替换为占位符 `[tool result for {tool_name} truncated]`。

**关键特征**：
- **零 LLM 成本**：纯字符串替换，不调用任何 API
- **保留最近 N 条完整**：LLM 需要最新的工具结果做决策
- **只替换 tool_result**：tool_call（函数名 + 参数）保持完整
- **效果巨大**：coding agent 场景中，tool_result 占上下文的 60-80%

**为什么这是最重要的层**：在一个典型的 20 轮工具调用 turn 中，每轮可能有 2-3 个工具调用，每个工具输出 1-5K tokens。如果不做 microCompact，这些输出会全部累积在 `tool_history` 中传给 LLM。microCompact 能在不损失任何决策信息的情况下，将上下文缩小 50% 以上。

#### Layer 2: autoCompact（传统压缩）⭐⭐⭐

**来源**：`src/services/compact/autoCompact.ts` + `compact.ts`

**触发条件**：
- 上下文使用量达到上下文窗口的 ~93% 时自动触发
- 阈值 = 有效上下文窗口 - 13,000 tokens 缓冲

**预警机制**：

| 阈值 | 状态 |
|------|------|
| 上下文窗口 - 20,000 tokens | ⚠️ 黄色预警 |
| 上下文窗口 - 13,000 tokens | 🚨 自动压缩触发 |
| 上下文窗口 - 3,000 tokens | 🔴 阻塞限制（强制压缩） |

**工作流程**：
1. `stripImages()` — 去除图片/文档，只保留文本标记
2. `stripReinjectedAttachments()` — 去除重复注入的附件
3. `createUserMessage(summarize_request)` — 构建压缩请求
4. 调用 AI 模型生成摘要（禁用工具调用）
5. 生成 `<analysis>` + `<summary>` 结构化输出
6. 用摘要替换原始消息

#### Layer 3: preservedSegment ⭐⭐

**核心机制**：标记关键消息为"不可压缩"，compact 跳过它们。

**与 Daedalus 的区别**：
- Daedalus：简单的"保留最近 N 条"（`compact_preserve_recent = 10`）
- Claude Code：**语义级标记**——根据消息的重要性标记，例如：
  - 用户最近的任务指令
  - 未完成的 TODO 列表
  - 关键的错误信息
  - 重要的决策点

#### Layer 4: compactBoundary ⭐⭐⭐

**核心机制**：compact 完成后，在消息列表中插入 `SystemCompactBoundaryMessage`。

**作用**：下次 compact 时，只需要处理 boundary 之后的新消息，而不是重新压缩整个历史。

**效果**：
- 第一次 compact：压缩全部历史 → O(N)
- 第二次 compact：只压缩 boundary 之后的新消息 → O(ΔN)
- compact 成本随时间**递减**

**Daedalus 的问题**：每次 compact 都是全量压缩。如果对话很长，compact 本身的 LLM 调用就会消耗大量 token。

#### Layer 5: partial compact ⭐

**核心机制**：允许只压缩对话的前半部分或后半部分，而非全量。

**使用场景**：用户手动 `/compact` 时，可以指定只压缩特定范围。

#### Layer 6: 熔断机制 ⭐⭐

**核心机制**：连续 3 次 compact 失败后停止自动重试。

**作用**：避免在 API 限流、网络问题等情况下浪费 API 调用。

---

## 4. 业界横向对比

| 特性 | Claude Code | Cursor Agent | OpenAI Codex CLI | **Daedalus** |
|------|:-----------:|:------------:|:----------------:|:------------:|
| microCompact (tool result 截断) | ✅ | ✅ (类似) | ✅ | ✅ |
| AI 摘要压缩 | ✅ | ✅ | ✅ | ✅ |
| 增量压缩 (boundary) | ✅ | ❌ | ❌ | ✅ |
| 保留段语义标记 | ✅ | ⚠️ | ❌ | ✅ (规则 + 手动) |
| Partial compact | ✅ | ❌ | ❌ | ✅ |
| 熔断机制 | ✅ | ❌ | ❌ | ✅ |
| 多级阈值 | ✅ (3级) | ✅ (2级) | ⚠️ | ✅ (3级) |
| Cache-aware 触发 | ⚠️ | ❌ | ❌ | ✅ |
| CJK token 估算 | ✅ | N/A | N/A | ✅ |
| 手动 /compact | ✅ | ✅ | ❌ | ✅ |
| Consolidation/Compact 互斥 | ❌ | N/A | N/A | ✅ |

**Daedalus 的独特优势**：
- Cache-aware 触发（system prompt 打折扣）
- Consolidation/Compact 互斥（避免双重 cache 失效）

**Daedalus 的主要差距**：
- 缺少 microCompact（最大差距）
- 缺少增量压缩（compactBoundary）
- 缺少熔断机制
- CJK token 估算不准确

---

## 5. 差距分析

### 5.1 🔴 P0: 缺少 microCompact — 最大的差距

**问题描述**：

Daedalus 目前在 `tool_loop.rs` 中实现了 `truncate_tool_history()`，对早期轮次的工具输出截断到 500 字符。但这只在 **tool_loop 内部**（同一 turn 的多轮工具调用）生效。

**跨 turn 的问题**：`summarize_tool_history()` 将工具摘要存为 assistant 消息（参数 200 chars，结果 500 chars），这些摘要会**永久留在消息列表中**。随着对话进行，几十轮工具调用的摘要会累积成巨大的上下文。

**Claude Code 的做法**：每次 LLM 调用前（不仅是 tool_loop 内部），扫描整个消息列表，将非最近 N 条的 tool_result 替换为占位符。这是零成本的（不调用 LLM），但效果巨大。

**预期收益**：减少 50%+ 的上下文 token，零 LLM 成本。

### 5.2 🟠 P1: CJK token 估算不准确

**问题描述**：

`CHARS_PER_TOKEN = 4` 对 ASCII 文本是准确的，但对 CJK 文本（中文/日文/韩文）严重低估。中文平均 ~1.5-2 chars/token，用 4 会导致：
- 估算 token 数 = 实际 token 数的 37-50%
- compact 触发过晚，可能在上下文已经溢出时才触发

**代码位置**：`src/memory/mod.rs`

```rust
pub(crate) const CHARS_PER_TOKEN: usize = 4;
```

**改进方向**：
- 方案 A：使用混合估算（检测 CJK 字符比例，动态调整）
- 方案 B：使用 tiktoken 或类似的精确 tokenizer
- 方案 C：保守策略，将默认值改为 3（折中）

### 5.3 🟠 P1: 缺少熔断机制

**问题描述**：

如果 compact 的 LLM 调用持续失败（网络问题、API 限流），当前实现会在每轮都重试 `maybe_compact()`，浪费 API 调用。

**改进方向**：添加失败计数器，连续 3 次失败后停止自动重试，直到下一次 `/compact` 手动触发或新会话开始。

### 5.4 🟡 P2: 缺少 compactBoundary 增量压缩

**问题描述**：

当前 compact 每次都是全量压缩——将所有非 preserved 消息一次性发给 LLM 生成摘要。如果对话很长（例如 compact 后又积累了 50 条消息），下次 compact 需要重新压缩"上次的摘要 + 50 条新消息"。

**Claude Code 的做法**：compact 后插入 boundary 标记。下次 compact 只处理 boundary 之后的新消息，然后将新摘要与旧摘要合并。

**预期收益**：compact 本身的 LLM 调用成本从 O(总消息数) 降为 O(新消息数)。

### 5.5 🟡 P2: 缺少多级阈值

**问题描述**：

当前只有一个 80% 的硬阈值。Claude Code 有三级：

| 阈值 | 状态 | 行为 |
|------|------|------|
| 80% | ⚠️ 黄色预警 | 通知用户上下文即将满 |
| 93% | 🚨 自动触发 | 执行 auto-compact |
| 97% | 🔴 硬限制 | 阻塞，强制 compact |

**改进方向**：添加预警和硬限制两个额外阈值。

### 5.6 🟢 P3: preservedSegment 语义标记

**问题描述**：

当前只是简单的"保留最近 N 条"，没有语义级别的重要性标记。

**改进方向**：为 `ChatMessage` 添加 `preserved: bool` 字段，允许标记关键消息（如用户的任务指令、重要决策）为不可压缩。

### 5.7 🟢 P3: Partial compact

**问题描述**：

当前 `/compact` 只支持全量压缩。

**改进方向**：支持 `/compact --range 1-50` 或 `/compact --before` / `/compact --after` 等参数。

---

## 6. 优化方案

### 6.1 P0: 实现 microCompact

**核心思路**：在 `build_messages()` 中（或在 `MemoryTurnMiddleware` 构建消息时），对非最近 N 条的 assistant 消息中的工具摘要进行截断。

**实现位置**：`src/memory/sliding_window/memory.rs` → `build_messages()` 或新增 `micro_compact()` 方法

**伪代码**：

```rust
const MICRO_COMPACT_PRESERVE_RECENT: usize = 6; // 保留最近 6 条消息的完整内容
const MICRO_COMPACT_MAX_CHARS: usize = 200;     // 旧消息截断到 200 字符

fn micro_compact(messages: &mut [ChatMessage]) {
    let total = messages.len();
    if total <= MICRO_COMPACT_PRESERVE_RECENT {
        return;
    }
    let cutoff = total - MICRO_COMPACT_PRESERVE_RECENT;
    for msg in &mut messages[..cutoff] {
        if msg.role == ChatRole::Assistant && msg.content.contains("[Tool call round") {
            // 这是一条工具摘要消息，截断它
            if msg.content.len() > MICRO_COMPACT_MAX_CHARS {
                let truncated = truncate_at_char_boundary(&msg.content, MICRO_COMPACT_MAX_CHARS);
                msg.content = format!("{}...[truncated tool context]", truncated);
            }
        }
    }
}
```

**关键设计决策**：
- 只截断 assistant 角色的工具摘要消息（包含 `[Tool call round` 标记）
- 不截断 user 消息（保留用户意图）
- 不截断最近 N 条消息（保留即时上下文）
- 在 `build_messages()` 返回前应用，不修改原始 `self.messages`

### 6.2 P1: CJK-aware token 估算

**实现位置**：`src/memory/mod.rs`

**方案**：混合估算——检测文本中 CJK 字符的比例，动态调整 chars-per-token。

```rust
fn estimate_tokens(text: &str) -> usize {
    let total_chars = text.chars().count();
    if total_chars == 0 { return 0; }

    let cjk_chars = text.chars().filter(|c| is_cjk(*c)).count();
    let ascii_chars = total_chars - cjk_chars;

    // CJK: ~1.5 chars/token, ASCII: ~4 chars/token
    let cjk_tokens = (cjk_chars as f64 / 1.5) as usize;
    let ascii_tokens = ascii_chars / 4;

    cjk_tokens + ascii_tokens
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   | // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}'   | // CJK Extension A
        '\u{3040}'..='\u{309F}'   | // Hiragana
        '\u{30A0}'..='\u{30FF}'   | // Katakana
        '\u{AC00}'..='\u{D7AF}'     // Hangul Syllables
    )
}
```

### 6.3 P1: 熔断机制

**实现位置**：`src/memory/sliding_window/memory.rs`

**方案**：在 `SlidingWindowMemory` 中添加失败计数器。

```rust
pub struct SlidingWindowMemory {
    // ... existing fields ...
    /// Consecutive auto-compact failure count for circuit breaker.
    compact_failure_count: usize,
}

const MAX_COMPACT_FAILURES: usize = 3;

pub async fn maybe_compact(&mut self, llm: &dyn LlmApi) {
    if self.compact_failure_count >= MAX_COMPACT_FAILURES {
        return; // Circuit breaker: stop retrying
    }
    if !self.should_compact() {
        return;
    }
    match self.compact(llm, None).await {
        Ok(_) => { self.compact_failure_count = 0; }
        Err(e) => {
            self.compact_failure_count += 1;
            tracing::warn!(
                error = %e,
                failures = self.compact_failure_count,
                max = MAX_COMPACT_FAILURES,
                "Auto-compact failed"
            );
        }
    }
}
```

### 6.4 P2: compactBoundary 增量压缩

**核心思路**：compact 后在消息列表中插入一个特殊的 boundary 标记。下次 compact 时，只压缩 boundary 之后的新消息。

**实现方案**：

1. 在 `ChatMessage` 中添加 `is_compact_boundary: bool` 字段（或使用特殊的 content 标记）
2. `compact()` 完成后，在摘要消息上标记 `is_compact_boundary = true`
3. 下次 `compact()` 时，找到最近的 boundary，只压缩 boundary 之后的消息
4. 新摘要 = "旧摘要 + 新消息的摘要"（合并）

**伪代码**：

```rust
pub async fn compact(&mut self, llm: &dyn LlmApi, instruction: Option<&str>) -> Result<CompactResult> {
    // 找到最近的 compact boundary
    let boundary_idx = self.messages.iter().rposition(|m| m.is_compact_boundary);

    let (compress_start, existing_summary) = match boundary_idx {
        Some(idx) => (idx + 1, Some(&self.messages[idx].content)),
        None => (0, None),
    };

    // 只压缩 boundary 之后的消息
    let messages_to_compress = &self.messages[compress_start..compress_count];

    // 如果有旧摘要，将其作为上下文传给 LLM
    let prompt = if let Some(summary) = existing_summary {
        format!("Previous summary:\n{}\n\nNew messages to compress:\n{}", summary, messages_text)
    } else {
        messages_text
    };

    // ... 调用 LLM 生成新摘要 ...

    // 新摘要标记为 boundary
    let summary_msg = ChatMessage::user(format!("[Compact summary]\n\n{}", new_summary))
        .with_compact_boundary(true);
}
```

### 6.5 P2: 多级阈值

**实现位置**：`src/memory/sliding_window/config.rs`

```rust
pub struct SlidingWindowConfig {
    // ... existing fields ...

    /// Warning threshold ratio (default: 0.8). Logs a warning when exceeded.
    pub compact_warning_ratio: f64,
    /// Auto-compact threshold ratio (default: 0.93). Triggers auto-compact.
    pub compact_threshold_ratio: f64,
    /// Hard limit ratio (default: 0.97). Forces compact even if it just ran.
    pub compact_hard_limit_ratio: f64,
}
```

---

## 7. 实施路线图

### Phase 1: 快速收益（1-2 天）✅ **已完成**

| 任务 | 优先级 | 预期收益 | 实现难度 | 状态 |
|------|:------:|:--------:|:--------:|:----:|
| microCompact | 🔴 P0 | 🔥🔥🔥🔥🔥 | 低 | ✅ 已实现 |
| 熔断机制 | 🟠 P1 | 🔥🔥 | 极低 | ✅ 已实现 |
| CJK-aware token 估算 | 🟠 P1 | 🔥🔥🔥 | 低 | ✅ 已实现 |

### Phase 2: 架构优化（3-5 天）✅ **已完成**

| 任务 | 优先级 | 预期收益 | 实现难度 | 状态 |
|------|:------:|:--------:|:--------:|:----:|
| compactBoundary 增量压缩 | 🟡 P2 | 🔥🔥🔥 | 中 | ✅ 已实现 |
| 多级阈值 | 🟡 P2 | 🔥🔥 | 低 | ✅ 已实现 |

### Phase 3: 高级特性 ✅ **已完成**

| 任务 | 优先级 | 预期收益 | 实现难度 | 状态 |
|------|:------:|:--------:|:--------:|:----:|
| preservedSegment 语义标记 | 🟢 P3 | 🔥🔥 | 高 | ✅ 已实现 |
| Partial compact | 🟢 P3 | 🔥 | 中 | ✅ 已实现 |

---

## 附录 A: 核心设计哲学对比

| 维度 | Claude Code | Daedalus |
|------|-------------|----------|
| **压缩哲学** | "能不调 LLM 就不调 LLM" — microCompact 用字符串替换解决大部分问题 | ✅ microCompact + AI 摘要双层压缩 |
| **阈值策略** | 激进（93% 才触发），留更多空间给对话 | ✅ 三级阈值（80% 预警 / 93% 自动 / 97% 强制） |
| **增量性** | 支持增量压缩（boundary），成本递减 | ✅ 支持增量压缩（boundary），成本递减 |
| **容错性** | 熔断机制，3 次失败停止 | ✅ 熔断机制，3 次失败停止 |
| **Cache 感知** | 无显式 cache 感知 | ✅ system prompt 打折扣 |
| **CJK 感知** | ✅ 精确 tokenizer | ✅ CJK-aware 启发式估算 |
| **语义保留** | ✅ preservedSegment 语义标记 | ✅ 规则自动标记 + 手动标记 |
| **部分压缩** | ✅ partial compact | ✅ --before/--after/--range |

## 附录 B: 相关文件索引

| 文件 | 职责 |
|------|------|
| `src/memory/sliding_window/config.rs` | Compact 配置项定义 |
| `src/memory/sliding_window/memory.rs` | Compact 核心算法实现 |
| `src/memory/sliding_window/prompts.rs` | Compact LLM prompt 模板 |
| `src/memory/mod.rs` | Memory trait 中的 compact 接口定义 |
| `src/middleware/builtin/memory.rs` | Auto-compact 触发逻辑 |
| `src/agent/mod.rs` | AgentMode trait 中的 compact 方法 |
| `src/agent/chat.rs` | ChatAgent 的 compact 实现 |
| `src/agent/tool_loop.rs` | Tool loop 内工具历史截断 |
| `src/cli/commands.rs` | `/compact` 命令定义 |
| `src/cli/repl.rs` | `/compact` 命令处理 |
