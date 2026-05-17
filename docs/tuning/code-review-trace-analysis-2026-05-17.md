# Code Review Trace 分析 — 多子代理编排（2026-05-17）

## 1. 运行概览

| 指标 | 值 |
|------|-----|
| Trace ID | `d956c355-f423-4986-95ee-ace98e809649` |
| 总耗时 | 786s（~13 min） |
| 总 token | 5,618,818 |
| 缓存 token | 4,570,752（81.4%） |
| 模型 | deepseek-v4-pro |
| 子代理数量 | 5（并行 code-reviewer） |
| 编排器 LLM 调用 | 4 轮（探索 2 + dispatch 1 + 综合 1） |
| 报告质量 | B+（20/20 发现定位准确，4 项严重程度夸大） |

### 1.1 子代理资源分布

| 子代理 | 审查范围 | 轮次 | Token | 每行 Token | 耗时 |
|--------|---------|------|-------|-----------|------|
| llm+cli | 30 文件, 8.8k 行 | 12 | 780k | 89 | 314s |
| prompt+tracing+其他 | 52 文件, 8.8k 行 | 13 | 865k | 98 | 403s |
| agent+acp+subagent+mcp | 33 文件, 13.3k 行 | 15 | 1,590k | 120 | 406s |
| tools+middleware | 35 文件, 9.2k 行 | 13 | 985k | 107 | 412s |
| memory | 67 文件, 16.5k 行 | 20 | 1,312k | 79 | 491s |

## 2. 发现的问题

### 2.1 🔴 编排器跳过了综合验证步骤

**现象**：编排器收到 5 个子代理结果后，最终 LLM 调用（第 4723 行）直接基于子代理文本输出生成报告，**未调用任何 read_file 或 grep_search 验证**。

**系统提示已明确要求**：
```
SYNTHESIS & VALIDATION:
2. VERIFY: For any finding with severity >= HIGH AND confidence < [HIGH],
   you MUST read the cited file:line yourself and confirm the issue exists.
```

**影响**：
- 3 项严重程度被夸大（`bash.rs`/`grep_search.rs` 的 `from_utf8_lossy` 对 `&[u8]` 操作不会 panic，被错误报告为 "运行时 Panic"）
- 2 项缺少上下文（`std::process::exit()` 之前已有 `shutdown().await`；ACP cancel 有 "Phase 2" 注释）
- 1 项描述有误（ChromaDB "永不重试" — 实际 `chroma_initialized` 为 false 时下次调用会重试）

**根因**：综合步骤的验证指令是建议性的（SHOULD 语义），模型在 token 预算紧张时选择跳过。

### 2.2 🔴 子代理探索策略：全文通读而非模式扫描

**现象**：以 llm+cli 子代理为例，12 轮工具调用中 10 轮是 `read_file`（逐文件通读），0 次 `grep_search`。

**影响**：
- 通读策略在 token 预算紧张时无法覆盖所有文件
- 早期读取的文件内容随上下文膨胀可能被截断，导致最终报告对早期文件记忆模糊
- 相同类别问题（如全项目的 `unwrap()`/`expect()` 模式）分散在多个子代理中，难以系统性聚合

### 2.3 🟠 `take_note` 使用率极低

**现象**：5 个子代理中仅 memory 子代理使用了 `take_note`（8 条笔记），其他 4 个为零。

**影响**：
- 发现结果全部依赖最终一次性输出，无法对抗上下文截断
- 对应 v6 分析（2026-04-25）发现的同类问题：`take_note` 不在工具列表中导致不可用

**本次确认**：v6 修复后 `take_note` 已加入工具列表（trace 中可见 `take_note` 在 available_tools 中），但 4/5 子代理仍主动选择不使用。

### 2.4 🟠 编排器未检测上下文中的重复执行

**现象**：trace 消息历史中 `[2]`/`[3]` 是前一次完整运行的结果（229 个文件，55,889 行），`[4]` 是用户的第二次请求。编排器未识别出上下文中已有完整报告，重新执行了所有 5 个子代理。

**影响**：浪费约 5.5M tokens（完全重复的工作），且两次运行结果混合导致数字不一致（摘要说 56 项，表格加起来 66 项）。

### 2.5 🟡 子代理负载不均衡

**现象**：agent+acp+subagent+mcp 子代理消耗 1.59M tokens，是 llm+cli 的 2 倍，但实际仅多 3 个文件和 4.5k 行代码。

