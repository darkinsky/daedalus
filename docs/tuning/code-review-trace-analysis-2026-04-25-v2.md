
# 代码审查任务 — Trace 分析与优化报告（第二轮）

> **日期**: 2026-04-25
> **Trace ID**: `85d3d3f6-adcc-4962-900c-bc9feda1348a`
> **任务**: "给当前项目做代码审查"
> **模型**: claude-sonnet-4-6（通过 Venus）
> **Trace 文件**: `.daedalus/traces/2026-04-25.yaml`
> **背景**: 上一轮分析（v1）后已修复 `maxTurns: 50 → 100`、实现 `force_final_summary` 优雅降级、code-reviewer 添加 `bash` 工具。本轮验证修复效果并发现新问题。

---

## 1. 执行摘要

| 指标 | 数值 | 对比 v1 |
|------|------|---------|
| 总耗时 | **877s**（约 14.6 分钟）| ↑ 66%（v1 528s）|
| 总 Token 数 | **6,205,285** | ↑ 65%（v1 3,771,879）|
| 主 agent LLM 调用次数 | **3** | ↓（v1 约 65）|
| subagent 总 LLM 调用次数 | ~**132** | ↑（v1 约 50）|
| 主代理重试轮次 | **0** | ✅ 已消除（v1 61 轮）|

### 执行流程

```
主代理（3 轮 LLM 调用）
  ├─ Round 1: spawn_subagent("explore", "全面代码探索和分析")
  │    └─ explore 子代理（30 轮，1,829,412 tokens，248s）
  │         └─ 正常完成（30 = maxTurns 上限）
  ├─ Round 2: spawn_subagent("code-reviewer", "深度代码审查")
  │    └─ code-reviewer 子代理（100 轮，4,337,982 tokens，537s）
  │         └─ 触顶 maxTurns=100 → force_final_summary 生效
  └─ Round 3: 主代理基于两个子代理结果整合输出最终审查报告（20,456 tokens）
```

---

## 2. v1 修复效果评估

### ✅ P0 已修复 — `force_final_summary` 优雅降级生效

**v1 问题**：子代理达到 maxTurns 后返回纯错误字符串，主代理被迫重做全部工作。

**v2 表现**：code-reviewer 达到 100 轮后，`force_final_summary` 成功触发：
- 遍历 100 轮 tool_history，每个结果取前 300 字节构建 work_summary
- 发起一次无工具 LLM 调用，要求模型基于摘要输出完整发现
- 返回内容：`"## Review Summary ... Scope: Daedalus — a Rust-based autonomous AI agent framework (~21,793 lines of source)..."`
- **主代理不再重试**，直接基于返回结果整合报告

**残余问题**：300 字节/轮的截断窗口过小，总结质量受限。详见问题 4。

### ✅ P0 已修复 — "循环犹豫"反模式消除

v1 中主代理反复"准备输出但又去读文件"的死循环不再出现。原因是子代理现在总能返回有用内容，主代理无需自己重做。

### ✅ P3 已修复 — code-reviewer 已有 `bash` 工具

工具列表确认包含 `bash`（trace 第 5394 行），实际使用中 code-reviewer 前 10 轮确实用了 `[bash, list_directory]`、`[bash, bash]` 等。

---

## 3. 新发现的问题

### 🔴 P0 — 上下文压缩算法过于激进，严重浪费 256K 上下文窗口

**核心事实**：

当前截断算法参数（`tool_loop.rs:52-63`）：

```rust
const FULL_RESULT_RECENT_ROUNDS: usize = 3;          // 最近 3 轮完整保留
const TRUNCATED_RESULT_MAX_CHARS: usize = 500;         // 倒数 4-6 轮截断到 500 字符
const MICRO_TRUNCATED_RESULT_MAX_CHARS: usize = 120;   // 更老的轮截断到 120 字符
```

**观察到的 prompt_tokens 变化**（code-reviewer 子代理）：

```
第  1 轮:   6,519   ▏
第  4 轮:  41,365   ████████████
第  7 轮:  53,317   ███████████████▌    ← 峰值
第 10 轮:  22,894   ██████▊             ← 截断生效，急剧下降
...后续 90 轮在 33K–66K 之间振荡，中位数约 43K
```

