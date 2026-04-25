# Code Review Trace 分析 — v5（2026-04-25）

## 1. v5 运行概览

| 指标 | 值 |
|------|-----|
| Trace ID | `a14e380f-7ffd-41f5-b294-56511aad291d` |
| 总耗时 | 393s（~6.5 min） |
| 总 token | 3,377,026 |
| subagent | 1（code-reviewer，20 轮） |
| 主 agent LLM 调用 | 2（派 subagent + 整合） |
| 审查报告质量 | A-（1 Critical + 8 Major，90% 准确率） |

## 2. 五版全量对比

| 指标 | v2 | v3 | v4 | v5 | 趋势 |
|------|:-:|:-:|:-:|:-:|:-:|
| 总耗时 | 877s | 609s | 706s | **393s** | ✅✅ |
| 总 token | 6.21M | 6.68M | 8.72M | **3.38M** | ✅✅ |
| subagent 数量 | 2 | 1 | 1 | 1 | ✅ |
| code-reviewer 轮次 | 100（触顶） | 70 | 74 | **20** | ✅✅ |
| 上下文峰值 | 66K | 153K | 230K | **240K** | ✅ |
| 上下文稳态 | 40-55K | 80-100K | 90-126K | **113-135K** | ✅ |
| 工具并行度（平均） | 1.3 | 2.3 | 2.3 | **4.1** | ✅✅ |
| 唯一文件读取 | 23 | 47 | 34 | 35 | ≈ |
| 文件最大重复 | 20 | 11 | 6 | **5** | ✅ |
| 中间 output 非空 | 1% | 6% | 18% | **21%** | ✅ |
| take_note 调用 | 0 | 0 | 6 | **0** | ⚠️ |
| grep_search 调用 | 少 | 21 | 21 | **0** | ⚠️ |
| cached_tokens | — | — | 317K | **52K** | ⚠️ |
| 缓存率 | — | — | 3.65% | **1.55%** | ⚠️ |
| 语言一致性 | 差 | — | 50%中英混杂 | **100%中文** | ✅✅ |
| 审查报告质量 | — | — | B+ | **A-** | ✅ |

## 3. v5 改善确认

### 3.1 效率飞跃：20 轮完成审查

v4 需要 74 轮，v5 只用 20 轮。token 消耗从 8.72M 降到 3.38M（-61%），耗时从 706s 降到 393s（-44%）。

### 3.2 并行度历史最高

```
4 工具/轮: 13 次 (62%)
5 工具/轮:  4 次 (19%)
6 工具/轮:  2 次 (10%)
3 工具/轮:  1 次 (5%)
1 工具/轮:  1 次 (5%)  ← 最终总结轮
```

平均 4.1 工具/轮，v4 为 2.3。code-reviewer prompt 中的并行指导生效。

### 3.3 语言一致性修复

5 条非空 output 全部为中文，无英文混杂。v4 中约 50% 中英交替。

### 3.4 Prompt tokens 变化

```
第  1 轮:   5,893  ▏
第  3 轮: 143,100  ████████████████████████████████████▎
第  4 轮: 168,338  ██████████████████████████████████████████▊
第  5 轮: 192,377  ████████████████████████████████████████████████▊
第  6 轮: 206,825  ████████████████████████████████████████████████████▊
第  7 轮: 214,434  ██████████████████████████████████████████████████████▋
第  8 轮: 220,126  ███████████████████████████████████████████████████████▋
第  9 轮: 224,764  ████████████████████████████████████████████████████████▊
第 10 轮: 228,914  █████████████████████████████████████████████████████████▋
第 11 轮: 232,107  ██████████████████████████████████████████████████████████▎
第 12 轮: 239,699  ████████████████████████████████████████████████████████████  ← 峰值
第 13 轮: 113,442  ████████████████████████████▋  ← 断崖 -126K ⚠️
第 14 轮: 117,696  █████████████████████████████▋
第 15 轮: 120,635  ██████████████████████████████▍
...
第 20 轮: 134,062  █████████████████████████████████▊
第 21 轮: 135,285  ██████████████████████████████████  ← 最终总结
```

## 4. 待修复问题清单

