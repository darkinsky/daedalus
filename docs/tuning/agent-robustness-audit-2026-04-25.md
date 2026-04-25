
# Agent 健壮性审查 — 对标 Claude Code 级别

> **基于 Trace**: v2 (`85d3d3f6`) + v3 (`9857f825`)
> **日期**: 2026-04-25
> **审查视角**: 如果要将 Daedalus 打造成 Claude Code 级别的健壮 agent，
>   从 trace 中可以发现哪些系统级问题？

---

## 一、致命问题：文件重复读取（Agent 没有"记忆索引"）

### 数据

| 文件 | v2 读取次数 | v3 读取次数 |
|------|:---------:|:---------:|
| `tool_loop.rs` | **20** | **11** |
| `venus_provider.rs` | **18** | **8** |
| `bash.rs` | **17** | **7** |
| `core_handler.rs` | **14** | — |
| `workspace.rs` | **13** | 2 |
| `tool_router.rs` | **11** | — |

v2 code-reviewer 读了 170 次文件，但只覆盖 **23** 个唯一文件。
v3 读了约 130 次，覆盖 **47** 个唯一文件。
项目共 **170** 个 `.rs` 文件。

**同一个文件被反复重新读取 10-20 遍**，因为截断后模型忘了自己已经读过这个文件。

### 对标 Claude Code

Claude Code 的 subagent 实现中有一个关键设计：**tool history 中的 tool call 参数（函数名+参数）永远不截断**。这意味着即使 tool response 被截断，模型仍然能看到"我在第 3 轮调用了 `read_file(path=tool_loop.rs)`"这个事实。

但光靠这一点不够。真正需要的是一个**文件读取索引**——一个不断追加、永不截断的紧凑列表，记录"哪些文件已读过、在哪一轮读的"。Claude Code 通过其 DynamicCheatsheet 实现类似功能。

### 建议

在 subagent 的 tool loop 中维护一个 **file access log**，作为额外的系统消息或注入到每轮的 user message 中：

```
[Files already read in this session]
Round 1: list_directory(/data/.../src, recursive)
Round 2: bash(wc -l ...), read_file(main.rs)
Round 3: read_file(tool_loop.rs), read_file(chat.rs), read_file(types.rs), bash(grep unwrap)
Round 4: read_file(venus_provider.rs), read_file(core_handler.rs), ...
...
(48 unique files read across 70 rounds)
```

这个列表只包含函数名+路径（不含 response），大约每个条目 80 字节，70 轮 × 3 个工具/轮 ≈ 16KB，远小于被截断丢失的信息量。

---

## 二、严重问题：Subagent 没有"进度感知"

### 数据

v2 code-reviewer 在第 100 轮（触顶）时的 output：

```
99 轮 output: ""  （空）
100 轮 → MaxRoundsExceeded → force_final_summary
```

v3 code-reviewer 的关键 output 时间线：

```
第  1 轮: "我将系统地审查这个 Rust 项目。首先进行整体结构探索。"
第 ?? 轮: "现在我已经对项目的核心模块有了深入了解，让我再看看几个关键区域。"
第 ?? 轮: "现在我已经阅读了大量关键源代码，让我再检查一些重要区域后输出完整报告。"
第 ?? 轮: "现在我已经收集了足够的信息来撰写全面的审查报告。让我整理所有发现。"
第 70 轮: 最终输出报告 (7061 completion tokens)
```

在 70 轮中**只有 4 次中间文本输出**，其中"让我再看看"、"再检查一些"反复出现——模型不知道自己离上限有多远。

### 对标 Claude Code

Claude Code 的 agent loop 有 **todolist/progress tracking** 机制：

1. Agent 知道当前轮次/总轮次上限
2. 在 prompt 中注入 `[Round 35/100]` 这样的信号
3. 当接近上限时（如 70%），agent 会主动转入总结模式

### 建议

在 `run_tool_loop` 中，每轮调用 LLM 前将**轮次进度**注入到工具历史或中间消息中：

```
[Progress: Round 50/100 — 50% of tool-calling budget used.
 You have read 35 unique files out of ~170 total.
 If you have collected enough findings, consider outputting your report soon.]
```

当达到 70% 时升级为更强警告：

```
⚠️ [Round 70/100 — 70% budget used. You MUST begin writing your report in the
next few rounds. Any unwritten findings will be lost.]
```

---

## 三、严重问题：Subagent 的中间输出几乎全空 — 发现随截断丢失

### 数据

| 指标 | v2 code-reviewer | v3 code-reviewer |
|------|:---------------:|:---------------:|
| 总 LLM 调用 | ~100 | 70 |
| output="" 的轮次 | **99**（99%） | **66**（94%） |
| 有内容 output 的轮次 | **1** | **4** |