**根因**：agent 模块文件平均行数高（~494 行/文件 vs llm+cli 的 ~293 行/文件），且子代理采用通读策略，大文件消耗大量上下文。

### 2.6 🟡 跨子代理边界信息缺失

**现象**：编排器 dispatch 时未提供 `<cross_module_context>` 信息。例如 `embedding/mod.rs` 的 `cosine_similarity` 使用 `assert_eq!` panic，但审查 memory 模块的子代理无法知道这个上游依赖的行为。

## 3. 通用化改进方向

### 3.1 编排器综合验证约束化

**问题模式**：编排器在综合阶段跳过验证直接输出。

**改进**：将验证从建议升级为强制约束。编排器综合时必须执行工具调用验证，而非纯文本推理。

两种实现路径：

**A. 提示层约束**（低成本，推荐先行）：
在编排器综合指令中增加硬约束：
```
MANDATORY VERIFICATION (cannot be skipped):
After receiving all subagent results, you MUST:
1. Select the top N findings by severity (N = min(10, critical_count + high_count))
2. For EACH selected finding, call read_file on the cited file:line
3. In your report, annotate each verified finding with [VERIFIED] or [UNVERIFIED]
4. REMOVE or DOWNGRADE any finding that doesn't match the actual code
```

**B. 代码层约束**（高可靠，长期方案）：
在编排器综合轮次的代码逻辑中检测：若上一步有子代理返回、但本轮未调用任何 read_file/grep_search，注入强制提示："You have not verified any findings. Please read at least the top 5 critical findings before finalizing the report."

### 3.2 子代理探索策略：扫描优先于通读

**问题模式**：子代理逐文件通读，token 效率低，覆盖面有限。

**改进**：在子代理 prompt 中要求分层探索——先广度扫描建立全局视图，再对可疑点深入。

核心原则：
1. **先结构后内容**：第一步始终是获取文件树和行数统计，而非直接读文件
2. **grep 先于 read**：用 `grep_search` 在全范围内搜索可疑模式（具体模式由子代理根据语言和审查目标自行决定），再对命中位置用 `read_file(offset, limit)` 查看上下文
3. **大文件分段读取**：对超过 300 行的文件，始终使用 offset+limit 读取目标区域，而非通读全文
4. **发现即记录**：每发现一个 MEDIUM 及以上的问题，立即 `take_note`，不要等到最后一次性输出

**反模式**：`read_file(整个文件)` × N → 凭记忆写报告。这种策略在上下文膨胀后会丢失早期文件内容。

**预期效果**：
- grep 扫描可在 2-3 轮内覆盖所有文件的关键反模式
- 仅深入可疑位置，减少不必要的全文读取
- 总 token 消耗预期下降 40-60%

### 3.3 `take_note` 强制使用策略

**问题模式**：子代理有 `take_note` 可用但主动选择不使用，导致长任务中发现丢失。

**改进**：

**A. 提示层强化**：
```
CRITICAL RULE: You MUST call take_note after EVERY finding with severity >= MEDIUM.
Notes survive context truncation — without them, your early findings will be lost
when the context window fills up. A review with 0 take_note calls is INVALID.
```

**B. 代码层检测**：
子代理完成时，检查 `take_note` 调用次数。若为 0 且轮次 > 5，在返回给编排器的结果中附加警告：`"⚠️ This subagent made 0 take_note calls — findings may be incomplete due to context truncation."`

### 3.4 编排器重复执行检测

**问题模式**：用户在同一会话中重复请求相同任务，编排器不识别已有结果。

**改进**：编排器在收到任务后，先检查对话历史中是否已有同类结果。如果有：

```
DUPLICATE DETECTION:
Before dispatching subagents, check if the conversation history already contains
a completed report for the same scope. If found:
1. Summarize what already exists
2. Ask the user: "A previous review already exists. Would you like me to:
   (a) Update it with fresh analysis  (b) Review a different scope  (c) Redo from scratch"
3. Only proceed with full re-execution if the user explicitly chooses (c)
```

这需要在编排器的第一轮推理中增加历史检测逻辑。

### 3.5 跨子代理边界信息传递

**问题模式**：并行子代理完全隔离，无法发现跨模块问题。

**改进**：编排器 dispatch 时为每个子代理注入 `<cross_module_context>`，描述与其审查范围相关的已知跨模块接口。

