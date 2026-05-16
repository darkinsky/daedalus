# Daedalus vs Claude Code — 性能关键领域差距分析与优化方案

> **版本**: v1.0
> **日期**: 2026-05-16
> **范围**: 上下文压缩、记忆系统、Prompt Caching、Token 管理、可观测性
> **方法**: 源码级对比分析（Daedalus Rust 实现 vs Claude Code TypeScript 源码）

---

## 一、总体评估

```
Daedalus 性能成熟度（相对 Claude Code）

上下文压缩     ██████████░░  80%   — 缓存感知 micro_compact 已实现
记忆系统       ████████████  100%+ — 6 种策略，超越 Claude Code
Prompt Caching ████████░░░░  70%   — 多点标记 + 中断检测已实现
上下文分析     ████████░░░░  70%   — /context 命令 + 建议系统已实现
工具输出优化   ██████████░░  85%   — ReadOnlyCache 独有优势
Token 管理     ████████░░░░  70%   — 估算可用，压力告警已实现
```

---

## 二、上下文压缩

### 2.1 现状对比

| 维度 | Daedalus | Claude Code | 差距 |
|------|----------|-------------|:----:|
| 全量压缩 (compact) | LLM 摘要，80%/90% 两级阈值 | 多策略（会话内存/响应式/传统） | 🟡 |
| 微压缩 (micro_compact) | 零成本截断旧工具上下文 | **三级**：时间触发 + 缓存编辑 + 内容清除 | 🔴 |
| 工具历史截断 | `truncate_tool_history()` 渐进截断 | 通过 microCompact 统一管理 | 🟢 |
| 上下文压力感知 | 三级压力 + budget hint 注入 | autocompact + 上下文建议 | 🟢 |
| 强制终止 | `force_final_response()` | 类似机制 | 🟢 |

### 2.2 核心差距：缓存感知微压缩

**Claude Code 的 `cachedMicrocompactPath` 是最关键的性能优化**：

```
┌─ Daedalus 当前方式 ─────────────────────────────────────┐
│                                                          │
│  micro_compact() → 直接修改消息内容 → 缓存前缀改变       │
│                                     → prompt cache 失效  │
│                                     → 下一轮全量重算      │
│                                     → 成本 ↑↑↑          │
└──────────────────────────────────────────────────────────┘

┌─ Claude Code 方式 ──────────────────────────────────────┐
│                                                          │
│  cachedMicrocompactPath()                                │
│    → 本地消息内容不变                                     │
│    → API 层添加 cache_reference + cache_edits            │
│    → 服务端"虚拟删除"旧工具结果                           │
│    → prompt cache 前缀保持不变                            │
│    → 下一轮缓存命中 → 成本 ↓↓↓                          │
└──────────────────────────────────────────────────────────┘
```

**影响量化**：
- 在 10+ 轮工具调用的会话中，每次微压缩都会破坏缓存前缀
- 以 128K context、50% 缓存命中率估算，每次缓存失效额外消耗 ~64K input tokens
- 一次长会话中可能触发 3-5 次微压缩 → 额外消耗 192K-320K tokens

### 2.3 Claude Code 微压缩三级策略详解

| 层级 | 触发条件 | 机制 | 是否破坏缓存 |
|:----:|---------|------|:----------:|
| L1: 时间触发 | 距上次助手消息 >N 分钟 | 直接清除旧工具结果内容 | ✅ 破坏（缓存已冷） |
| L2: 缓存编辑 | 工具数量超阈值 | `cache_edits` API 虚拟删除 | ❌ 保持 |
| L3: 自动压缩 | 上下文超阈值 | LLM 全量摘要压缩 | ✅ 破坏 |

**关键洞察**：L1 在缓存已经冷的时候触发（长时间未活动），所以破坏缓存无所谓；L2 在缓存热的时候触发，通过 API 层操作避免破坏；L3 是最后手段。

---

## 三、记忆系统

### 3.1 现状对比

