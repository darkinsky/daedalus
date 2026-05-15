
# Daedalus Prompt 设计优化分析

> **日期**：2026-05-15
> **版本**：v1.0
> **范围**：对比 Claude Code (v2.1.88 泄露源码) 与 Daedalus 的 System Prompt 设计，从 6 个维度评估改进空间
> **相关文件**：
> - `src/prompt/mod.rs` — 统一入口 + Default style builder
> - `src/prompt/coding/mod.rs` — Coding style builder
> - `src/prompt/coding/sections/` — Coding style 各 section
> - `src/prompt/sections/` — Default style 各 section
> - `src/prompt/inputs.rs` — 共享输入字段
> - `src/subagent/prompt.rs` — Subagent prompt 组装

---

## 目录

- [一、架构总览对比](#一架构总览对比)
- [二、六维对比分析](#二六维对比分析)
  - [1. 通用性](#1-通用性--是否对任务足够泛化)
  - [2. 专用性](#2-专用性--是否对特殊任务有充分指导)
  - [3. 清晰性](#3-清晰性--是否足够清晰)
  - [4. 模块化](#4-模块化--是否足够模块化)
  - [5. 简洁性](#5-简洁性--是否足够简洁)
  - [6. 布局](#6-布局--布局是否足够清晰)
- [三、改进计划](#三改进计划)
  - [P0 — 高收益低成本](#p0--高收益低成本)
  - [P1 — 中等收益](#p1--中等收益)
  - [P2 — 长期架构优化](#p2--长期架构优化)
- [四、总结评分](#四总结评分)

---

## 一、架构总览对比

### Claude Code 的 Prompt 架构（7 层静态 + 11+ 动态）

```
getSystemPrompt() → string[]

═══ 静态区（全局缓存） ═══
1. getSimpleIntroSection()        — 身份："你是一个交互式智能体"
2. getSimpleSystemSection()       — 安全：防 prompt 注入、hooks 处理
3. getSimpleDoingTasksSection()   — 任务执行策略（区分写代码/回答问题/分析）
4. getActionsSection()            — 文件操作确认边界
5. getUsingYourToolsSection()     — 工具使用指导（按工具分类）
6. getSimpleToneAndStyleSection() — 语气与风格
7. getOutputEfficiencySection()   — 输出效率（简洁性）

═══ SYSTEM_PROMPT_DYNAMIC_BOUNDARY ═══

═══ 动态区（按会话变化） ═══
8.  getEnvironmentSection()            — CWD、OS、shell
9.  getMCPSection()                    — MCP 工具扩展指令
10. getLanguageSection()               — 语言偏好（变量注入）
11. getOutputStyleSection()            — 输出风格配置
12. getHooksSection()                  — 用户 hooks 反馈
13. getSystemRemindersSection()        — 系统提醒标签说明
14. getProjectRulesSection()           — CLAUDE.md 项目规则
15. getMemorySection()                 — 记忆注入
16. getSkillsSection()                 — 技能注入
17. getSummarizeToolResultsSection()   — 工具结果摘要说明
...
```

**核心设计原则**：

| 原则 | 说明 |
|------|------|
| 动静分离 | `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 标记将提示词分为静态（可全局缓存）和动态（按会话）两部分 |
| 零拷贝传递 | Fork 子进程直接复用父进程的渲染后系统提示字节，保证 prompt cache 完全命中 |
| 按需计算 | 动态 section 通过 `systemPromptSection()` 缓存，通过 `DANGEROUS_uncachedSystemPromptSection()` 强制每轮重算 |
| 上下文压缩 | `SUMMARIZE_TOOL_RESULTS_SECTION` 告知模型旧工具结果会自动清除 |

### Daedalus Coding Prompt 架构（5 静态 + 3 动态）

```
CodingPromptBuilder.build() → String

═══ 静态区 ═══
1. identity     — 身份 + 核心原则 + 工具感知
2. personality  — Soul（可选）
3. tools        — 工具列表 + 使用策略
4. rules        — 核心规则 + 代码变更 + 搜索策略 + 沟通风格 + 子代理委派
5. reminders    — 8 条关键提醒

═══ SYSTEM_PROMPT_DYNAMIC_BOUNDARY ═══

═══ 动态区 ═══
6. environment    — OS、shell、CWD、项目类型、日期
7. project_rules  — DAEDALUS.md
8. memory         — 记忆注入
```

### Daedalus Default Prompt 架构（6 静态 + 2 动态）

```
PromptBuilder.build() → String

═══ 静态区 ═══
1. role             — 身份 + 能力 + 工具感知
2. soul             — 人格（可选）
3. thinking_style   — 推理方法 + 自适应规划
4. tool_system      — 工具列表 + 使用指南（仅有工具时）
5. response_style   — 输出格式
6. reminders        — 关键提醒

═══ CACHE_BOUNDARY ═══

═══ 动态区 ═══
7. project_rules    — DAEDALUS.md
8. context          — 日期 + 记忆注入
```

### 架构差异总结

| 维度 | Claude Code | Daedalus (Coding) | Daedalus (Default) |
|------|:-----------:|:-----------------:|:------------------:|
| 静态 section 数 | 7 | 5 | 6 |
| 动态 section 数 | 11+ | 3 | 2 |
| 总 section 数 | 18+ | 8 | 8 |
| 缓存边界 | ✅ | ✅ | ✅ |
| XML 标签包裹 | ❌（用 `#` 标题） | ✅ | ✅ |
| 条件化 section | 每个可独立启用/禁用 | 仅 tools + soul | 仅 tools + soul |
| 动态注册机制 | `resolvedDynamicSections` | ❌ 硬编码 | ❌ 硬编码 |

---

## 二、六维对比分析

### 1. 通用性 — 是否对任务足够泛化

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| 身份定义 | "交互式智能体，帮助用户处理软件工程任务" — 泛化但明确 | "autonomous AI coding agent with expert-level knowledge" — 过于绝对 | 🟡 |
| 任务范围 | 有独立的 `DoingTasks` section 区分"编写代码"vs"回答问题"vs"分析"等不同任务类型 | 混在 identity 中一句话带过 | 🔴 |
| 非编码场景 | 有 `OutputStyle` 配置，可切换为"回答问题"模式 | Default 和 Coding 两种 style 硬编码，无运行时切换 | 🟡 |

**具体问题**：

1. **identity.rs** 中 `"expert-level knowledge across all programming languages"` 过于绝对，容易导致模型在不熟悉的语言上过度自信
2. 缺少对不同任务类型的分类指导。Claude Code 的 `DoingTasks` section 明确区分了"写代码"、"回答问题"、"分析代码"等场景的不同行为策略
3. **rules.rs** 中的 "Subagent Delegation" 部分（~40 行）放在通用 rules 中不合适——只有在有 subagent 工具时才需要

### 2. 专用性 — 是否对特殊任务有充分指导

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| 文件操作安全 | 独立的 `ActionsSection` 定义文件操作确认边界（哪些操作需要确认、哪些可以自动执行） | 无。只在 reminders 中有一句 "Safety first" | 🔴 |
| 工具结果摘要 | `SummarizeToolResults` section 告知模型"旧工具结果会被自动清除" | 无。模型不知道上下文会被截断 | 🔴 |
| Hooks 处理 | 独立的 `HooksSection` 指导如何处理用户配置的 hooks 反馈 | 无（虽然 Daedalus 有 hooks 系统，但 prompt 中未提及） | 🟡 |
| 防 prompt 注入 | `SystemSection` 中有明确的防注入指令 | 无 | 🟡 |
| 上下文压缩感知 | 模型被告知"对话通过自动摘要拥有无限上下文" | 无。模型不知道 sliding window 和 consolidation 的存在 | 🔴 |

**关键缺失**：

Daedalus 有 hooks 系统、有 sliding window memory、有 tool result truncation，但 **prompt 中完全没有告知模型这些机制的存在**。模型无法配合这些机制工作。例如：

- 模型不知道旧的工具结果可能被截断，可能会引用已被清除的内容
- 模型不知道 hooks 反馈的含义，可能忽略或误解 hooks 输出
- 模型不知道对话会被自动摘要，可能在长对话中重复已总结的内容

### 3. 清晰性 — 是否足够清晰

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| 工具选择 | 按工具分类给出具体的 when-to-use / when-NOT-to-use 指导 | 只有泛化的 "Right tool for the job" 列表，没有 when-NOT-to-use | 🟡 |
| 规划策略 | 明确的任务复杂度判断标准 + 对应行为 | 有 Adaptive Planning，但判断标准模糊（"involves 5+ files" 是唯一量化标准） | 🟡 |
| 代码变更 | 有明确的确认边界（什么时候自动执行、什么时候需要确认） | "Minimal disruption" 和 "Verify your work" 过于抽象 | 🟡 |
| 语言一致性 | 独立的 `LanguageSection`，用变量注入具体语言偏好 | 在 reminders 和 response_style 中**重复出现两次**，且没有变量化 | 🔴 |

**具体问题**：

1. **语言一致性规则重复**：在 `coding/sections/reminders.rs`（L8）和 Default style 的 `sections/response_style.rs`（L6）+ `sections/reminders.rs` 中重复出现，措辞不同
2. **工具引用错误**：`coding/sections/tools.rs` 中 "Right tool for the job" 列出了 `codebase search` 和 `view code item`，但这些是外部平台注入的工具，**不是 Daedalus 内置工具**——prompt 中引用了不存在的工具名
3. 规划策略中 "involves 5+ files" 是唯一的量化标准，缺少 LOC、模块数等其他维度

### 4. 模块化 — 是否足够模块化

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| Section 粒度 | 7 静态 + 11+ 动态 = 18+ 个独立 section，每个职责单一 | 5 静态 + 3 动态 = 8 个 section，部分 section 职责过重 | 🟡 |
| 条件化 | 每个 section 可独立启用/禁用，通过 `filter(s => s !== "")` 过滤空段 | 只有 tools 和 soul 是条件化的，其他 section 始终存在 | 🟡 |
| 两种 style 的复用 | N/A（Claude Code 只有一种 style） | Default 和 Coding 有大量重复内容（reminders、response_style 各有两份） | 🔴 |
| 动态 section 注册 | `resolvedDynamicSections` 支持运行时注册新 section | 硬编码在 `build()` 方法中，无法运行时扩展 | 🟡 |

**关键问题**：

1. **`rules.rs` 是一个 God Section**（~150 行），混合了 5 个不同关注点：
   - Core Operating Principles
   - Making Code Changes
   - Search Strategy
   - Communication Style
   - Subagent Delegation

   Claude Code 将这些拆分为独立的 section（`DoingTasks`、`Actions`、`ToneAndStyle` 等）

2. **两套 reminders 维护成本高**：
   - `src/prompt/sections/reminders.rs` — Default style 的 reminders
   - `src/prompt/coding/sections/reminders.rs` — Coding style 的 reminders

   内容高度重叠但措辞不同，修改一处容易忘记同步另一处

3. **缺少 section 注册机制**：如果要添加新的 prompt section（如 hooks 指导、上下文压缩感知），需要修改 `build()` 方法本身

### 5. 简洁性 — 是否足够简洁

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| 总 token 量 | 静态区 ~2000 tokens，动态区按需 | Coding style 静态区 ~3500 tokens（估算） | 🟡 |
| 重复内容 | 极少重复，每个概念只出现一次 | 多处重复（见下表） | 🔴 |
| 条件裁剪 | 无工具时整个 tools section 不生成；无 hooks 时 hooks section 不生成 | 无工具时 tools section 不生成，但 rules 中的工具相关内容仍然存在 | 🟡 |
| 输出效率 | 独立的 `OutputEfficiency` section，用极简措辞指导简洁输出 | 分散在 response_style 和 communication style 中 | 🟡 |

**重复规则清单**：

| 规则 | 出现位置 | 次数 |
|------|---------|:----:|
| 语言一致性 | `coding/sections/reminders.rs` (L8), `sections/response_style.rs` (L6), `sections/reminders.rs` | 2-3 |
| 不要编造 | `coding/sections/reminders.rs` (L1, L3), `coding/sections/identity.rs` ("Honesty about limitations") | 3 |
| 简洁回复 | `sections/response_style.rs` ("Clear and concise"), `coding/sections/rules.rs` ("Concise responses") | 2 |
| 不暴露工具名 | `coding/sections/tools.rs` ("Never mention tool names"), `sections/reminders.rs` ("Never expose raw tool errors") | 2 |

每处重复约浪费 30-50 tokens，总计浪费约 **150-200 tokens**。

### 6. 布局 — 布局是否足够清晰

| 维度 | Claude Code | Daedalus | 差距 |
|------|------------|---------|:----:|
| 缓存边界 | `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 明确分割 | ✅ 同样有缓存边界标记 | ✅ |
| 结构标记 | 不使用 XML 标签包裹 section（直接用 `#` 标题分隔） | Coding style 用 XML 标签（`<identity>`, `<tools>`, `<rules>`, `<critical_reminders>`, `<environment>`） | ✅ |
| Section 排序 | 身份 → 安全 → 任务 → 操作 → 工具 → 风格 → 效率 → [边界] → 环境 → ... | 身份 → 人格 → 工具 → 规则 → 提醒 → [边界] → 环境 → 规则 → 记忆 | 🟡 |
| 安全规则位置 | 安全规则在第 2 位（`SystemSection`），利用首因效应 | 安全规则在最后（`critical_reminders`），利用近因效应 | 🟡 |

**布局分析**：

- Claude Code 将安全规则放在**前面**（首因效应 + 缓存友好），Daedalus 放在**后面**（近因效应）。两种策略各有道理，但 Daedalus 的 `critical_reminders` 在缓存边界**之前**（静态区），这是正确的设计
- `rules.rs` 中的 section 内部缺少清晰的层级结构。Claude Code 的每个 section 都是扁平的、单一职责的；Daedalus 的 `<rules>` 内部有 5 个 `##` 子标题，实际上是 5 个独立 section 被强行塞进一个标签
- Default style 和 Coding style 的缓存边界标记不一致：`<!-- CACHE_BOUNDARY -->` vs `<!-- SYSTEM_PROMPT_DYNAMIC_BOUNDARY -->`

---

## 三、改进计划

### P0 — 高收益、低成本

#### P0-1：消除重复规则 ✅ 已完成（2026-05-15）

**现状**：语言一致性、不编造、简洁回复等规则在多个 section 中重复出现，浪费 ~200 tokens 并可能因措辞不同导致歧义。

**改进方案**：
- 语言一致性：仅保留在 `critical_reminders` 中（利用近因效应确保遵守）
- 不编造信息：仅保留在 `critical_reminders` 中的 "Never fabricate information"
- 简洁回复：仅保留在 `rules.rs` 的 Communication Style 中
- 不暴露工具名：仅保留在 `tools.rs` 的 Important Constraints 中

**涉及文件**：
- `src/prompt/coding/sections/reminders.rs`
- `src/prompt/sections/response_style.rs`
- `src/prompt/sections/reminders.rs`
- `src/prompt/coding/sections/tools.rs`

**预期收益**：节省 ~200 tokens/请求，消除歧义风险
**工作量**：0.5 天

---

#### P0-2：添加上下文压缩感知 ✅ 已完成（2026-05-15）

**现状**：Daedalus 有 sliding window memory 和 tool result truncation 机制，但 prompt 中完全没有告知模型这些机制的存在。模型无法配合工作。

**改进方案**：在动态区添加新 section `<context_management>`：

```
<context_management>
This conversation uses automatic context management:
- Old tool results may be summarized or removed to stay within context limits.
  Do not reference specific tool outputs from many rounds ago — re-read if needed.
- Conversation history is automatically consolidated via sliding window.
  You have effectively unlimited context through automatic summarization.
- If you notice missing context, use tools to re-gather the information
  rather than guessing from memory.
</context_management>
```

**涉及文件**：
- `src/prompt/coding/mod.rs` — 在 `build()` 中添加新 section
- 新建 `src/prompt/coding/sections/context_management.rs`

**预期收益**：模型能配合 sliding window 工作，减少引用已清除内容的错误
**工作量**：0.5 天

---

#### P0-3：修正工具引用错误 ✅ 已完成（2026-05-15）

**现状**：`coding/sections/tools.rs` 中 "Right tool for the job" 列出了 `codebase search` 和 `view code item`，但这些不是 Daedalus 内置工具，是外部平台注入的工具名。

**改进方案**：
- 将工具选择指导改为基于工具类别而非具体工具名
- 或者动态生成工具选择指导（基于实际可用的工具列表）

**当前代码**（`coding/sections/tools.rs`）：
```
3. **Right tool for the job**:
   - Exact text/symbol lookup → grep/ripgrep tools
   - Semantic understanding → codebase search
   - Known file path → read file directly
   - Need to understand a function → view code item
   - Multiple edits to one file → multi-edit tools
```

**建议修改为**：
```
3. **Right tool for the job**:
   - Exact text/symbol lookup → grep_search
   - Known file path → read_file directly
   - Multiple edits to one file → multi_edit
   - File discovery → search_files or list_directory
   - System commands → bash
```

**涉及文件**：`src/prompt/coding/sections/tools.rs`
**预期收益**：消除幻觉引导，工具名与实际可用工具一致
**工作量**：0.5 天

---

### P1 — 中等收益

#### P1-4：拆分 rules.rs God Section ✅ 已完成（2026-05-15）

**现状**：`coding/sections/rules.rs` 是一个 ~150 行的 God Section，混合了 5 个不同关注点。

**改进方案**：拆分为 5 个独立 section：

| 新 Section | 来源 | 条件化 |
|-----------|------|--------|
| `core_principles.rs` | Core Operating Principles | 始终存在 |
| `code_changes.rs` | Making Code Changes | 仅有编辑工具时 |
| `search_strategy.rs` | Search Strategy | 仅有搜索工具时 |
| `communication.rs` | Communication Style | 始终存在 |
| `delegation.rs` | Subagent Delegation | 仅有 spawn_subagent 工具时 |

**关键收益**：
- Subagent Delegation（~40 行）在无 subagent 工具时不生成，节省 tokens
- 每个 section 可独立测试和维护
- 符合 Claude Code 的单一职责设计

**涉及文件**：
- 删除 `src/prompt/coding/sections/rules.rs`
- 新建 5 个文件
- 修改 `src/prompt/coding/mod.rs` 的 `build()` 方法
- 修改 `src/prompt/coding/sections/mod.rs`

**预期收益**：更好的模块化 + 条件化节省 tokens
**工作量**：1 天

---

#### P1-5：添加任务分类指导 ✅ 已完成（2026-05-15）

**现状**：缺少对不同任务类型的分类指导，模型对"写代码"和"回答问题"使用相同的行为策略。

**改进方案**：参考 Claude Code 的 `DoingTasks` section，新建 `task_strategy.rs`：

```
<task_strategy>
Adapt your behavior based on the task type:

**Writing code** (creating/modifying files):
- Gather full context before editing (read files, check imports, understand patterns)
- Make changes, then verify (check for errors, run tests if available)
- Show the result, not the process

**Answering questions** (explaining, analyzing):
- Answer directly and concisely
- Use code examples only when they clarify the explanation
- Cite specific files/lines when referencing the codebase

**Debugging** (fixing errors, investigating issues):
- Reproduce the issue first (read error messages, check logs)
- Trace the root cause before applying fixes
- Verify the fix resolves the original issue

**Exploring/reviewing** (code review, architecture analysis):
- Start broad, then drill into specifics
- Use structured output (severity levels, categories)
- Provide actionable recommendations, not just observations
</task_strategy>
```

**涉及文件**：
- 新建 `src/prompt/coding/sections/task_strategy.rs`
- 修改 `src/prompt/coding/mod.rs`

**预期收益**：提升非编码任务（问答、分析）的输出质量
**工作量**：1 天

---

#### P1-6：添加文件操作安全边界 ✅ 已完成（2026-05-15）

**现状**：无文件操作安全边界定义，只在 reminders 中有一句 "Safety first"。

**改进方案**：参考 Claude Code 的 `ActionsSection`，在 `code_changes.rs`（拆分后）中添加：

```
### File Operation Safety

- **Auto-execute** (no confirmation needed): reading files, searching, listing directories
- **Execute with caution**: creating new files, editing existing files (use edit_file over write_file)
- **Require extra care**: deleting files, overwriting files with write_file, running destructive bash commands
- Prefer `edit_file` / `multi_edit` over `write_file` — surgical edits are safer than full overwrites
- Before deleting or overwriting, verify the file path is correct
```

**涉及文件**：`src/prompt/coding/sections/rules.rs`（或拆分后的 `code_changes.rs`）
**预期收益**：减少误操作风险
**工作量**：0.5 天

---

#### P1-7：统一两种 style 的 reminders ✅ 已完成（2026-05-15）

**现状**：Default 和 Coding style 各有一份 reminders，内容高度重叠但措辞不同。

**改进方案**：
- 将 reminders 提取到 `src/prompt/shared/reminders.rs`
- 通过参数控制差异（如 Coding style 的 "No hallucinated code" 在 Default style 中不需要）
- 两种 style 共享同一份 reminders 函数

**涉及文件**：
- 新建 `src/prompt/shared/reminders.rs`（或直接复用 `sections/reminders.rs`）
- 修改 `src/prompt/coding/sections/reminders.rs` 改为调用共享函数
- 修改 `src/prompt/coding/mod.rs`

**预期收益**：减少维护成本，确保规则一致性
**工作量**：0.5 天

---

### P2 — 长期架构优化

#### P2-8：动态 section 注册机制 ✅ 已完成（2026-05-15）

**现状**：所有 section 硬编码在 `build()` 方法中，无法运行时扩展。

**改进方案**：
- 定义 `PromptSection` trait：`fn build(&self) -> Option<String>`
- `CodingPromptBuilder` 持有 `Vec<Box<dyn PromptSection>>`
- 支持运行时注册新 section（如 hooks 指导、MCP 工具扩展指令）
- 保持 section 排序的确定性（通过 priority 字段）

```rust
trait PromptSection: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> u32;  // Lower = earlier in prompt
    fn is_static(&self) -> bool; // true = before cache boundary
    fn build(&self, ctx: &PromptContext) -> Option<String>;
}
```

**预期收益**：可扩展性，新功能（hooks、MCP 指令等）可通过注册 section 实现
**工作量**：2 天

---

#### P2-9：语言偏好变量化 ✅ 已完成（2026-05-15）

**现状**：语言一致性规则硬编码为"respond in the same language as the user"，没有具体的语言偏好注入。

**改进方案**：参考 Claude Code 的 `LanguageSection`：
- 在 `AgentConfig` 中添加 `language_preference: Option<String>` 字段
- 在动态区注入具体语言偏好：`"Always respond in {language_preference}"`
- 支持通过 CLI 参数或配置文件设置

**涉及文件**：
- `src/config/agent_config.rs`
- 新建 `src/prompt/coding/sections/language.rs`
- `src/prompt/coding/mod.rs`

**预期收益**：更精确的语言控制，支持多语言场景
**工作量**：0.5 天

---

#### P2-10：弱化绝对表述 ✅ 已完成（2026-05-15）

**现状**：identity 中 `"expert-level knowledge across all programming languages"` 过于绝对。

**改进方案**：改为更谦逊的表述：

```diff
- You are {name}, an autonomous AI coding agent with expert-level knowledge across
- all programming languages, frameworks, design patterns, and best practices.
+ You are {name}, an autonomous AI coding agent with broad knowledge across
+ programming languages, frameworks, design patterns, and best practices.
```

**涉及文件**：`src/prompt/coding/sections/identity.rs`
**预期收益**：减少模型在不熟悉领域的过度自信
**工作量**：0.5 天

---

## 四、总结评分

| 维度 | Daedalus 评分 | Claude Code 评分 | 主要差距 |
|------|:---:|:---:|------|
| 通用性 | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 缺任务分类指导 |
| 专用性 | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 缺安全边界、上下文压缩感知、hooks 指导 |
| 清晰性 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 工具引用错误、规划标准模糊 |
| 模块化 | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | rules God Section、两套 reminders |
| 简洁性 | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 多处重复规则 |
| 布局 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 缓存边界设计良好，但 section 内部层级混乱 |

### 实施优先级矩阵

```
                    高收益
                      │
         P0-2         │         P1-4
     (上下文压缩感知)  │      (拆分 rules)
                      │
         P0-1         │         P1-5
      (消除重复)       │     (任务分类指导)
                      │
  ────────────────────┼────────────────────
                      │
         P0-3         │         P2-8
     (修正工具引用)    │    (动态注册机制)
                      │
         P1-7         │         P2-9
     (统一 reminders)  │    (语言偏好变量化)
                      │
                    低收益
        低成本                    高成本
```

### 预估总工作量

| 优先级 | 项数 | 总工作量 | 建议时间窗口 |
|:------:|:----:|:-------:|:-----------:|
| P0 | 3 项 | 1.5 天 | 本周 |
| P1 | 4 项 | 3 天 | 下周 |
| P2 | 3 项 | 3 天 | 下月 |
| **总计** | **10 项** | **7.5 天** | — |

---

*变更历史*

| 日期 | 版本 | 变更 | 来源 |
|------|------|------|------|
| 2026-05-15 | v1.0 | 初始创建：6 维对比分析 + 10 项改进计划 | Claude Code 源码对比分析 |
| 2026-05-15 | v1.1 | P0 全部完成：消除重复规则 + 上下文压缩感知 + 修正工具引用 | 代码实现 |
| 2026-05-15 | v1.2 | P1 全部完成：拆分 rules God Section + 任务分类指导 + 文件操作安全 + 统一 reminders | 代码实现 |
| 2026-05-15 | v1.3 | P2 全部完成：动态 section 注册（轻量版 extra_sections）+ 语言偏好变量化 + 弱化绝对表述 | 代码实现 |