**问题**：

1. **上下文利用率极低**：模型上下文窗口 256K tokens，实际只用了 **15–25%**（40-66K）。最佳工作窗口应为 200K 以内（约 78%），当前利用率仅为目标的 1/4。

2. **120 字符的 micro-truncation 近似丢弃**：`read_file` 返回的源代码通常 5,000–20,000 字符。截断到 120 字符后，仅剩文件头几行或错误信息。code-reviewer 在第 7 轮读过的文件，到第 10 轮已经只剩 120 字符——**几乎记不住任何之前读过的代码**。

3. **直接后果**：
   - 模型每次只能"看到"最近 3 轮读到的文件完整内容
   - 代码审查需要交叉引用多文件，但窗口只有 3 轮
   - 模型必须反复重新读取同一个文件 → 解释了 100 轮全在做 `read_file`

4. **对比**：Claude Code 的 microcompact 策略是在**接近上下文限制时**才触发激进截断，而非从第 7 轮就开始。我们的实现缺少对模型上下文大小的感知。

### 🔴 P0 — 主代理串行派生 explore → code-reviewer，导致双重读取

**发生了什么**：

1. 主代理收到"给当前项目做代码审查"
2. Round 1：派生 `explore` 子代理，花 30 轮读取所有 170 个源文件
3. Round 2：派生 `code-reviewer` 子代理，又花 100 轮**重新读取**所有源文件

**为什么是浪费**：

- 子代理上下文隔离：code-reviewer **看不到** explore 读过的任何文件内容
- 主代理虽然把 explore 的分析摘要塞进了 code-reviewer 的 task 描述（约 3,278 字符），但这只是文字描述，不是原始代码
- code-reviewer 必须自己重新读取所有文件才能做审查
- explore 的 30 轮（1.83M tokens，248s）**完全浪费**

**Token 浪费量化**：

| 阶段 | Token 消耗 | 是否可避免 |
|------|-----------|-----------|
| explore 子代理 | 1,829,412 | ⚠️ 完全可避免 |
| code-reviewer 子代理 | 4,337,982 | 必需（但可优化） |
| 主代理整合 | 37,891 | 必需 |
| **总浪费** | **~1.83M（29%）** | |

### 🔴 P0 — code-reviewer 工具并行度从前期正常退化为后期单调用

**数据**：

| 轮次范围 | 典型 tool_calls | 并行度 |
|----------|----------------|--------|
| 第 1-5 轮 | `[list_directory, bash]`、`[read_file × 3]` | 2-3 |
| 第 6-10 轮 | `[read_file, read_file]` | 2 |
| 第 11-25 轮 | `[read_file, read_file]` | 2 |
| 第 26-100 轮 | `[read_file]` | **1** |

**对比 explore 子代理**（同模型、同提示词风格）：

| 轮次范围 | 典型 tool_calls | 并行度 |
|----------|----------------|--------|
| 第 1-3 轮 | `[list_directory, read_file]` | 2 |
| 第 4 轮 | `[read_file × 10]` | **10** |
| 第 5 轮 | `[read_file × 7]` | **7** |
| 后续轮 | `[read_file × 2-5]` | 2-5 |

**根因**：与上下文压缩过度直接相关。explore 子代理只跑 30 轮，上下文还能容纳较多历史；code-reviewer 跑到后期时，micro-truncation 把大量历史压缩到 120 字符，模型失去了对全局进度的把控：

- 看不到之前读过哪些文件 → 无法制定并行读取计划
- 每轮只能"看到" 3 轮历史 → 保守策略：一次只读一个文件
- 信息密度极低 → 模型用 completion tokens "思考"如何规划的开销变高，但可用信息太少

### 🟠 P1 — `force_final_summary` 中的 300 字节截断过小

**位置**：`runner.rs:388`

```rust
let preview = if resp.content.len() > 300 {
    let safe = crate::tools::truncate_at_char_boundary(&resp.content, 300);
    format!("{}...", safe)
} else {
    resp.content.clone()
};
```

**问题**：100 轮 × 每轮 1-2 个工具调用 × 300 字节 ≈ 30-60KB 的 work_summary。对于 256K 上下文模型来说太保守了。可以安全地提高到 1000-2000 字节。