| 维度 | Daedalus | Claude Code | 差距 |
|------|----------|-------------|:----:|
| 策略丰富度 | **6 种**（SlidingWindow, Cheatsheet, Agentic, ACE, Wiki, MemPalace） | 3 种（AutoMem, TeamMem, Kairos） | 🟢 **优势** |
| 跨会话持久化 | `session_state.rs` + `long_term.rs` | 文件系统 `~/.claude/projects/` | 🟢 |
| 记忆注入 | Memory middleware → 消息序列 | `getClaudeMds()` → system prompt | 🟢 |
| 团队记忆同步 | **无** | TeamMem API + ETag 乐观锁 + 秘密扫描 | 🔴 |
| 自动反思 | `reflect_on_turn()` 每轮 | AutoMem 自动记录 | 🟢 |
| 合并去重 | `maybe_consolidate()` LLM 合并 | 依赖文件级管理 | 🟢 **优势** |
| 记忆类型分类 | 策略隐式分类 | 显式 5 种类型（User/Project/Local/TeamMem/AutoMem） | 🟡 |

### 3.2 评估结论

记忆系统是 Daedalus 的**明显优势领域**。6 种记忆策略（特别是 Dynamic Cheatsheet、A-MEM 知识图谱、Memory Palace 空间记忆）远超 Claude Code 的 3 种基于文件的模式。

唯一缺失是**团队记忆同步**，但这更多是产品需求而非性能问题。

---

## 四、Prompt Caching

### 4.1 现状对比

| 维度 | Daedalus | Claude Code | 差距 |
|------|----------|-------------|:----:|
| cache_control 标记 | system message 标记 `Ephemeral` | 多点标记（system prompt + 截断工具 + 缓存编辑） | 🟡 |
| 缓存断点策略 | "首个被截断工具轮次"作为稳定边界 | 同 + `cache_edits` 保持前缀稳定 | 🔴 |
| 缓存命中追踪 | `cache_creation_tokens` / `cache_read_tokens` | 同 + `PROMPT_CACHE_BREAK_DETECTION` | 🟡 |
| 缓存中断检测 | **无** | 检测 cache_read 突然下降 → 区分正常压缩 vs 异常 | 🔴 |

### 4.2 核心差距：缓存中断检测

Claude Code 通过 `PROMPT_CACHE_BREAK_DETECTION` 特性：
- 监控每次 API 调用的 `cache_read_input_tokens`
- 当缓存读取从高位突降到 0 时，检查是否有预期原因（微压缩、compact 等）
- 没有预期原因 → 告警，说明消息构造可能出了问题
- 有预期原因 → 通过 `notifyCacheDeletion()` 抑制误报

**这对于调试和优化 prompt caching 效率至关重要。**

---

## 五、上下文分析与可观测性（最大差距）

### 5.1 现状对比

| 维度 | Daedalus | Claude Code | 差距 |
|------|----------|-------------|:----:|
| `/context` 命令 | **无** | `analyzeContextUsage()` 分析 8+ 类别 | 🔴 严重 |
| 上下文可视化 | **无** | 网格视图 + 分类统计 + 百分比 | 🔴 严重 |
| 优化建议 | **无** | 5 类检查（容量/大工具/读取膨胀/记忆膨胀/压缩状态） | 🔴 严重 |
| 重复文件读取检测 | **无** | `duplicateFileReads` 追踪重复读取浪费 | 🔴 |
| Token 分类统计 | **无** | 按工具类型/消息类型/附件类型分别统计 | 🔴 |

### 5.2 Claude Code 上下文分析架构

```
/context 命令触发
    │
    ▼
analyzeContextUsage()
    ├── countSystemTokens()           → System prompt token 占用
    ├── countMemoryFileTokens()       → 记忆文件 token 占用
    ├── countBuiltInToolTokens()      → 内置工具 schema token
    ├── countMcpToolTokens()          → MCP 工具 token
    ├── countCustomAgentTokens()      → 自定义代理 token
    ├── countSkillTokens()            → Skill token
    ├── countSlashCommandTokens()     → 命令 token
    └── approximateMessageTokens()    → 消息 token（含工具调用/结果分类）
    │
    ▼
生成 ContextData
    ├── categories: 各类别 token 统计
    ├── gridRows: 可视化网格（10x10 或 20x10）
    ├── messageBreakdown: 消息内 token 分布
    ├── autoCompactThreshold: 压缩阈值
    └── apiUsage: 实际 API 使用量
    │
    ▼
contextSuggestions.ts
    ├── checkNearCapacity()           → "接近上限，建议 /compact"
    ├── checkLargeToolResults()       → "XX 工具消耗了 30% 上下文"
    ├── checkReadResultBloat()        → "同一文件读了 5 次"
    ├── checkMemoryBloat()            → "记忆文件占了 20%"
    └── checkAutoCompactDisabled()    → "建议启用自动压缩"
```

