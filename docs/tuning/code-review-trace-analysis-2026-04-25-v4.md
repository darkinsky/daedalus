
# 代码审查任务 — Trace 分析与优化报告（第四轮）

> **日期**: 2026-04-25
> **Trace ID**: `64aafb7c-197e-4a8b-b61c-8daea15dd6f3`
> **任务**: "帮我给当前项目做一个代码审查"
> **模型**: claude-sonnet-4-6（通过 Venus）
> **背景**: v3 修复后引入了预算式动态截断、文件读取索引、进度注入、take_note 工具。
>   本轮验证效果、评估审查报告质量、发现残余问题。

---

## 1. 执行摘要

| 指标 | v2 | v3 | **v4** | 趋势 |
|------|:--:|:--:|:------:|:----:|
| 总耗时 | 877s | 609s | **706s** | ↑ |
| 总 token | 6.21M | 6.68M | **8.72M** | ↑ |
| subagent 数量 | 2 | 1 | **1** | ✅ |
| code-reviewer 轮次 | 100（触顶） | 70 | **74** | ✅ |
| 上下文峰值 | 66K | 153K | **230K** | ✅✅ |
| 上下文稳态 | 40-55K | 80-100K | **90-126K** | ✅ |
| 并行度（平均） | 1.3 | 2.3 | **2.3** | ✅ |
| 唯一文件读取 | 23 | 47 | **34** | ⚠️ |
| 最大重复读取 | 20 | 11 | **6** | ✅ |
| 中间文本输出 | 1% | 6% | **18%** | ✅✅ |
| take_note 调用 | — | — | **6** | ✅ |
| cached_tokens | 未记录 | 未记录 | **317,502** | ✅ |
| 审查报告质量 | 低 | 中 | **高**（B+） | ✅✅ |

## 2. 改善确认

### ✅ 上下文利用率：230K 峰值（90% 窗口使用率）

```
v3:  5K → 153K → (截断) → 80-100K 稳态
v4:  5K → 141K → 195K → 215K → 223K → 230K（峰值）
     → (截断) → 102K → 82K → 73K → 回升到 90-126K 稳态
```

预算截断在接近上限时才触发，而非 v3 中第 14 轮就开始截断。

### ✅ 文件重复读取从 20 次降到 6 次

文件读取索引注入生效。`bash.rs` 和 `venus_provider.rs` 仍有 6 次重复，
因为模型在不同阶段需要审查同一文件的不同部分（通过 `bash cat ... | head -50`），
属于合理的部分重读。

### ✅ 中间发现持久化：take_note 6 次 + 文本输出 18%

v2 中 100 轮只有 1 轮产出文本；v4 中 74 轮有 13 轮产出文本 + 6 次 take_note 调用。
模型开始主动记录中间发现。

### ✅ 审查报告质量为历轮最高

v4 产出的审查报告包含 6 个 Critical + 5 个 Major，且全部指向真实代码问题：
- API key UTF-8 切片 panic（真实 bug）
- PermissionMode::Plan 权限绕过（功能性安全漏洞）
- bash 超时子进程泄漏（资源泄漏）
- shared_notes: None 导致 take_note 功能失效（刚写的代码中的 bug）
- HTTP 客户端无超时（生产挂死风险）

### ✅ Prompt cache 首次记录

`cached_tokens: 317,502`（占 prompt 的 3.7%）。缓存机制工作中，
但命中率较低，可能是因为 tool history 每轮变化导致缓存前缀不稳定。

### ✅ 工具多样性提升

| 工具 | v3 占比 | v4 占比 |
|------|:------:|:------:|
| bash | ~5% | **54%**（91 次） |
| read_file | ~85% | **33%**（55 次） |
| grep_search | ~10% | **12%**（21 次） |
| take_note | — | 6 次 |

符合 "Prefer cheap tools" 的指导，`bash grep/wc/cat` 信息密度高于 `read_file`。

---

## 3. 残余问题

### 🔴 P0 — 截断断崖：230K → 73K（应平缓到 ~120K）

**症状**: prompt tokens 在第 12 轮达到 230K 后，第 13 轮骤降到 102K，
继续降到 73K，然后花了 20 轮才慢慢回升到 120K。

**根因**: 三级截断（moderate → aggressive → micro）对**所有**非保护轮次
**同时**应用。当触发时，所有老轮一起被砍，砍掉的量远超必要。

**对标 Claude Code**: Claude Code 的截断是**逐轮渐进**的——从最老的一轮开始，
每次只截断一轮，截完检查是否回到预算，够了就停。

**修复方案**: 反转截断循环的嵌套顺序：

```rust
// 当前（问题代码）: 按级别遍历，每级截断所有轮
for tier in [moderate, aggressive, micro] {
    if within_budget() { break; }
    for round in 0..protected_start {  // ← 所有轮同时被砍
        truncate(round, tier);
    }
}

// 修复后: 按轮遍历，每轮逐级截断
for round in 0..protected_start {      // ← 从最老的轮开始
    for tier in [moderate, aggressive, micro] {
        truncate(round, tier);
        if within_budget() { return; }  // ← 够了就停
    }
}
```

**预期效果**: 230K 时只截断最老的 3-5 轮就能回到 120K 预算，
其余轮保持完整。上下文曲线变为平缓下降而非断崖。

---

### 🟠 P1 — Session Progress 注入未生效