### 🟠 P1 — `force_final_summary` 没有传入实际的文件内容

`force_final_summary` 只看到了 `Tool: read_file → [前300字节]...` 的碎片化摘要，而非文件的完整内容。对于代码审查，这意味着最终总结调用在没有看到实际代码的情况下"编造"审查发现。

---

## 4. 问题间的因果关系

```
上下文压缩过于激进（P0-截断算法）
  ├→ 子代理每轮只能记住 3 轮内容
  │   ├→ 模型无法做多文件交叉引用
  │   └→ 退化为单文件逐个读取（P0-并行度退化）
  ├→ 100 轮仍读不完所有需要审查的文件
  │   └→ 触顶后 force_final_summary 只有碎片化信息（P1-300字节截断）
  └→ 主代理"感觉"需要先 explore 再 review
      └→ 串行派生两个子代理，explore 完全浪费（P0-双重读取）
```

**结论**：截断算法是所有问题的根因。修复它将级联解决其余问题。

---

## 5. 优化建议

### 5.1 上下文截断算法感知模型窗口大小（P0，工作量：⭐⭐⭐）

**文件**：`src/agent/tool_loop.rs`

**当前**：硬编码常量，不感知模型上下文大小。

**建议**：

```rust
/// 截断配置，可根据模型上下文窗口动态调整
pub struct TruncationConfig {
    /// 最近 N 轮完整保留
    pub full_recent_rounds: usize,
    /// 中期轮次的截断字符数
    pub moderate_truncation_chars: usize,
    /// 远期轮次的截断字符数
    pub aggressive_truncation_chars: usize,
    /// 目标总历史 token 上限（不超过此值才触发截断）
    pub target_history_budget: usize,
}

impl TruncationConfig {
    /// 根据模型上下文窗口大小生成合理的截断配置
    pub fn for_context_window(context_window: usize) -> Self {
        // 目标：使用上下文窗口的 60% 给历史
        let budget = context_window * 60 / 100;
        Self {
            full_recent_rounds: if budget > 100_000 { 10 } else { 3 },
            moderate_truncation_chars: if budget > 100_000 { 3000 } else { 500 },
            aggressive_truncation_chars: if budget > 100_000 { 800 } else { 120 },
            target_history_budget: budget,
        }
       }
}
```

**关键改动**：
- `LoopConfig` 增加 `context_budget: usize` 字段
- `truncate_tool_history()` 改为先估算当前历史总 token，只有**超过预算时**才触发截断
- 对 256K 模型，约 153K tokens 的历史预算可容纳 ~50 个完整文件读取结果，足够做深度代码审查

**预期效果**：prompt_tokens 从 40-55K 提升到 120-150K，模型可记住更多文件，并行度和审查质量同步提升。

### 5.2 消除不必要的 explore 前置步骤（P0，工作量：⭐）

**方案 A**：在主代理 prompt 中添加策略指导：

```markdown
## 子代理使用策略

- 代码审查任务：直接使用 code-reviewer 子代理，不需要先用 explore 探索。
  code-reviewer 自身有 list_directory、grep_search、bash 等工具，
  可以自行完成项目结构探索。
- 只有当任务明确需要"先了解再做"的两步流程时，才串行使用多个子代理。
```

**方案 B**：在 `spawn_subagent` 工具描述中添加：

```
⚠️ 子代理间上下文完全隔离。串行派生多个子代理时，后者看不到前者读过的任何文件。
避免用 explore 预读后再用 code-reviewer 审查 — 这会导致所有文件被读取两次。
```

**预期效果**：省去 explore 的 1.83M tokens（29% 总成本），总耗时减少约 248s。

### 5.3 提高 `force_final_summary` 中的截断阈值（P1，工作量：⭐）

**文件**：`src/subagent/runner.rs:388`

```rust
// 修改前
let preview = if resp.content.len() > 300 {

// 修改后
let preview = if resp.content.len() > 1500 {
```

同时可考虑对 work_summary 设置总预算上限（如 200K 字符），而非对每条结果固定截断。

### 5.4 在 code-reviewer prompt 中显式告知上下文预算（P1，工作量：⭐）

**文件**：`.daedalus/agents/code-reviewer.md`，添加到"Working Principles"：