**这是对用户体验影响最大的缺失。** 用户在长会话中无法知道 token 花在了哪里，无法做出优化决策。

---

## 六、工具输出优化

### 6.1 现状对比

| 维度 | Daedalus | Claude Code | 差距 |
|------|----------|-------------|:----:|
| 输出截断 | bash 256KB, grep 128KB | `maxResultSizeChars` per-tool | 🟢 |
| 只读缓存 | **`ReadOnlyCache`** + 写操作全量失效 | 无类似机制 | 🟢 **优势** |
| 流式执行 | bash 流式输出 | `StreamingToolExecutor` | 🟢 |
| 并行执行 | 基于 `is_concurrency_safe()` | 基于 `isConcurrencySafe` | 🟢 |

### 6.2 Daedalus 独有优势

`ReadOnlyCache` 缓存 `list_directory`/`search_files`/`get_file_info` 结果：
- 相同参数的只读工具调用直接返回缓存结果
- 任何写工具（`edit_file`/`multi_edit`/`write_file`/`bash`）执行后全量失效
- 缓存命中时追加 `[cached]` 标记，避免 LLM 重复检测误报
- **Claude Code 没有这个机制**，每次都重新执行

---

## 七、优化方案（按 ROI 排序）

### P0 — 实现 `/context` 上下文分析命令 ✅ 已完成

**预期收益**: 🔼🔼🔼（用户体验 + 调试能力的根本提升）
**工作量**: 3-5 天
**类型**: 可观测性

**实现方案**:

```rust
// 新增 CLI 命令: /context
// 文件: src/cli/commands/context.rs

pub struct ContextAnalysis {
    pub categories: Vec<ContextCategory>,     // 分类统计
    pub total_tokens: usize,
    pub max_tokens: usize,                    // context window 大小
    pub usage_percentage: f64,
    pub message_breakdown: MessageBreakdown,  // 消息内分布
    pub suggestions: Vec<Suggestion>,         // 优化建议
}

pub struct ContextCategory {
    pub name: String,          // "System prompt" / "Tools" / "Memory" / "Messages"
    pub tokens: usize,
    pub percentage: f64,
}

pub struct MessageBreakdown {
    pub tool_calls_by_type: HashMap<String, usize>,   // 按工具名统计
    pub tool_results_by_type: HashMap<String, usize>,
    pub user_message_tokens: usize,
    pub assistant_message_tokens: usize,
    pub duplicate_file_reads: Vec<DuplicateRead>,     // 重复读取检测
}
```

**分步实施**:
1. Phase 1: 基础分类统计（system/tools/memory/messages 四大类）
2. Phase 2: 消息内细分（按工具类型统计 token 占比）
3. Phase 3: 优化建议系统（5 类检查）
4. Phase 4: 可视化渲染（网格 or 进度条）

**涉及文件**:
- 新增 `src/cli/commands/context.rs`
- 修改 `src/cli/mod.rs`（注册命令）
- 修改 `src/memory/mod.rs`（暴露 token 统计接口）

---

### P1 — 缓存感知微压缩 ✅ 已完成

**预期收益**: 🔼🔼🔼（长会话成本可降低 30-50%）
**工作量**: 5-7 天
**类型**: 性能核心

**实现方案**:

