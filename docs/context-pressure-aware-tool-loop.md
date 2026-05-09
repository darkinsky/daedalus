# Context Pressure Aware Tool Loop — 调研与设计文档

> 日期: 2026-05-09  
> 状态: 实施中  
> 作者: Daedalus Team

## 1. 问题描述

在复杂任务场景下，agent 在单个 turn 内不断调用工具（读取文件、搜索代码等），导致：

1. 上下文窗口快速填满
2. 触发对之前 turn 的压缩（sliding window compact）
3. 之前 turn 已完全压缩后，被迫压缩当前 turn 的工具调用结果
4. 关键工具调用结果丢失，任务质量下降

这是一个典型的 **"context rot in long tool loops"** 问题。

## 2. 业界调研

### 2.1 Claude Code 的五级压缩流水线

Claude Code（源码泄露版 v2.1.88）实现了 5 层递进式压缩策略：

| 层级 | 名称 | 文件 | 代价 |
|:----:|------|------|------|
| 1 | Time-based MicroCompact | microCompact.ts | 极低（无 AI 调用） |
| 2 | Cache-based MicroCompact | microCompact.ts | 极低（无 AI 调用） |
| 3 | SessionMemory Compact | compact.ts | 中（AI 摘要） |
| 4 | Full Compact | compact.ts | 高（全量 AI 摘要） |
| 5 | Hard Reset | - | 极高（丢失所有上下文） |

**关键参数**：
- `AUTOCOMPACT_BUFFER_TOKENS = 13,000`：当 `contextUsage >= (effectiveWindow - 13,000)` 时强制触发
- 上下文使用率阈值建议：
  - 0-50%: 充裕，自由工作
  - 50-70%: 注意，准备压缩
  - 70-90%: 警告，立即 /compact
  - 90%+: 危险，必须 /clear

### 2.2 Context Offloading（上下文卸载）

来源：2025 年 AI Agent 工程实践论文

核心思想：将信息存在语言模型的"活跃上下文窗口"之外，通过外部工具或记忆系统单独保存数据，模型需要时再去访问。

**为什么有效**：
- 研究表明，重要信息埋得太深时，模型使用的准确性会下降（"上下文腐烂"）
- 百万 token 上下文并不意味着可以把所有东西都塞进去
- Agent 会随时间积累复杂的长上下文，失效会累积

### 2.3 Cursor 的动态上下文发现

Cursor 推出的 Dynamic Context Discovery 方法：
- 摒弃在请求开始时就包含大量静态上下文的做法
- 让 agent 按需动态检索所需信息
- 显著减少 token 消耗

### 2.4 Nvidia Nemotron 的 Thinking Budget

Nvidia Nemotron-Nano-9B-v2 引入了 runtime "thinking budget" management：
- 允许开发者限制模型的推理 token 数量
- 通过 `/think` 或 `/no_think` 控制 token 来管理

### 2.5 Apple 的 Context Window 管理

Apple SystemLanguageModel 新增：
- `contextSize` 属性：返回可用上下文容量
- `tokenCount(for:)` 方法：计算指定输入所消耗的 Token 数量
- 避免硬编码上限，提供动态调整能力

## 3. 解决方案设计

### 3.1 方案一：Context Pressure Aware System Prompt Injection ⭐⭐⭐⭐⭐

**核心思路**：在 tool loop 的每一轮 LLM 调用前，根据当前上下文使用百分比，动态注入"上下文预算提醒"到 tool history 中，引导模型尽快收敛。

**阈值设计**：
- 60-70%: 注入 soft notice（建议减少工具调用）
- 70-80%: 注入 warning（必须尽快结束）
- 80-90%: 注入 hard warning（立即总结）
- 90%+: 强制终止 tool loop

**优点**：实现简单，利用模型自身理解力，渐进式引导  
**缺点**：模型可能不完全遵守，注入本身占用 token

### 3.2 方案二：Tool Loop Context Budget Gate ⭐⭐⭐⭐

**核心思路**：在 `run_tool_loop` 中增加硬性上下文预算检查，超过阈值时强制终止并要求模型给出最终答案。

**优点**：硬性保证不溢出，可在终止前做总结性 LLM 调用  
**缺点**：可能在任务未完成时被强制中断

### 3.3 方案三：Context Offloading + Scratchpad ⭐⭐⭐⭐⭐

**核心思路**：将工具调用的详细结果存储到外部"暂存区"，上下文中只保留摘要引用。

**优点**：从根本上解决膨胀问题，不丢失信息  
**缺点**：实现复杂度高，回查增加额外轮次

### 3.4 方案四：Intra-Turn MicroCompact ⭐⭐⭐⭐