实现方式：
1. 编排器在探索阶段额外提取模块间依赖关系（如通过 import/use 语句的 grep）
2. 为每个子代理的 task 描述追加已知的跨模块接口摘要，使子代理能检查接口两侧的一致性

**注意**：这会增加编排器的探索成本（额外 1-2 轮），需权衡收益。建议仅在项目 > 100 文件时启用。

### 3.6 子代理负载均衡优化

**问题模式**：按模块分区导致负载不均，大文件模块消耗过多 token。

**改进**：分区时除了文件数和总行数，还需考虑**平均文件大小**：

```
PARTITION BALANCE — Enhanced:
- Primary metric: total LOC per partition (target ±20%)
- Secondary metric: max single-file LOC (if any file > 500 LOC, it dominates
  the subagent's context — consider splitting that module further)
- If avg_lines_per_file > 400, reduce max_rounds proportionally
  (large files fill context faster)
```

### 3.7 严重程度校准：要求检查缓解措施

**问题模式**：子代理发现代码模式后直接按最坏情况定级，忽略已有的缓解措施（注释、guard、分阶段实现标记等）。

**改进**：在子代理 prompt 中增加校准指引：

```
SEVERITY CALIBRATION:
Before rating any finding as CRITICAL, you MUST verify:
1. Is the code path reachable in normal usage? (not just test/dead code)
2. Are there documented mitigations? (comments like "SAFETY:", "TODO:",
   "Phase N:", "best-effort", or guards in calling code)
3. Does the surrounding context change the semantics?
   (e.g., a fallback that looks dangerous in isolation may be safe
   when the caller already validates input)

Rate CRITICAL only when: reachable + no mitigation + impact is crash/data-loss/security.
Rate HIGH when: reachable + partial mitigation + impact is degraded behavior.
```

### 3.8 静态分析辅助验证

**问题模式**：完全依赖人工代码阅读发现问题，遗漏编译器/linter 可自动检测的问题。

**改进**：子代理在手动审查前，先尝试运行项目已配置的 linter 或编译器检查（如项目有 `Makefile`、`lint` script、或标准工具链），将输出作为审查的起点。

**原则**：
- 子代理应自行识别项目类型并选择合适的静态分析命令
- 仅在项目有现成工具链时执行（不要安装新工具）
- 限制输出行数（如 `| head -100`）避免淹没上下文
- 将 linter 发现记录到 `take_note`，再决定哪些值得深入分析

**收益**：静态分析在几秒内发现的问题，人工审查需要数分钟。优先消费这些"免费"信息。

## 4. 与历史版本对比

| 维度 | v6（单子代理） | 本次（5 并行子代理） | 评价 |
|------|---------------|-------------------|------|
| 总耗时 | 357s | 786s | 并行开销 2.2x |
| 总 token | 2.35M | 5.62M | 2.4x |
| 缓存率 | 9.1% | 81.4% | ✅ 并行子代理缓存优异 |
| 审查覆盖 | 41 文件 | 217 文件（全量） | ✅ 全覆盖 |
| 报告质量 | A（0 误报） | B+（4 项夸大） | ⚠️ 综合验证缺失 |
| take_note | 0 | 8（仅 1/5 子代理） | 🔴 仍需强化 |
| grep_search | 16 | 因子代理而异 | 🟡 不稳定 |

**关键结论**：多子代理并行架构实现了全量覆盖（单子代理无法在上下文预算内审查 217 个文件），但引入了新的质量风险——编排器综合阶段的验证缺失成为准确性瓶颈。

## 5. 优先级排序

| # | 改进 | 预期收益 | 实现成本 | 优先级 |
|---|------|---------|---------|--------|
| 1 | 编排器综合验证约束化 | 误报率 ↓50%+ | 低（提示修改） | P0 |
| 2 | 子代理扫描优先策略 | token ↓40%, 覆盖率 ↑ | 低（提示修改） | P0 |
| 3 | `take_note` 强制使用 | 长任务发现完整性 ↑ | 低（提示修改） | P1 |
| 4 | 严重程度校准指引 | 夸大率 ↓ | 低（提示修改） | P1 |
| 5 | 编译器辅助验证 | 免费发现来源 | 低（提示修改） | P1 |
| 6 | 跨子代理边界信息 | 跨模块发现率 ↑ | 中（编排逻辑） | P2 |
| 7 | 重复执行检测 | 避免浪费 | 中（编排逻辑） | P2 |
| 8 | 负载均衡优化 | token 分布均匀化 | 低（分区公式） | P3 |