```
当前: micro_compact() → 修改消息内容 → 缓存失效
目标: micro_compact() 分两条路径:

路径 A (缓存热): 缓存感知模式
  1. 检测缓存是否热（上一轮 cache_read_tokens > 0）
  2. 如果热 → 不修改消息内容
  3. 在 API 调用层注入 cache_edits 指令
  4. 服务端虚拟删除旧工具结果
  5. 缓存前缀保持不变

路径 B (缓存冷): 传统模式（保持现有行为）
  1. 检测缓存是否冷（长时间未活动 或 cache_read_tokens = 0）
  2. 直接修改消息内容（反正缓存已经冷了）
  3. 省去 API 层复杂度
```

**前置条件**: 需要确认当前使用的 LLM API 是否支持 `cache_edits` 特性（Anthropic Claude 3.5+ 支持）。

**涉及文件**:
- 修改 `src/memory/sliding_window/compact_ops.rs`（micro_compact 双路径）
- 修改 `src/llm/adapter/anthropic.rs`（注入 cache_edits）
- 修改 `src/llm/adapter/venus.rs`（同上）
- 修改 `src/llm/types.rs`（新增 CacheEdit 类型）

---

### P2 — 上下文优化建议系统 ✅ 已完成

**预期收益**: 🔼🔼（帮助用户主动优化 token 使用）
**工作量**: 2-3 天
**类型**: 可观测性

**实现方案**:

```rust
pub fn generate_suggestions(analysis: &ContextAnalysis) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. 容量检查
    if analysis.usage_percentage > 80.0 {
        suggestions.push(Suggestion::warning(
            "上下文使用超过 80%，建议运行 /compact 压缩对话历史"
        ));
    }

    // 2. 大工具结果检查
    for (tool, tokens) in &analysis.message_breakdown.tool_results_by_type {
        let pct = *tokens as f64 / analysis.total_tokens as f64 * 100.0;
        if pct > 20.0 {
            suggestions.push(Suggestion::info(
                format!("{} 工具结果占用了 {:.0}% 上下文，考虑使用更精确的查询", tool, pct)
            ));
        }
    }

    // 3. 重复文件读取检查
    for dup in &analysis.message_breakdown.duplicate_file_reads {
        if dup.count > 2 {
            suggestions.push(Suggestion::info(
                format!("{} 被读取了 {} 次，浪费约 {} tokens", dup.path, dup.count, dup.wasted_tokens)
            ));
        }
    }

    // 4. 记忆膨胀检查
    if let Some(mem_cat) = analysis.categories.iter().find(|c| c.name == "Memory") {
        if mem_cat.percentage > 15.0 {
            suggestions.push(Suggestion::info(
                "记忆文件占用超过 15%，考虑清理过期的长期记忆"
            ));
        }
    }

    // 5. 压缩状态检查
    if !analysis.auto_compact_enabled {
        suggestions.push(Suggestion::info(
            "自动压缩未启用，长会话可能因上下文溢出而中断"
        ));
    }

    suggestions
}
```

---

### P3 — Prompt Cache 中断检测 ✅ 已完成

**预期收益**: 🔼🔼（快速发现缓存效率问题）
**工作量**: 1-2 天
**类型**: 性能监控

**实现方案**:

```rust
// 新增 src/llm/cache_monitor.rs

pub struct CacheMonitor {
    /// 上一轮的缓存读取 token 数
    last_cache_read: usize,
    /// 是否有预期的缓存失效（compact/micro_compact 触发后设置）
    expected_invalidation: bool,
}

impl CacheMonitor {
    pub fn record_usage(&mut self, usage: &TokenUsage) {
        let cache_read = usage.cache_read_tokens;

        // 检测异常：上一轮缓存读取很高，这一轮突然为 0
        if self.last_cache_read > 1000 && cache_read == 0 && !self.expected_invalidation {
            tracing::warn!(
                last_cache_read = self.last_cache_read,
                "Prompt cache break detected without expected cause — \
                 check message construction for unintended changes"
            );
        }

        self.last_cache_read = cache_read;
        self.expected_invalidation = false;
    }

    /// 在 compact/micro_compact 后调用
    pub fn notify_expected_invalidation(&mut self) {
        self.expected_invalidation = true;
    }
}
```

**涉及文件**:
- 新增 `src/llm/cache_monitor.rs`
- 修改 `src/agent/tool_loop/mod.rs`（在 LLM 调用后记录 usage）
- 修改 `src/memory/sliding_window/compact_ops.rs`（compact 后通知）