### 4.1 🔴 截断断崖仍然存在（240K → 113K，-126K）

**现状**：v4 修复后从 230K→73K（-157K）改善到 240K→113K（-126K），但断崖仍然过大。

**根因分析**：逐轮截断逻辑已生效，但 `estimate_history_chars` 使用 `CHARS_PER_TOKEN = 4` 估算偏低。JSON 格式的 tool calls 含大量 `{}",:` 元字符，实际比率约 3 chars/token。这导致算法以为自己在预算内（按 4 chars 算 113K tokens），实际已超出（按 3 chars 算 150K tokens），需要一次性截断多轮才能回到真正的预算。

**修复方案**：
```rust
// 方案 A：降低 CHARS_PER_TOKEN
const CHARS_PER_TOKEN: usize = 3; // 从 4 降到 3，更保守的估算

// 方案 B：分别估算 tool calls 和 responses
fn estimate_history_chars(history: &[ToolRound]) -> usize {
    let mut total = 0;
    for round in history {
        for call in &round.calls {
            // JSON 元字符开销 ~30%，用 0.7 折扣
            total += (call.function_name.len() + call.arguments.to_string().len()) * 10 / 7;
        }
        for resp in &round.responses {
            total += resp.content.len(); // 纯文本，4 chars/token 合理
        }
    }
    total
}
```

**优先级**：P0

---

### 4.2 🔴 Cache 命中率极低（1.55%）

**现状**：v5 缓存率 1.55%（52K / 3.36M），比 v4 的 3.65% 还低。

**原因**：v5 trace 运行时间为 15:49-15:56，cache 断点优化代码在 16:11 才提交。v5 **没有享受到 cache 优化**。

**已实施的修复**（待验证）：
1. `venus_provider.rs`：tool history 中最后一个截断轮次的 response 标记 `cache_control: ephemeral`
2. `runner.rs`：`cache_control` 标记从 system prompt 移到 user task，扩大缓存前缀
3. `genai_provider.rs`：同步实现了 cache_control 传递和 cached_tokens 提取

**验证方式**：再跑一次代码审查，检查 span 级 trace 中是否出现 `cached=N` 字段。

**预期效果**：缓存率从 1.55% → 30-50%。

**优先级**：P0（已修复，待验证）

---

### 4.3 🟠 Span 级 cached_tokens 仍不可见

**现状**：trace 总汇总有 `cached_tokens: 52070`，但每轮的 `usage: prompt=X, completion=Y, total=Z` 中没有 `cached=W`。

**原因**：`file.rs` 中 span 级格式化已添加 `cached_tokens`（本轮修复），但 v5 trace 在修复前运行。

**验证方式**：下次运行应在每轮 usage 行中看到 `cached=N`。

**优先级**：P1（已修复，待验证）

---

### 4.4 🟠 `take_note` 工具未被使用

**现状**：v5 一次都没调用 `take_note`（v4 调用了 6 次）。

**原因分析**：
- v5 只有 20 轮，上下文从未溢出截断窗口（113-135K 稳态），模型没有"忘记"的压力
- code-reviewer prompt 中 `take_note` 的引导是被动式的（"Record findings as you go"），不够强制

**风险**：对更大项目（100+ 文件，需要 50+ 轮），上下文会溢出，如果模型仍然不用 `take_note`，发现会丢失。

**修复方案**：在 code-reviewer prompt 中将 `take_note` 从建议变为 Phase 转换的强制步骤：

```markdown
### Phase Transitions

- After completing Phase 2 (Pattern Scan), **you MUST call `take_note`
  once per critical/major finding** before proceeding to Phase 3.
- After completing Phase 3 (Deep Read) for each batch of files,
  call `take_note` to record any new findings.
- When writing the final report, review all notes first.
```

**优先级**：P1

---

### 4.5 🟠 `grep_search` 完全被 `bash grep` 替代

**现状**：v5 有 39 次 bash 调用，0 次 grep_search。模型用 `bash` 运行 `grep`/`rg` 替代了内置 `grep_search`。

**问题**：
- `grep_search` 内置 ripgrep，自动 .gitignore 过滤，返回格式化的匹配行
- `bash grep` 没有 .gitignore 过滤，可能搜到 `target/`、`.git/` 中的噪音
- `bash grep` 的输出格式不如 `grep_search` 结构化