**症状**: trace 中搜索 `Session Progress` = 0 结果。synthetic tool round
使用了不存在的工具名 `_session_progress`，可能被 LLM API 忽略或过滤。

**对标 Claude Code**: Claude Code 不伪造 tool round，而是在 system prompt
的动态段追加 session 状态，或作为 user message 插入。

**修复方案**: 将 session metadata 从 synthetic tool round 改为注入到
tool history 的最后一个 response 中（作为附加文本），或构建为额外的
system message：

```rust
// 方案 A: 追加到最近一轮的最后一个 tool response 后面
if let Some(last_round) = truncated_history.last_mut() {
    if let Some(last_resp) = last_round.responses.last_mut() {
        last_resp.content.push_str(&format!(
            "\n\n[Session: Round {}/{}, {} files read, {} notes]",
            round_number, max_rounds, files_read.len(), note_count
        ));
    }
}

// 方案 B: 作为 messages 的最后一条 user message 追加
// （需要修改 messages 的构建流程）
```

---

### 🟡 P2 — bash 过度使用，read_file 不足

**症状**: 从第 25 轮开始几乎全是 `[bash, bash]` 模式。模型用
`bash cat file.rs | head -50` 替代 `read_file`，丢失了行号信息，
每次只看 50 行片段，容易遗漏文件中间部分的问题。

**对标 Claude Code**: Claude Code 的 subagent prompt 明确区分工具角色：
bash 用于搜索和统计，read_file 用于深度阅读。

**修复方案**: 在 `code-reviewer.md` 的 "Context & Parallelism Management"
段落补充：

```markdown
- **Use read_file for code review, not bash cat**: `read_file` returns
  line numbers and supports offset/limit for large files. `bash cat`
  loses line numbers, making it harder to report issues with precise
  locations. Reserve `bash` for `grep`, `wc -l`, `find`, and other
  analysis commands.
```

---

## 4. 成本分析

### 当前成本（Claude Sonnet 4: $3/M input, $15/M output）

| 阶段 | Prompt | Completion | 成本 |
|------|:------:|:----------:|:----:|
| 主代理（2 轮） | ~20K | ~4.4K | ~$0.13 |
| code-reviewer（74 轮） | 8.69M | ~24K | ~$26.4 |
| **合计** | **8.72M** | **28.6K** | **~$26.5** |

### 成本趋势

| 版本 | 总 token | 成本 | 报告质量 |
|------|:-------:|:----:|:--------:|
| v2 | 6.21M | ~$19 | 低 |
| v3 | 6.68M | ~$20 | 中 |
| v4 | 8.72M | ~$26 | **高** |

成本上涨 37%，但审查质量从"低"提升到"高"。Token 增加主要来自更大的上下文窗口
（90-126K 稳态 vs v3 的 80-100K），属于"用更多上下文换更好质量"的合理代价。

### 如果修复截断断崖后的预估

修复后上下文稳态应从 90-126K（波动大）收窄到 110-120K（稳定），
prompt tokens 预计减少 ~10%，约 $23-24。

---

## 5. 审查报告质量评估

v4 产出的审查报告评级 **B+**：

**优点**:
- 6 个 Critical 全部是真实 bug
- 修复建议可执行，附代码片段
- 💚 Praise 部分有实质内容（不是泛泛的夸奖）
- 架构建议准确

**不足**:
- 171 个文件只涉及 ~15 个（覆盖率 ~9%）
- 缺少并发安全 / async 取消安全的系统性分析
- 部分 Minor 是对刚写的代码的"自我审查"

---

## 6. 实施检查清单

- [ ] **P0**: `tool_loop.rs` — 截断循环改为逐轮渐进（反转嵌套顺序）
- [ ] **P1**: `tool_loop.rs` — session progress 从 synthetic tool round 改为追加到最近 response
- [ ] **P2**: `code-reviewer.md` — 补充"read_file 用于审查，bash 用于搜索"指导

---

## 7. 附录：Prompt Token 变化趋势

```
第  1 轮:   5,825  ▏
第  3 轮: 141,017  ████████████████████████████████████▍         ← 首批大量读取
第  4 轮: 146,633  █████████████████████████████████████▊
第  5 轮: 180,331  ██████████████████████████████████████████████▋
第  6 轮: 195,605  ██████████████████████████████████████████████████▋
第  7 轮: 205,137  █████████████████████████████████████████████████████▏
第  8 轮: 215,634  ███████████████████████████████████████████████████████▋
第  9 轮: 219,686  ████████████████████████████████████████████████████████▊
第 10 轮: 223,836  █████████████████████████████████████████████████████████▊
第 11 轮: 226,765  ██████████████████████████████████████████████████████████▋
第 12 轮: 230,091  ███████████████████████████████████████████████████████████▌ ← 峰值
第 13 轮: 102,621  ██████████████████████████▌                                  ← 断崖！(-128K)
第 14 轮: 104,129  ██████████████████████████▊
第 15 轮:  82,744  █████████████████████▍                                       ← 谷底
第 16 轮:  75,579  ███████████████████▌
第 17 轮:  73,599  ███████████████████▏
...逐步回升
第 30 轮:  94,178  ████████████████████████▍
第 50 轮: 109,751  ████████████████████████████▍
第 70 轮: 125,702  ████████████████████████████████▋
第 74 轮: 126,130  ████████████████████████████████▊                            ← 最终总结前
```