**核心思路**：在 tool loop 运行过程中，对较早轮次的工具结果进行实时截断/摘要。根据上下文压力动态调整截断激进程度。

**优点**：与现有 TruncationConfig 架构完美契合，不需要额外 LLM 调用  
**缺点**：截断可能丢失关键信息

### 3.5 方案五：Hierarchical Sub-task Decomposition ⭐⭐⭐

**核心思路**：检测到上下文压力时，自动将剩余任务委托给 subagent（独立上下文）。

**优点**：理论上可处理无限复杂任务  
**缺点**：实现复杂，信息传递有损耗

## 4. 推荐实施方案

采用 **方案一 + 方案二 + 方案四** 的组合：

```
Tool Loop 开始
    │
    ▼
每轮检查上下文使用率
    │
    ├── < 60%  → 正常执行
    ├── 60-70% → 方案四: 动态加强截断
    ├── 70-80% → 方案一: 注入 soft warning
    ├── 80-90% → 方案一: 注入 hard warning
    └── > 90%  → 方案二: 强制终止 + 最后一次总结调用
```

### 4.1 实施优先级

| 优先级 | 方案 | 预期收益 | 实现难度 | 兼容性 |
|:------:|------|:--------:|:--------:|:------:|
| P0 | 方案一（Prompt Injection） | 高 | 极低 | 极高 |
| P0 | 方案二（Budget Gate） | 极高 | 低 | 极高 |
| P1 | 方案四（Intra-Turn MicroCompact） | 中 | 中 | 高 |
| P2 | 方案三（Context Offloading） | 极高 | 高 | 中 |
| P3 | 方案五（Sub-task Decomposition） | 中 | 很高 | 中 |

## 5. 实现细节

### 5.1 LoopConfig 新增字段

```rust
pub struct LoopConfig {
    // ... existing fields ...
    
    /// Context window size (in tokens) for budget-aware behavior.
    /// When set, enables context pressure hints and hard-stop gate.
    pub context_window_tokens: Option<usize>,
    
    /// Ratio at which to start injecting "wrap up" hints (default: 0.7).
    pub context_soft_limit_ratio: f64,
    
    /// Ratio at which to force-stop the loop (default: 0.9).
    pub context_hard_limit_ratio: f64,
}
```

### 5.2 上下文使用率估算

```rust
fn estimate_context_usage_pct(
    messages: &[ChatMessage],
    tool_history: &[ToolRound],
    context_window: usize,
) -> u8 {
    let msg_chars: usize = messages.iter().map(|m| m.content_len()).sum();
    let history_chars = estimate_history_chars(tool_history);
    let total_tokens = (msg_chars + history_chars) / CHARS_PER_TOKEN;
    let pct = (total_tokens * 100) / context_window;
    pct.min(100) as u8
}
```

### 5.3 Context Budget Hint 生成

```rust
fn context_budget_hint(usage_pct: u8) -> Option<String> {
    match usage_pct {
        70..=79 => Some("[CONTEXT NOTICE] ~70% context used. Prefer summarizing over reading more files."),
        80..=89 => Some("[CONTEXT WARNING] ~80% context used. Conclude your work NOW."),
        90..=100 => Some("[CONTEXT CRITICAL] >90% context used. STOP tool calls. Answer immediately."),
        _ => None,
    }
}
```

### 5.4 动态截断加强

当上下文使用率 > 60% 时，动态缩小 TruncationConfig 的 budget_tokens：

```rust
let effective_budget = if usage_pct > 60 {
    let pressure = (usage_pct - 60) as f64 / 40.0; // 0.0 ~ 1.0
    let reduction = (cfg.budget_tokens as f64 * pressure * 0.5) as usize;
    cfg.budget_tokens.saturating_sub(reduction)
} else {
    cfg.budget_tokens
};
```

## 6. 关键设计原则

1. **Claude Code 哲学**："能不调 LLM 就不调 LLM" — 优先用规则截断，其次才用 AI 摘要
2. **渐进式降级**：不要突然中断，而是逐步收紧（预警 → 限制 → 强制）
3. **信息不丢失原则**：即使截断了工具结果，也要保留 tool_call 的函数名和参数
4. **Cache 感知**：计算上下文压力时考虑 prompt cache 的折扣效应
5. **当前 turn 优先**：压缩策略优先保护当前 turn 的最近几轮工具结果

## 7. 参考资料

- Claude Code 源码 v2.1.88（泄露版）: `src/services/compact/compact.ts`, `microCompact.ts`
- Context Offloading 论文 (2025): 用 LangGraph 实现端到端上下文卸载
- Cursor Dynamic Context Discovery (2026-01)
- Apple SystemLanguageModel contextSize API (2026-03)
- Nvidia Nemotron thinking budget management (2025-08)