**修复方案**：在 code-reviewer prompt 的 "Right tool for the job" 中强化：

```markdown
- **Pattern search → `grep_search`** (not `bash grep`): built-in ripgrep
  is faster, respects .gitignore, and returns structured output with
  file paths and line numbers. Reserve `bash` for `wc`, `find`, `sort`,
  and other non-search commands.
```

**优先级**：P2

---

### 4.6 🟠 覆盖率低（35/171 = 20%）

**现状**：v5 只读取了 35 个唯一文件，是所有版本中覆盖率最低的（v3 覆盖 47 个）。

**原因**：20 轮完成审查，时间不够遍历更多文件。但审查质量反而最高（A-），说明**精读 20% > 粗读 50%**。

**权衡**：这可能不需要修复——20 轮 + 4.1 并行度 + 精准定位 = 高 ROI。如果用户需要更高覆盖率，可以手动设置 `maxTurns: 50`。

**潜在改进**：在 Phase 1 中加入文件优先级排序：
```markdown
- Phase 1 first step: run `wc -l src/**/*.rs | sort -rn | head -30`
  to identify the largest files. Prioritize reviewing files > 200 lines.
```

**优先级**：P2

---

### 4.7 🟡 `estimate_history_chars` 每轮重复序列化 JSON

**现状**：`call.arguments.to_string()` 在每次调用 `estimate_history_chars` 时都完整序列化一次 JSON。截断算法中这个函数被多次调用（每轮检查是否在预算内 + 每级截断后重检查）。

**修复方案**：
```rust
// 方案 A：预计算 arguments 大小
// 在 ToolCall 中缓存 arguments 的序列化长度

// 方案 B：直接用 serde_json::Value 的递归大小估算
fn estimate_json_size(value: &serde_json::Value) -> usize {
    match value {
        Value::String(s) => s.len() + 2, // quotes
        Value::Object(map) => map.iter()
            .map(|(k, v)| k.len() + estimate_json_size(v) + 4) // key:value,
            .sum::<usize>() + 2, // braces
        Value::Array(arr) => arr.iter()
            .map(|v| estimate_json_size(v) + 1) // element,
            .sum::<usize>() + 2, // brackets
        _ => 8, // numbers, bools, nulls
    }
}
```

**优先级**：P2

---

### 4.8 🟡 Subagent 缺少语言指令传递

**现状**：v5 的语言一致性已经修复（全中文），但这是偶然的——因为 task 是中文、模型能力足够。code-reviewer 的 system prompt 和 `build_effective_prompt()` 中仍然没有显式的语言指令。

**之前的问题**（v4）：50% 中英文混杂。

**修复方案**：在 `build_effective_prompt()` 或 `build_constraints_section()` 中添加：
```rust
constraints.push(
    "Respond in the same language as the task description. \
     If the task is in Chinese, all output must be in Chinese."
);
```

或让 `SubagentRunner::run()` 从 task 中检测语言并注入到 constraints。

**优先级**：P2

---

## 5. 已修复但待验证的项

| 修复 | 文件 | 状态 |
|------|------|------|
| Cache 断点优化（tool history 稳定前缀） | `venus_provider.rs` | ✅ 已合入，⏳ 待 trace 验证 |
| Cache 断点同步到 genai | `genai_provider.rs` | ✅ 已合入，⏳ 待 trace 验证 |
| Span 级 cached_tokens 输出 | `file.rs` | ✅ 已合入，⏳ 待 trace 验证 |
| Cache control 从 system 移到 user | `runner.rs` | ✅ 已合入，⏳ 待 trace 验证 |
| Genai cached_tokens 提取 | `genai_provider.rs` | ✅ 已合入，⏳ 待 trace 验证 |

## 6. 下一步优先级

1. **再跑一次代码审查**验证 cache 优化效果
2. **P0**：修复 `CHARS_PER_TOKEN` 估算偏差（截断断崖根因）
3. **P1**：强化 `take_note` 在 code-reviewer prompt 中的引导
4. **P2**：语言指令传递、`grep_search` 引导、覆盖率优化