```markdown
### 上下文管理

- 你的上下文窗口足够容纳大量文件内容，不必过于保守。
- **大胆并行**：每轮可以同时读取 5-10 个文件，不会溢出。
- 在 Phase 2（Scan）阶段用 grep_search 快速定位问题区域，
  Phase 3（Deep Read）阶段再并行读取目标文件。
- 避免逐个文件顺序读取 — 这会浪费大量轮次。
```

---

## 6. 成本分析

### 当前成本（Claude Sonnet 4 定价：$3/M 输入，$15/M 输出）

| 阶段 | Prompt Tokens | Completion Tokens | 估算成本 |
|------|:---:|:---:|:---:|
| 主代理（3 轮）| ~32K | ~5.5K | ~$0.18 |
| explore 子代理（30 轮）| 1,829K | ~8K | ~$5.6 |
| code-reviewer 子代理（100 轮）| 4,338K | ~9K | ~$13.2 |
| **合计** | **6,182K** | **~23K** | **~$19.0** |

### 优化后预估成本

| 场景 | 预估成本 | 节省 |
|------|:---:|:---:|
| 去掉 explore（只派 code-reviewer） | ~$13.2 | **30%** |
| + 上下文截断优化（减少重复读取） | ~$7-8 | **58-63%** |
| + prompt 引导并行（50 轮内完成） | ~$4-5 | **74-79%** |

---

## 7. 实施检查清单

- [ ] **P0**: `tool_loop.rs` — 截断算法增加模型上下文窗口感知，动态调整截断参数
- [ ] **P0**: 主代理 prompt — 添加"代码审查直接用 code-reviewer，不需先 explore"的策略
- [ ] **P0**: `spawn_subagent` 工具描述 — 添加子代理间上下文隔离警告
- [ ] **P1**: `runner.rs` — `force_final_summary` 截断阈值从 300 提高到 1500
- [ ] **P1**: `code-reviewer.md` — 添加上下文管理指南，鼓励大胆并行
- [ ] **P2**: 评估是否需要 `TruncationConfig` 作为 `LoopConfig` 的一部分传入
- [ ] **P2**: 验证 prompt 缓存在子代理 100 轮调用中的命中率

---

## 8. 附录

### A. Prompt Token 变化趋势

#### explore 子代理（30 轮）

```
第  1 轮:   2,408  ▏
第  3 轮:  20,313  █████▏
第  4 轮:  77,421  ████████████████████▏       ← 10 个并行 read_file
第  5 轮: 106,594  ████████████████████████████▏← 峰值
第  6 轮: 137,686  ████████████████████████████████████▌  ← 最大值，截断触发
第  7 轮:  87,793  ██████████████████████▊     ← 截断生效
...后续在 39K–78K 振荡
```

#### code-reviewer 子代理（100 轮）

```
第  1 轮:   6,519  █▋
第  3 轮:  26,946  ██████▊
第  5 轮:  48,582  ████████████▍
第  7 轮:  53,317  █████████████▋     ← 峰值
第 10 轮:  22,894  █████▉             ← 截断生效
第 15 轮:  65,266  █████████████████▎
第 20 轮:  35,231  █████████▏
...后续 80 轮在 33K–56K 振荡，中位数约 43K
```

### B. 工具并行度对比

| 子代理 | 总轮次 | 平均并行度 | 最大并行度 | 单调用轮占比 |
|--------|--------|-----------|-----------|------------|
| explore | 30 | **3.6** | 10 | 3% |
| code-reviewer | 100 | **1.3** | 3 | **75%** |

### C. 与 v1 的关键指标对比

| 指标 | v1 | v2 | 变化 |
|------|----|----|------|
| 总 Token | 3.77M | 6.21M | ↑ 65% |
| 总耗时 | 528s | 877s | ↑ 66% |
| 主代理重试轮次 | 61 | **0** | ✅ 已消除 |
| 子代理能否返回有用结果 | ❌ 纯错误 | ✅ 总结报告 | ✅ 已修复 |
| 上下文利用率 | ~18% | ~18% | ⚠️ 未改善 |
| 工具并行度 | 未记录 | 1.3（CR） | ⚠️ 过低 |
| 是否存在双重读取浪费 | ❌ | ⚠️ 是（+explore） | 新问题 |