---

### P4 — 多点 cache_control 标记 ✅ 已完成

**预期收益**: 🔼（提升缓存命中率）
**工作量**: 1 天
**类型**: 性能

**当前**: 仅 system message 标记 `cache_control: ephemeral`

**目标**: 额外标记以下位置：
1. **最后一个已截断的工具轮次**（内容已稳定，不会再变）
2. **长期记忆段**（跨 turn 不变的部分）
3. **tool definitions 段**（除非加载了新 MCP 工具，否则不变）

```rust
// 在 build_messages() 中标记多个缓存断点
fn mark_cache_breakpoints(messages: &mut Vec<ChatMessage>) {
    // 1. System message（已有）
    // 2. 找到最后一个被截断的工具轮次
    if let Some(idx) = find_last_truncated_round(messages) {
        messages[idx].cache_control = Some(CacheControl::Ephemeral);
    }
    // 3. 长期记忆注入点（如果 token 数 > 阈值）
    if let Some(idx) = find_long_term_memory_message(messages) {
        if estimate_tokens(&messages[idx].content) > 500 {
            messages[idx].cache_control = Some(CacheControl::Ephemeral);
        }
    }
}
```

---

### P5 — 团队记忆同步（远期，按需实施）

**预期收益**: 🔼（团队场景需求）
**工作量**: 5+ 天
**类型**: 产品功能

Claude Code 的 TeamMem 实现要点：
- API 端点 + ETag 乐观锁实现双向同步
- 推送：增量上传（仅 hash 不同的条目）
- 拉取：服务器内容覆盖本地
- 秘密扫描：上传前用 Gitleaks 规则扫描敏感信息
- 冲突处理：ETag 不匹配时刷新 hash → 重算增量 → 重试（最多 2 次）

**评估**: 仅在团队使用场景下有价值，可延后实施。

---

## 八、Daedalus 独有优势（无需改动）

以下是 Daedalus 已经超越 Claude Code 的领域，应该继续保持：

| 领域 | Daedalus 优势 | Claude Code 现状 |
|------|--------------|-----------------|
| **记忆策略丰富度** | 6 种策略（含知识图谱、空间记忆等） | 3 种基于文件的模式 |
| **记忆合并去重** | `maybe_consolidate()` LLM 自动合并 | 无，依赖文件级手动管理 |
| **只读工具缓存** | `ReadOnlyCache` + 写操作失效 | 无类似机制 |
| **上下文压力三级感知** | Normal/Warning/Critical + budget hint 注入 | 仅 autocompact 阈值 |
| **工具历史渐进截断** | `truncate_tool_history()` 按轮次渐进 | 统一 microCompact |

---

## 九、实施路线图

```
Phase 1: 可观测性基础 ✅ 已完成 (2026-05-16)
├── P0: /context 命令 — 基础分类统计 + 消息细分             ✅
├── P3: Cache 中断检测                                      ✅
└── P4: 多点 cache_control 标记                             ✅

Phase 2: 性能核心 ✅ 已完成 (2026-05-16)
├── P1: 缓存感知微压缩                                      ✅
└── P2: 上下文优化建议系统                                   ✅

Phase 3: 产品功能 — 按需实施
└── P5: 团队记忆同步                                         ⏳ 等待团队使用需求
```

**关键度量指标**:
- 长会话（20+ 轮）的 cache_read_tokens / total_input_tokens 比率
- compact 触发频率
- 用户主动 /compact 的频率（越低越好 = 自动管理越好）
- 同一文件重复读取次数

---

*变更历史*

| 日期 | 版本 | 变更 | 来源 |
|------|------|------|------|
| 2026-05-16 | v1.0 | 初版：6 大领域对比分析 + 6 项优化方案 + 实施路线图 | Claude Code 源码对比 + Daedalus 全量代码审查 |
| 2026-05-16 | v1.1 | Phase 1+2 全部完成：`/context` 命令 + CacheMonitor + 多点 cache_control + 缓存感知 micro_compact + 压力告警。P0-P4 标记 ✅ | 代码实现 |