在代码审查这种增量发现的任务中，模型在 100 轮的读文件过程中**几乎不产生任何中间文本输出**。所有发现都"记在脑子里"（即留在上下文中），一旦被截断就永远丢失了。

### 对标 Claude Code

Claude Code 的 prompt 中有明确指令：

> "Record findings as you go. Never accumulate everything in your head
> and output at the end — if context is lost, so are your findings."

我们的 code-reviewer prompt 中虽然也有类似文字（Working Principles 第 1 条），但模型行为表明**它没有遵守**。原因是：

1. Tool-calling 模式下，模型倾向于只输出 tool calls 而不产生文本
2. Claude 的 API 行为：当返回 tool_calls 时，content 通常为空——这不是模型的选择，而是 API 的默认行为
3. 缺乏系统级强制手段让模型在 tool-calling 轮次中也产出文本

### 建议

**方案 A：引入"笔记"工具**

创建一个 `take_note` 内置工具，让模型在 tool-calling 轮次中通过工具调用来记录发现：

```json
{
  "name": "take_note",
  "description": "Record a finding or observation. Notes are preserved across context truncation and will be available when writing the final report. Use this after reading each file to capture any issues found.",
  "parameters": {
    "note": { "type": "string", "description": "The finding to record" }
  }
}
```

这些 note 存储在一个不随 tool history 截断的单独列表中，并在每轮注入给 LLM。

**方案 B：Scratchpad 系统消息**

在 tool loop 中维护一个 scratchpad，每隔 N 轮强制注入一条系统消息：

```
[Scratchpad — your recorded findings so far:]
- tool_loop.rs:228 — truncate_tool_history 没有预算感知
- venus_provider.rs:155 — API key 在日志中明文输出
- bash.rs:89 — timeout 硬编码为 30s
...
```

---

## 四、架构问题：Subagent 无法增量输出（全有或全无）

### 问题

Subagent 的结果返回是**原子性的**——要么完整返回 `SubagentResult.content`，要么触顶后由 `force_final_summary` 生成一个压缩版。主 agent 看不到子代理的任何中间进展。

这导致：
- 用户在 10 分钟内看不到任何进度
- 如果网络中断或进程被杀，所有工作丢失
- 无法实现"超时后继续"或"中断后恢复"

### 对标 Claude Code

Claude Code 的 subagent 通过 `ToolEvent::SubagentProgress` 提供实时进度更新，且支持 streaming partial results。但更重要的是 Claude Code 的 subagent 天然在主 agent 的工具循环内运行，主 agent 能实时看到 subagent 的工具调用结果。

### 建议

考虑为长时间运行的 subagent 实现**检查点机制**：

1. 每 N 轮（如 10 轮）将当前已完成的工作写入一个临时文件
2. `force_final_summary` 可以直接读取检查点文件，而非从 300 字节碎片重建
3. 如果 subagent 被中断，主 agent 可以从检查点恢复

---

## 五、效率问题：Completion tokens 极度浪费

### 数据

v2 code-reviewer 的 completion tokens 分布：

```
70-75 tokens/轮:   51 轮（51%）  ← 这些是纯 tool call JSON
127-131 tokens/轮: 13 轮（13%）  ← 稍长的 tool call
164-200 tokens/轮: 18 轮（18%）  ← 稍长的 tool call
```

v3 类似但稍好（更多 200-300 token 轮次表示更丰富的 tool call 参数）。

**关键观察**：模型在 100 轮中的 completion tokens 总量仅约 **13K**（v2）和 **15K**（v3），但 prompt tokens 消耗了 **4.3M**（v2）和 **6.6M**（v3）。

**输入/输出比 = 300:1 到 440:1**。

这意味着模型花了 99.7% 的 token 在"阅读"，只有 0.3% 在"产出"。对于代码审查任务来说这个比例极不健康——它应该是大约 10:1 到 20:1。

### 根因

1. 每轮都发送完整的 system prompt（~15K chars）+ 完整的 tool definitions（~3K chars）
2. 每轮都发送所有历史 tool history（截断后仍有 80-120K tokens）
3. 模型的"思考"全部在 prompt tokens 中完成（读取历史），而非 completion tokens（输出分析）

### 建议

1. **Prompt caching 验证**：确认 Venus provider 的 prompt caching 实际命中。
   System prompt + tool definitions 在 100 轮中完全相同，应该有 ~18K tokens/轮的缓存节省。
   如果缓存命中率低，这是最大的优化机会。

2. **Tool history 增量传递**：考虑只发送上一轮的 delta（新增的 tool calls + responses），
   而非每轮重新发送完整历史。这需要 LLM API 支持 conversation continuation 模式。

---

## 六、覆盖率问题：170 个文件只审查了 14-28%

### 数据

| 指标 | v2 | v3 |
|------|----|----|
| 唯一文件读取数 | 23 (explore 49 + CR 23) | 47 |
| 总 .rs 文件数 | 170 | 170 |
| 覆盖率 | **14%** | **28%** |
| token 成本 | 6.2M | 6.7M |

花费 600 万+ tokens，只审查了不到三分之一的代码。主要原因：

1. **重复读取**（见问题一）浪费了大量轮次
2. **无文件优先级排序**：模型逐个读取文件，没有"哪些文件最重要/最大/最可能有问题"的策略
3. **缺乏"够了就停"的信号**：对于 21K 行的项目，读完 50 个关键文件（覆盖 80% 代码量）就足以写出高质量报告

### 建议

1. 在 code-reviewer 的 prompt Phase 2 (Scan) 中，先用 `bash wc -l src/**/*.rs | sort -rn` 获取文件大小排序，然后按大小/重要性优先级读取

2. 引入**覆盖率跟踪**：在每轮注入 `[Coverage: 47/170 files read, ~12,000/21,877 lines covered (55%)]`

3. 设置**覆盖率目标**：当已读文件覆盖了 >60% 的代码行数时，自动提示模型开始输出报告

---

## 七、安全问题：Subagent 的 `bash` 工具无沙箱

### 观察

v3 trace 中 code-reviewer 调用了 `bash` 工具：

```
Round 2: bash("find /data/workspace/ams/daedalus/src -name '*.rs' | wc -l")
Round 3: bash("wc -l /data/workspace/ams/daedalus/src/agent/*.rs | sort -rn")
```

code-reviewer 的 `permissionMode: plan` 但仍然能执行任意 bash 命令。虽然 prompt 说 "read-only"，但这是一个软约束。

### 对标 Claude Code

Claude Code 的 subagent 在 `plan` 模式下：
- 写入操作会被拦截
- bash 命令经过安全审查
- 有 `allowedCommands` 白名单

### 建议

为 `plan` 模式的 subagent 实现 bash 命令白名单：
- 允许：`find`、`wc`、`grep`、`cat`、`head`、`tail`、`ls`、`file`、`stat`
- 拒绝：`rm`、`mv`、`cp`、`chmod`、`curl`、`wget`、管道到文件（`>`）

---

## 八、可观测性问题：Trace 缺乏关键诊断指标

### 当前 trace 记录的

- LLM 调用时间、tokens、工具调用列表
- Subagent 的总轮次和 token 消耗

### 缺少的关键指标

| 缺失指标 | 为什么重要 |
|----------|----------|
| **每轮的 truncated history 估算 tokens** | 无法区分"上下文不够"vs"截断太激进" |
| **截断触发次数和被截断的内容量** | 无法衡量信息丢失程度 |
| **文件读取去重率** | 直接衡量"遗忘"的严重程度 |
| **prompt cache 命中率** | 关键成本优化指标 |
| **中间文本输出量** | 衡量"增量记录"策略的执行效果 |
| **工具调用并行度（每轮平均/最大）** | 衡量 prompt 中并行指导的有效性 |

### 建议

在 `run_tool_loop` 中每轮记录：

```rust
tracing::info!(
    round = round_number,
    full_history_tokens = estimate_history_chars(&tool_history) / 4,
    truncated_history_tokens = estimate_history_chars(&truncated_history) / 4,
    tokens_truncated = (full - truncated),
    unique_files_read = file_access_set.len(),
    total_file_reads = total_reads,
    parallel_calls = tool_calls.len(),
    "Tool loop round stats"
);
```

---

## 九、总结：优先级排序

| # | 问题 | 影响 | 工作量 | 对标 Claude Code |
|---|------|------|--------|-----------------|
| 1 | 文件重复读取（无读取索引） | 🔴 60-80% 的轮次浪费在重读 | ⭐⭐ | DynamicCheatsheet |
| 2 | 中间发现随截断丢失（无笔记系统） | 🔴 审查质量根本受限 | ⭐⭐ | Scratchpad / take_note |
| 3 | 无进度感知（不知距上限多远） | 🟠 触顶风险 + 犹豫行为 | ⭐ | Round counter in prompt |
| 4 | 覆盖率低（28%/170 文件） | 🟠 审查不完整 | ⭐ | File prioritization |
| 5 | 增量输出机制缺失 | 🟠 全有或全无 | ⭐⭐⭐ | Checkpoint system |
| 6 | Prompt cache 未验证 | 🟡 可能浪费 30% 成本 | ⭐ | Cache-control headers |
| 7 | Bash 无沙箱 | 🟡 安全风险 | ⭐⭐ | Command allowlist |
| 8 | Trace 缺诊断指标 | 🟡 无法持续优化 | ⭐ | Structured logging |

**如果只做一件事**：实现 #1（文件读取索引）。它同时改善 #4（覆盖率）和减少 #3（犹豫行为），是投入产出比最高的改动。
