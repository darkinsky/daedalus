# Long Horizon Agent 详细技术方案

> **版本**: v2.0
> **日期**: 2026-05-16
> **基于**: 2025-2026 年前沿论文与工程实践
> **目标**: 为 Daedalus 提供分期可落地的 Long Horizon Agent 优化技术方案
> **方法**: 源码级架构分析 + 论文/开源实现参考

---

## 〇、背景与现状分析

### 0.1 为什么 Long Horizon 是关键瓶颈

METR（2025.03）的研究揭示了核心事实：

| 人类任务时长 | 当前前沿模型成功率 |
|:----------:|:-----------------:|
| < 4 分钟 | ~100% |
| 1 小时 | ~50%（Claude 3.7 Sonnet） |
| > 4 小时 | < 10% |

- 50% 成功率时间地平线呈指数增长，**倍增周期约 7 个月**
- 瓶颈不在单步智能，而在**将长动作序列串联起来的能力**

> **参考**: METR — *Measuring AI Ability to Complete Long Tasks* (2025.03)

### 0.2 Daedalus 现有架构基线

**上下文压缩（两级架构）**：
- Level 1: `tool_loop/truncation.rs` — 三级渐进式工具历史截断（moderate → aggressive → micro），工具密度感知（grep_search 1.5x, read_file 0.8x, edit_file 0.5x）
- Level 1b: `tool_loop/context_pressure.rs` — `ContextHealth` 四级健康度（Healthy → Mild → Moderate → Severe），>90% 时强制终结
- Level 2: `memory/sliding_window/compact_ops.rs` — LLM 驱动的增量会话压缩，`[Previous conversation context --]` 边界标记，93%/97% 自动触发

**记忆系统（6 种策略）**：
- `sliding_window/` — 默认策略，含 micro_compact + ReadOnlyCache
- `dynamic_cheatsheet/` — 动态速查表，7 文件实现
- `agentic/` — A-MEM 实现
- `ace/` — ACE 策略
- `wiki/` — Wiki 策略
- `mempalace/` — Memory Palace 策略

**Subagent 系统**：已有 orchestrator → specialist 架构，`.daedalus/agents/` 下可配置

**中间件管道**：`MemoryTurnMiddleware`（自动 compact 触发）、`TracingMiddleware` 等

---

## 分期总览

| 期次 | 时间 | 主题 | 核心交付 |
|:----:|:----:|------|---------|
| **Phase 1** | 第 1-3 周 | 持久化状态 + 深度压缩增强 | 项目状态持久化、深度衰减压缩、阶段性自检 |
| **Phase 2** | 第 4-7 周 | 分层记忆 + 计划缓存 | 子目标级记忆管理、计划模板缓存系统 |
| **Phase 3** | 第 8-11 周 | 记忆演化 + 自适应压缩 | A-MEM 增强、ACON 式压缩策略进化 |
| **Phase 4** | 第 12-15 周 | 自改进 + 记忆统一 | Prompt 自调优、记忆工具化 |

---

# Phase 1：持久化状态 + 深度压缩增强（第 1-3 周）

> 目标：让 Agent 在长任务中"不迷路、不遗忘"

## 1.1 项目状态持久化系统

### 参考来源
- OpenAI — *Run Long Horizon Tasks with Codex* (2026.02)
- AgentMemory — *Persistent Memory for AI Coding Agents* (2026.05, github.com/rohitg00/agentmemory)

### 1.1.1 设计概述

借鉴 Codex 四文件方案 + AgentMemory 的 Hook 生命周期，在 `.daedalus/` 下建立持久化项目状态：

```
.daedalus/
├── agents/             # 已有：agent 配置
├── state/              # 新增：项目状态持久化
│   ├── goal.md         # 当前目标 + 非目标 + 约束 + 验收标准
│   ├── plan.md         # 里程碑计划 + 验证命令 + 决策备注
│   ├── progress.md     # 实时进度 + 已完成里程碑摘要
│   └── decisions.md    # 架构决策日志（时间 + 原因 + 备选方案）
└── cache/              # 新增：计划缓存（Phase 2）
```

### 1.1.2 详细实现

#### 数据结构（`src/state/project_state.rs`）

```rust
/// 项目状态——持久化到 .daedalus/state/
pub struct ProjectState {
    pub goal: GoalSpec,
    pub plan: MilestonePlan,
    pub progress: ProgressLog,
    pub decisions: Vec<DecisionRecord>,
}

pub struct GoalSpec {
    pub objectives: Vec<String>,       // 要做什么
    pub non_objectives: Vec<String>,   // 明确不做什么
    pub constraints: Vec<String>,      // 硬约束
    pub done_when: Vec<String>,        // 验收标准
}

pub struct MilestonePlan {
    pub milestones: Vec<Milestone>,
    pub current_index: usize,
}

pub struct Milestone {
    pub id: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub verify_commands: Vec<String>,   // e.g. ["cargo test", "cargo clippy"]
    pub status: MilestoneStatus,        // Pending | InProgress | Done | Failed
    pub summary: Option<String>,        // 完成后的摘要（用于压缩）
}

pub struct DecisionRecord {
    pub timestamp: DateTime<Utc>,
    pub decision: String,
    pub rationale: String,
    pub alternatives_considered: Vec<String>,
}
```

#### 生命周期 Hook

| Hook 时机 | 动作 |
|----------|------|
| `/goal <desc>` 命令 | 创建 `goal.md`，LLM 解析为结构化 GoalSpec |
| 每个里程碑完成 | 运行 verify_commands → 更新 `progress.md` → 压缩为 1-2 句摘要 |
| 架构决策点 | 追加 `decisions.md` |
| compact 触发时 | 将 `goal.md` + `plan.md` 当前段注入为 preserved messages |
| 会话重启时 | 从 `.daedalus/state/` 恢复全部状态，注入系统 prompt |

#### 与 compact 的集成

在 `compact_ops.rs` 的 `run_compact()` 中增加逻辑：

```rust
// compact 时自动保留项目状态
fn build_preserved_context(state: &ProjectState) -> Vec<ChatMessage> {
    let mut preserved = vec![];

    // 1. 始终保留当前目标（精简版）
    preserved.push(system_msg(format!(
        "[Project Goal]\n{}\n[Current Milestone]\n{}",
        state.goal.summary(),           // 目标 + 约束的 1-段摘要
        state.plan.current_milestone(), // 当前里程碑详情
    )));

    // 2. 已完成里程碑只保留摘要
    for m in state.plan.completed_milestones() {
        preserved.push(system_msg(format!(
            "[Completed: {}] {}",
            m.id, m.summary.unwrap_or_default()
        )));
    }

    // 3. 关键决策记录
    for d in state.decisions.iter().rev().take(5) {
        preserved.push(system_msg(format!(
            "[Decision] {} — Reason: {}",
            d.decision, d.rationale
        )));
    }

    preserved
}
```

#### CLI 命令

```
/goal <description>          — 设定项目目标，LLM 自动生成 GoalSpec
/plan                        — 查看/编辑当前里程碑计划
/milestone done [summary]    — 标记当前里程碑完成
/progress                    — 查看整体进度
/decisions                   — 查看决策日志
```

### 1.1.3 验证标准

- [ ] compact 后目标和当前里程碑不丢失
- [ ] 会话重启后能恢复到之前的进度
- [ ] 里程碑验证命令自动运行并记录结果

---

## 1.2 深度衰减压缩

### 参考来源
- Anthropic — *Effective Context Engineering for AI Agents* (2025.09)
- Microsoft — *ACON* (2025.10) 中关于"上下文腐蚀"的分析

### 1.2.1 设计概述

**核心洞察**：上下文越长，注意力越稀释（Context Rot）。当前 Daedalus 的截断策略是"按轮次从旧到新"，但缺乏**按交互深度递增的压缩比**。

引入**深度衰减压缩（Depth-Decay Compression）**：距当前越远的消息，压缩越激进。

### 1.2.2 详细实现

#### 衰减函数

```rust
/// 计算消息的保留比例，基于其距当前轮次的"深度"
/// depth = 0 表示最近的消息，depth 越大越旧
fn retention_ratio(depth: usize, total_rounds: usize) -> f64 {
    // 分三个区间：
    // - 最近 20% 轮次：保留 100%（不压缩）
    // - 中间 40% 轮次：线性衰减 100% → 30%
    // - 最早 40% 轮次：固定 20%（仅保留关键信息）
    let ratio = depth as f64 / total_rounds as f64;

    if ratio < 0.2 {
        1.0  // 最近：全量保留
    } else if ratio < 0.6 {
        // 中间：线性衰减
        1.0 - (ratio - 0.2) / 0.4 * 0.7  // 1.0 → 0.3
    } else {
        0.2  // 最早：仅保留 20%
    }
}
```

#### 与现有三级截断的集成

修改 `truncation.rs` 中的 `truncate_tool_history()`：

```rust
fn truncate_tool_history(
    messages: &mut [ToolMessage],
    config: &TruncationConfig,
    context_pressure: &ContextHealth,
) {
    let total_rounds = messages.len();

    for (i, msg) in messages.iter_mut().enumerate() {
        let depth = total_rounds - 1 - i; // 0 = 最新
        let base_ratio = retention_ratio(depth, total_rounds);

        // 叠加 context pressure 调整
        let pressure_factor = match context_pressure.severity {
            Healthy => 1.0,
            Mild => 0.8,
            Moderate => 0.6,
            Severe => 0.4,
        };

        let final_ratio = base_ratio * pressure_factor;
        let target_tokens = (msg.token_count as f64 * final_ratio) as usize;

        // 应用工具类型权重（已有逻辑）
        let tool_weight = tool_type_weight(&msg.tool_name);
        let adjusted_target = (target_tokens as f64 * tool_weight) as usize;

        truncate_message(msg, adjusted_target);
    }
}
```

#### 针对不同消息类型的差异化处理

```rust
/// 深度衰减对不同类型消息的处理策略
enum CompressionStrategy {
    /// 工具输出：head+tail 截断（已有），但按深度缩减保留量
    ToolOutput { head_ratio: f64, tail_ratio: f64 },

    /// 用户消息：永不压缩前 3 条，之后按深度衰减
    UserMessage { min_preserve: usize },

    /// Assistant 消息：保留思考链的关键步骤，压缩中间推理
    AssistantMessage { preserve_conclusions: bool },

    /// 系统消息 (preserved)：永不压缩
    SystemPreserved,
}

fn strategy_for_depth(msg_type: MessageType, depth: usize) -> CompressionStrategy {
    match msg_type {
        MessageType::ToolOutput => ToolOutput {
            head_ratio: if depth < 3 { 0.6 } else { 0.3 },
            tail_ratio: if depth < 3 { 0.4 } else { 0.1 },
        },
        MessageType::User => UserMessage {
            min_preserve: if depth < 5 { usize::MAX } else { 200 },
        },
        MessageType::Assistant => AssistantMessage {
            preserve_conclusions: depth < 10,
        },
        MessageType::System => SystemPreserved,
    }
}
```

### 1.2.3 验证标准

- [ ] 在 200+ 轮对话中，token 使用量增长曲线从线性变为对数
- [ ] 压缩后最近 20% 消息仍完整可用
- [ ] 不引起关键信息丢失（通过 compact 前后的 LLM 评测验证）

---

## 1.3 阶段性自检机制

### 参考来源
- 智谱 AI — *GLM-5.1: Towards Long-Horizon Tasks* (2026.04)
- OpenAI — *Run Long Horizon Tasks with Codex* (2026.02)

### 1.3.1 设计概述

GLM-5.1 通过"楼梯式训练"实现 1700 步连续工具调用。Daedalus 不训练模型，但可以在系统层面实现**周期性自检（Periodic Self-Check）**：每 N 步强制 Agent 回顾目标和进度。

### 1.3.2 详细实现

#### 自检触发器（修改 `tool_loop` 主循环）

```rust
const SELF_CHECK_INTERVAL: usize = 15; // 每 15 个 tool_use 触发一次
const SELF_CHECK_INTERVAL_SEVERE: usize = 8; // 压力大时更频繁

fn should_self_check(
    round: usize,
    context_health: &ContextHealth,
    last_check_round: usize,
) -> bool {
    let interval = match context_health.severity {
        Severe | Moderate => SELF_CHECK_INTERVAL_SEVERE,
        _ => SELF_CHECK_INTERVAL,
    };
    round - last_check_round >= interval
}
```

#### 自检 Prompt（注入到下一轮的系统消息中）

```rust
const SELF_CHECK_PROMPT: &str = r#"
[Periodic Self-Check — Round {round}]
Before proceeding, briefly assess:
1. **Goal alignment**: Is your current action still aligned with the project goal?
2. **Progress**: What milestones have you completed? What's the current milestone?
3. **Context health**: Are you losing track of important earlier context?
4. **Scope creep**: Are you working on something that wasn't in the plan?

If you detect drift, state it explicitly and correct course.
If context is degrading, consider writing key state to project notes.
"#;
```

#### 自检结果处理

```rust
enum SelfCheckOutcome {
    OnTrack,                          // 继续执行
    DriftDetected { correction: String }, // 需要纠偏
    ContextDegraded,                  // 建议触发 compact
    ScopeCreep { offending_action: String }, // 回退到计划
}

// 自检后的动作
fn handle_self_check(outcome: SelfCheckOutcome, state: &mut ProjectState) {
    match outcome {
        OnTrack => { /* no-op, 继续 */ },
        DriftDetected { correction } => {
            // 注入纠偏指令
            inject_system_message(&format!(
                "[Course Correction] {correction}\nResume from: {}",
                state.plan.current_milestone().description
            ));
        },
        ContextDegraded => {
            // 1. 将关键状态写入 progress.md
            state.progress.snapshot();
            // 2. 触发 compact
            trigger_compact(CompactReason::SelfCheckContextDegraded);
        },
        ScopeCreep { offending_action } => {
            // 记录并回退
            state.decisions.push(DecisionRecord {
                decision: format!("Reverted scope creep: {offending_action}"),
                rationale: "Not in current milestone plan".into(),
                ..Default::default()
            });
        },
    }
}
```

### 1.3.3 验证标准

- [ ] 50+ 轮长任务中，自检至少触发 3 次
- [ ] 自检不引入超过 200 token 的额外开销
- [ ] 存在目标偏离时能检测并纠正

---

# Phase 2：分层记忆 + 计划缓存（第 4-7 周）

> 目标：让 Agent 在长任务中"按目标管理记忆、复用历史经验"

## 2.1 子目标级工作记忆管理

### 参考来源
- *HiAgent: Hierarchical Working Memory Management* (ACL 2025)
- Anthropic — *Effective Context Engineering for AI Agents* (2025.09) — 子 Agent 摘要回传

### 2.1.1 设计概述

HiAgent 的核心洞察：**以子目标为粒度管理工作记忆**——已完成的子目标压缩为摘要，只有当前子目标保留完整上下文。

对应到 Daedalus：将 Phase 1 的 Milestone 与 compact 深度集成，实现**子目标感知的上下文压缩**。

### 2.1.2 详细实现

#### 子目标追踪器（`src/memory/subgoal_tracker.rs`）

```rust
/// 子目标追踪器——跟踪消息与子目标的归属关系
pub struct SubgoalTracker {
    /// 当前活跃的子目标
    pub active_subgoal: Option<SubgoalId>,
    /// 消息 → 子目标的映射
    pub message_subgoal_map: HashMap<MessageId, SubgoalId>,
    /// 子目标状态
    pub subgoals: IndexMap<SubgoalId, SubgoalState>,
}

pub struct SubgoalState {
    pub id: SubgoalId,
    pub description: String,
    pub status: SubgoalStatus,       // Active | Completed | Abandoned
    pub message_range: (usize, usize), // 消息索引范围 [start, end)
    pub summary: Option<String>,     // 完成后的 LLM 生成摘要
    pub key_artifacts: Vec<String>,  // 关键产出（文件路径、函数名等）
}
```

#### 子目标感知的 compact 策略

```rust
/// 替换原有的"从旧到新全量压缩"，改为"按子目标分块压缩"
fn subgoal_aware_compact(
    messages: &[ChatMessage],
    tracker: &SubgoalTracker,
    current_budget: usize,
) -> CompactOutput {
    let mut result_messages = vec![];
    let mut used_tokens = 0;

    for subgoal in tracker.subgoals.values() {
        match subgoal.status {
            SubgoalStatus::Completed => {
                // 已完成子目标 → 替换为 1-2 句摘要
                let summary = subgoal.summary.as_ref().unwrap_or(&subgoal.description);
                result_messages.push(system_msg(format!(
                    "[Completed Subgoal: {}]\n{}\nArtifacts: {}",
                    subgoal.id,
                    summary,
                    subgoal.key_artifacts.join(", ")
                )));
                // 摘要通常 < 100 tokens
                used_tokens += estimate_tokens(summary);
            },
            SubgoalStatus::Active => {
                // 当前子目标 → 保留完整消息（应用深度衰减）
                let subgoal_messages = &messages[subgoal.message_range.0..subgoal.message_range.1];
                for msg in subgoal_messages {
                    if used_tokens < current_budget * 0.8 {
                        result_messages.push(msg.clone());
                        used_tokens += msg.token_count;
                    }
                }
            },
            SubgoalStatus::Abandoned => {
                // 放弃的子目标 → 只保留决策记录
                result_messages.push(system_msg(format!(
                    "[Abandoned: {}] Reason: {}",
                    subgoal.id,
                    subgoal.summary.as_deref().unwrap_or("N/A")
                )));
            },
        }
    }

    CompactOutput { messages: result_messages, tokens_used: used_tokens }
}
```

#### 子目标边界检测

```rust
/// 从 LLM 响应中检测子目标转换信号
/// 基于关键模式匹配 + 里程碑状态变化
fn detect_subgoal_transition(
    assistant_msg: &str,
    plan: &MilestonePlan,
) -> Option<SubgoalTransition> {
    // 模式 1：显式的里程碑完成声明
    if assistant_msg.contains("milestone") && assistant_msg.contains("complete") {
        return Some(SubgoalTransition::MilestoneCompleted);
    }

    // 模式 2：Plan.md 中的里程碑状态变化
    if plan.just_transitioned() {
        return Some(SubgoalTransition::PlanDriven);
    }

    // 模式 3：LLM 生成的子目标标记（通过 prompt 引导）
    // 在系统 prompt 中要求 Agent 在切换子目标时输出 [SUBGOAL_SWITCH]
    if assistant_msg.contains("[SUBGOAL_SWITCH]") {
        return Some(SubgoalTransition::ExplicitMarker);
    }

    None
}
```

#### 子目标完成时的摘要生成

```rust
const SUBGOAL_SUMMARY_PROMPT: &str = r#"
Summarize the completed subgoal in 2-3 sentences. Include:
1. What was accomplished
2. Key files/functions created or modified
3. Any important decisions made

Subgoal: {subgoal_description}
Messages from this subgoal:
{messages}
"#;

async fn generate_subgoal_summary(
    subgoal: &SubgoalState,
    messages: &[ChatMessage],
    llm: &dyn LlmClient,
) -> String {
    let prompt = SUBGOAL_SUMMARY_PROMPT
        .replace("{subgoal_description}", &subgoal.description)
        .replace("{messages}", &format_messages_for_summary(messages));

    llm.complete(&prompt, CompletionConfig {
        max_tokens: 150,
        temperature: 0.3,
        ..Default::default()
    }).await
}
```

### 2.1.3 验证标准

- [ ] 含 5+ 子目标的长任务中，compact 后仅当前子目标保留完整
- [ ] 已完成子目标摘要不超过 100 tokens
- [ ] 子目标切换不丢失跨子目标的依赖信息

---

## 2.2 计划模板缓存系统

### 参考来源
- *Agentic Plan Caching: Test-Time Memory* (NeurIPS 2025, arXiv:2506.14852)
- Stanford — Qizheng Zhang, Michael Wornow, Gerry Wan, Kunle Olukotun

### 2.2.1 设计概述

APC 的核心流程（已验证：成本降 50%，延迟降 27%）：

```
查询 → 关键词提取(小模型) → 缓存查找(O(1)精确匹配)
    ├── 命中 → 小模型根据模板适配 → 执行
    └── 未命中 → 大模型规划 → 执行 → 提取模板 → 存入缓存
```

适配到 Daedalus：不是替换 Skill 系统，而是在 Skill 之上增加**自动化的执行模式缓存层**。

### 2.2.2 详细实现

#### 缓存存储（`src/cache/plan_cache.rs`）

```rust
/// 计划模板缓存
pub struct PlanCache {
    /// 关键词 → 模板的精确匹配映射
    templates: HashMap<String, PlanTemplate>,
    /// 持久化路径：.daedalus/cache/plans/
    storage_path: PathBuf,
}

pub struct PlanTemplate {
    pub keyword: String,
    pub steps: Vec<PlanStep>,
    pub success_count: usize,   // 成功复用次数
    pub fail_count: usize,      // 失败次数（用于淘汰）
    pub created_at: DateTime<Utc>,
    pub last_used: DateTime<Utc>,
}

pub struct PlanStep {
    pub action_type: String,     // "read_file", "edit_file", "search", "test"
    pub description: String,     // 泛化描述（不含具体文件名/函数名）
    pub expected_outcome: String,
}
```

#### 关键词提取

```rust
/// 使用小模型从用户请求中提取高层意图关键词
/// 参考 APC 的 prompt：提取与问题细节无关的"task/keyword"
const KEYWORD_EXTRACTION_PROMPT: &str = r#"
Extract the high-level task keyword from this request.
The keyword must be independent from problem-specific details
(file names, function names, variable names, etc.).

Examples:
- "Add unit tests for the auth module" → "add unit tests"
- "Refactor UserService to use dependency injection" → "refactor to dependency injection"
- "Fix the null pointer exception in checkout flow" → "fix null pointer exception"

Request: {request}
Keyword:
"#;

async fn extract_keyword(request: &str, llm: &dyn LlmClient) -> String {
    // 使用低成本模型（如果可用）
    llm.complete(
        &KEYWORD_EXTRACTION_PROMPT.replace("{request}", request),
        CompletionConfig { max_tokens: 20, temperature: 0.0, ..Default::default() },
    ).await.trim().to_lowercase()
}
```

#### 模板生成（从成功执行中提取）

```rust
/// 在 tool_loop 成功完成后，从执行日志提取计划模板
const TEMPLATE_EXTRACTION_PROMPT: &str = r#"
Extract a reusable plan template from this execution log.
Remove ALL problem-specific details (file paths, function names,
variable names, specific numbers). Keep only the general workflow structure.

Return a JSON array of steps, each with:
- "action_type": the type of action (read_file, edit_file, search, test, etc.)
- "description": what this step does (generalized)
- "expected_outcome": what to expect after this step

Execution log:
{log}
"#;

async fn extract_template(
    keyword: &str,
    execution_log: &[ToolUseRecord],
    llm: &dyn LlmClient,
) -> PlanTemplate {
    // 第一步：规则过滤——去除 verbose 输出，只保留工具名+简要结果
    let filtered_log = execution_log.iter()
        .map(|r| format!("[{}] {} → {}", r.tool_name, r.input_summary, r.output_summary))
        .collect::<Vec<_>>()
        .join("\n");

    // 第二步：LLM 泛化——去除所有具体细节
    let template_json = llm.complete(
        &TEMPLATE_EXTRACTION_PROMPT.replace("{log}", &filtered_log),
        CompletionConfig { max_tokens: 500, temperature: 0.2, ..Default::default() },
    ).await;

    let steps: Vec<PlanStep> = serde_json::from_str(&template_json).unwrap_or_default();
    PlanTemplate {
        keyword: keyword.to_string(),
        steps,
        success_count: 1,
        fail_count: 0,
        created_at: Utc::now(),
        last_used: Utc::now(),
    }
}
```

#### 缓存命中时的模板适配

```rust
/// 缓存命中后，将通用模板适配为具体任务的执行计划
const TEMPLATE_ADAPT_PROMPT: &str = r#"
You have a reusable plan template for the task type "{keyword}".
Adapt it to the specific request below.

Template steps:
{template_steps}

Current request: {request}
Current project context: {context}

Generate a concrete execution plan with specific file paths,
function names, and other details from the current request.
"#;

async fn adapt_template(
    template: &PlanTemplate,
    request: &str,
    context: &str,
    llm: &dyn LlmClient,
) -> Vec<ConcreteStep> {
    let template_steps = template.steps.iter()
        .enumerate()
        .map(|(i, s)| format!("{}. [{}] {}", i + 1, s.action_type, s.description))
        .collect::<Vec<_>>()
        .join("\n");

    let result = llm.complete(
        &TEMPLATE_ADAPT_PROMPT
            .replace("{keyword}", &template.keyword)
            .replace("{template_steps}", &template_steps)
            .replace("{request}", request)
            .replace("{context}", context),
        CompletionConfig { max_tokens: 500, ..Default::default() },
    ).await;

    parse_concrete_steps(&result)
}
```

#### 缓存淘汰策略

```rust
impl PlanCache {
    /// 淘汰策略：失败率 > 30% 的模板自动删除
    fn evict_stale(&mut self) {
        self.templates.retain(|_, t| {
            let total = t.success_count + t.fail_count;
            if total < 3 { return true; } // 样本太少，保留
            let fail_rate = t.fail_count as f64 / total as f64;
            fail_rate < 0.3
        });
    }

    /// 容量控制：超过 200 条时按 LRU 淘汰
    fn enforce_capacity(&mut self) {
        const MAX_CAPACITY: usize = 200;
        if self.templates.len() > MAX_CAPACITY {
            let mut entries: Vec<_> = self.templates.iter().collect();
            entries.sort_by_key(|(_, t)| t.last_used);
            let to_remove: Vec<_> = entries.iter()
                .take(self.templates.len() - MAX_CAPACITY)
                .map(|(k, _)| k.to_string())
                .collect();
            for key in to_remove {
                self.templates.remove(&key);
            }
        }
    }
}
```

### 2.2.3 验证标准

- [ ] 10 次相似任务中，第 2 次开始缓存命中率 > 80%
- [ ] 缓存命中时 token 消耗减少 > 30%
- [ ] 泛化后的模板不包含项目特定的文件名/变量名

---

# Phase 3：记忆演化 + 自适应压缩（第 8-11 周）

> 目标：让记忆不断"成长"，让压缩策略自动"进化"

## 3.1 A-MEM 增强：结构化记忆演化

### 参考来源
- *A-MEM: Agentic Memory for LLM Agents* (NeurIPS 2025, arXiv:2502.12110)
- *AgentMemory* (2026.05) — 四层记忆整合 + 遗忘曲线

### 3.1.1 设计概述

Daedalus 已有 `agentic/` 记忆模块。基于 A-MEM 论文的三个核心机制进行增强：

1. **结构化笔记** — 每条记忆包含 7 个属性（内容、时间戳、关键词、标签、上下文描述、嵌入、链接集）
2. **自主链接** — embedding 粗筛 + LLM 精判，形成自组织的"盒子"（主题簇）
3. **记忆演化** — 新记忆触发旧记忆的属性更新

### 3.1.2 详细实现

#### 增强 dynamic_cheatsheet 的记忆结构

```rust
/// 增强后的 cheatsheet 条目——从扁平文本升级为结构化笔记
pub struct EnhancedEntry {
    pub id: EntryId,
    pub content: String,           // 原始内容
    pub keywords: Vec<String>,     // LLM 提取的关键词
    pub tags: Vec<String>,         // 分类标签
    pub context_description: String, // 语境描述
    pub links: HashSet<EntryId>,   // 与其他条目的链接
    pub embedding: Option<Vec<f32>>, // 嵌入向量（可选）
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: usize,
    pub decay_score: f64,          // 基于遗忘曲线的衰减分数
}
```

#### 链接生成机制（两阶段）

```rust
/// 阶段 1：粗筛——基于关键词/标签重叠快速筛选候选
fn coarse_filter(
    new_entry: &EnhancedEntry,
    all_entries: &[EnhancedEntry],
    top_k: usize, // 默认 10
) -> Vec<&EnhancedEntry> {
    let mut scored: Vec<_> = all_entries.iter()
        .filter(|e| e.id != new_entry.id)
        .map(|e| {
            // 关键词重叠分数
            let keyword_overlap = new_entry.keywords.iter()
                .filter(|k| e.keywords.contains(k))
                .count() as f64 / new_entry.keywords.len().max(1) as f64;

            // 标签重叠分数
            let tag_overlap = new_entry.tags.iter()
                .filter(|t| e.tags.contains(t))
                .count() as f64 / new_entry.tags.len().max(1) as f64;

            // 如果有 embedding，加入余弦相似度
            let embedding_sim = match (&new_entry.embedding, &e.embedding) {
                (Some(a), Some(b)) => cosine_similarity(a, b),
                _ => 0.0,
            };

            let score = keyword_overlap * 0.3 + tag_overlap * 0.3 + embedding_sim * 0.4;
            (e, score)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored.into_iter().take(top_k).map(|(e, _)| e).collect()
}

/// 阶段 2：精判——LLM 判断是否建立链接
const LINK_JUDGE_PROMPT: &str = r#"
Determine if these two knowledge entries should be linked.
Consider: shared concepts, causal relationships, complementary information.

Entry A: {entry_a}
Keywords: {keywords_a}

Entry B: {entry_b}
Keywords: {keywords_b}

Should they be linked? Answer YES or NO with one-sentence reason.
"#;

async fn precise_link_judge(
    entry_a: &EnhancedEntry,
    entry_b: &EnhancedEntry,
    llm: &dyn LlmClient,
) -> bool {
    let result = llm.complete(
        &LINK_JUDGE_PROMPT
            .replace("{entry_a}", &entry_a.content)
            .replace("{keywords_a}", &entry_a.keywords.join(", "))
            .replace("{entry_b}", &entry_b.content)
            .replace("{keywords_b}", &entry_b.keywords.join(", ")),
        CompletionConfig { max_tokens: 30, temperature: 0.0, ..Default::default() },
    ).await;

    result.to_uppercase().starts_with("YES")
}
```

#### 记忆演化机制

```rust
/// 新记忆加入后，检查是否需要更新近邻旧记忆
const EVOLUTION_PROMPT: &str = r#"
A new piece of knowledge has been added. Check if this existing entry
needs to be updated based on the new information.

New entry: {new_entry}
Existing entry: {existing_entry}

If the existing entry needs updating, provide the updated:
- keywords (comma-separated)
- context_description (one sentence)
Or respond "NO_UPDATE" if no changes needed.
"#;

async fn evolve_neighbors(
    new_entry: &EnhancedEntry,
    neighbors: &[&EnhancedEntry],
    llm: &dyn LlmClient,
    store: &mut EntryStore,
) {
    for neighbor in neighbors {
        let result = llm.complete(
            &EVOLUTION_PROMPT
                .replace("{new_entry}", &new_entry.content)
                .replace("{existing_entry}", &neighbor.content),
            CompletionConfig { max_tokens: 100, temperature: 0.2, ..Default::default() },
        ).await;

        if !result.contains("NO_UPDATE") {
            if let Some(updated) = parse_evolution_result(&result) {
                store.update_entry(neighbor.id, updated);
            }
        }
    }
}
```

#### 遗忘曲线衰减（参考 AgentMemory）

```rust
/// Ebbinghaus 遗忘曲线：retention = e^(-t/S)
/// S = stability（由访问频率增强）
fn calculate_decay_score(entry: &EnhancedEntry) -> f64 {
    let hours_since_access = (Utc::now() - entry.last_accessed)
        .num_hours() as f64;

    // stability 随访问次数增长（间隔重复效应）
    let stability = 24.0 * (1.0 + (entry.access_count as f64).ln());

    (-hours_since_access / stability).exp()
}

/// 定期清理衰减到阈值以下的记忆
fn gc_decayed_entries(store: &mut EntryStore, threshold: f64) {
    store.entries.retain(|_, entry| {
        let score = calculate_decay_score(entry);
        entry.decay_score = score;
        score > threshold // 默认 0.1
    });
}
```

### 3.1.3 验证标准

- [ ] 50 条记忆后，自动形成 3+ 个主题簇（盒子）
- [ ] 矛盾信息触发旧记忆更新
- [ ] 30 天未访问的低频记忆被自动清理

---

## 3.2 ACON 式压缩策略进化

### 参考来源
- Microsoft — *ACON: Optimizing Context Compression for Long-horizon LLM Agents* (2025.10, arXiv:2510.00615)
- 开源实现: github.com/microsoft/acon

### 3.2.1 设计概述

ACON 的核心四阶段流水线适配到 Daedalus 的 compact 系统：

```
阶段 1：收集压缩成功/失败的配对轨迹
阶段 2：LLM 分析失败原因 → 迭代更新压缩 prompt
阶段 3：（可选）蒸馏到更小的压缩模型
```

在 Daedalus 中不需要蒸馏（不训练模型），**重点实现阶段 1-2 的压缩策略自进化**。

### 3.2.2 详细实现

#### 轨迹收集器（`src/memory/sliding_window/trajectory_collector.rs`）

```rust
/// 记录 compact 前后的对比数据，用于压缩策略进化
pub struct CompactTrajectory {
    pub id: TrajectoryId,
    pub timestamp: DateTime<Utc>,

    // compact 前的状态
    pub pre_compact_messages: Vec<ChatMessage>,
    pub pre_compact_tokens: usize,

    // compact 后的状态
    pub post_compact_messages: Vec<ChatMessage>,
    pub post_compact_tokens: usize,
    pub compression_ratio: f64,

    // 后续表现（compact 后的 N 轮交互）
    pub post_compact_performance: CompactPerformance,
}

pub struct CompactPerformance {
    pub rounds_after: usize,
    pub tool_success_rate: f64,    // 工具调用成功率
    pub had_context_confusion: bool, // 是否出现"我之前提到过"等困惑
    pub needed_re_read: bool,       // 是否需要重新读取已读文件
    pub user_correction_count: usize, // 用户纠正次数
}

impl CompactPerformance {
    /// 判断 compact 是否导致了信息丢失
    pub fn indicates_failure(&self) -> bool {
        self.had_context_confusion
            || self.needed_re_read
            || self.user_correction_count > 0
            || self.tool_success_rate < 0.7
    }
}
```

#### 压缩策略进化（`src/memory/sliding_window/strategy_evolution.rs`）

```rust
const COMPRESSION_ANALYSIS_PROMPT: &str = r#"
Analyze why the context compression led to poor performance.

Pre-compact context (last 5 messages):
{pre_compact_tail}

Post-compact summary:
{post_compact_summary}

Post-compact failure signals:
- Context confusion: {had_confusion}
- Needed re-read files: {needed_reread}
- User corrections: {correction_count}
- Tool success rate: {tool_success_rate}

Current compression guidelines:
{current_guidelines}

What information was likely lost that caused the failure?
How should the compression guidelines be updated to prevent this?
Provide updated guidelines in the same format.
"#;

/// 每积累 5 条失败轨迹，触发一次策略进化
async fn evolve_compression_strategy(
    failed_trajectories: &[CompactTrajectory],
    current_guidelines: &str,
    llm: &dyn LlmClient,
) -> String {
    // 选取最近的 3 条失败案例
    let examples = failed_trajectories.iter()
        .rev()
        .take(3)
        .map(|t| format_trajectory_for_analysis(t))
        .collect::<Vec<_>>()
        .join("\n---\n");

    llm.complete(
        &COMPRESSION_ANALYSIS_PROMPT
            .replace("{pre_compact_tail}", &examples)
            // ... 填充其他字段
            .replace("{current_guidelines}", current_guidelines),
        CompletionConfig { max_tokens: 500, temperature: 0.3, ..Default::default() },
    ).await
}
```

#### 将进化后的策略注入 compact prompt

```rust
/// 动态更新 COMPACT_SYSTEM_PROMPT
/// 在 compact_ops.rs 的 run_compact() 开头调用
fn get_evolved_compact_prompt(
    base_prompt: &str,
    evolved_guidelines: &Option<String>,
) -> String {
    match evolved_guidelines {
        Some(guidelines) => format!(
            "{base_prompt}\n\n\
            [Learned Compression Guidelines]\n\
            Based on past compression failures, pay special attention to:\n\
            {guidelines}"
        ),
        None => base_prompt.to_string(),
    }
}
```

### 3.2.3 验证标准

- [ ] 5 次 compact 失败后自动触发策略进化
- [ ] 进化后的策略在同类场景中减少信息丢失
- [ ] 进化日志可审计（存储在 `.daedalus/cache/compression_evolution.json`）

---

# Phase 4：自改进 + 记忆统一（第 12-15 周）

> 目标：让 Agent "从经验中学习"，实现自主改进闭环

## 4.1 Prompt 自调优循环

### 参考来源
- *A Self-Improving Coding Agent (SICA)* (ICLR 2025, Robeyns & Szummer)
- Microsoft — *ACON* 的"自然语言空间优化"方法

### 4.1.1 设计概述

SICA 的核心洞察：Agent 可以编辑自身的 prompt 和代码来提升性能。适配到 Daedalus：

```
记录失败 case → 归因分析 → 自动修订 system prompt / tool descriptions → 评估 → 采纳或回滚
```

### 4.1.2 详细实现

#### 失败记录器

```rust
pub struct FailureRecord {
    pub timestamp: DateTime<Utc>,
    pub task_description: String,
    pub failure_type: FailureType,
    pub context_snapshot: String, // 失败时的关键上下文
    pub tool_trace: Vec<ToolUseRecord>,
    pub user_feedback: Option<String>, // 用户的纠正信息
}

pub enum FailureType {
    WrongToolChoice,        // 选错了工具
    IncompleteResult,       // 结果不完整
    ContextLoss,            // 上下文丢失导致错误
    InfiniteLoop,           // 陷入循环
    ScopeCreep,             // 范围蔓延
    UserCorrected(String),  // 用户主动纠正
}
```

#### Prompt 修订引擎

```rust
const PROMPT_REVISION_PROMPT: &str = r#"
You are a prompt engineering expert. Analyze these failure cases
and suggest specific improvements to the system prompt.

Current system prompt (relevant section):
{current_prompt_section}

Failure cases:
{failure_cases}

Rules:
1. Only suggest MINIMAL changes — prefer adding 1-2 sentences over rewriting
2. Changes must be specific and actionable
3. Each change must reference which failure it addresses
4. Do NOT weaken existing correct behaviors

Output format:
SECTION: <which section to modify>
CHANGE_TYPE: ADD | MODIFY | REMOVE
ORIGINAL: <original text, if MODIFY>
REVISED: <new text>
RATIONALE: <which failure(s) this addresses>
"#;

async fn suggest_prompt_revisions(
    failures: &[FailureRecord],
    current_prompts: &PromptConfig,
    llm: &dyn LlmClient,
) -> Vec<PromptRevision> {
    // 按 failure type 分组，每组取最近 2 条
    let grouped = group_by_type(failures);

    let mut all_revisions = vec![];
    for (failure_type, cases) in grouped {
        let relevant_section = find_relevant_prompt_section(current_prompts, &failure_type);
        let result = llm.complete(
            &PROMPT_REVISION_PROMPT
                .replace("{current_prompt_section}", relevant_section)
                .replace("{failure_cases}", &format_failure_cases(&cases[..2.min(cases.len())])),
            CompletionConfig { max_tokens: 300, temperature: 0.3, ..Default::default() },
        ).await;

        all_revisions.extend(parse_revisions(&result));
    }

    all_revisions
}
```

#### 安全回滚机制

```rust
pub struct PromptVersionControl {
    pub versions: Vec<PromptVersion>,
    pub current: usize,
    pub max_versions: usize,  // 保留最近 10 个版本
}

pub struct PromptVersion {
    pub version: usize,
    pub prompt: String,
    pub revisions_applied: Vec<PromptRevision>,
    pub performance_after: Option<PerformanceMetrics>,
}

impl PromptVersionControl {
    /// 如果新版本的失败率比旧版本高 20%+，自动回滚
    fn auto_rollback_check(&mut self) -> bool {
        if self.versions.len() < 2 { return false; }

        let current = &self.versions[self.current];
        let previous = &self.versions[self.current - 1];

        match (&current.performance_after, &previous.performance_after) {
            (Some(cur), Some(prev)) => {
                if cur.failure_rate > prev.failure_rate * 1.2 {
                    self.current -= 1; // 回滚
                    true
                } else {
                    false
                }
            },
            _ => false,
        }
    }
}
```

### 4.1.3 验证标准

- [ ] 连续 3 次相同类型失败后自动生成修订建议
- [ ] 修订后同类失败率下降 > 30%
- [ ] 修订导致回归时自动回滚

---

## 4.2 记忆工具化

### 参考来源
- *Agentic Memory (AgeMem): Learning Unified LTM and STM Management* (ACL 2026, arXiv:2601.01885)
- *AgentMemory* (2026.05) — 记忆槽位 + MCP 工具暴露

### 4.2.1 设计概述

AgeMem 的核心洞察：将 LTM/STM 操作暴露为 tool call，让模型主动管理记忆。适配到 Daedalus：将 `dynamic_cheatsheet` 的操作包装为可被模型调用的工具。

### 4.2.2 详细实现

#### 记忆工具定义

```rust
/// 注册为 Agent 可调用的工具
fn register_memory_tools(registry: &mut ToolRegistry) {
    registry.register(Tool {
        name: "memory_store",
        description: "Store important information in long-term memory for later recall. \
            Use this when you encounter key facts, decisions, patterns, or user preferences \
            that may be needed in future interactions.",
        parameters: json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The information to store" },
                "category": {
                    "type": "string",
                    "enum": ["fact", "decision", "pattern", "preference", "architecture"],
                    "description": "Category of the memory"
                },
                "keywords": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keywords for retrieval"
                }
            },
            "required": ["content", "category"]
        }),
        handler: memory_store_handler,
    });

    registry.register(Tool {
        name: "memory_recall",
        description: "Search long-term memory for previously stored information. \
            Use this when you need to recall earlier decisions, facts, or patterns \
            that may have been compressed out of the current context.",
        parameters: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to search for" },
                "category": {
                    "type": "string",
                    "enum": ["fact", "decision", "pattern", "preference", "architecture", "any"],
                    "description": "Optional category filter"
                },
                "limit": { "type": "integer", "default": 5 }
            },
            "required": ["query"]
        }),
        handler: memory_recall_handler,
    });

    registry.register(Tool {
        name: "memory_forget",
        description: "Mark a memory as outdated or incorrect. \
            Use this when stored information is no longer valid.",
        parameters: json!({
            "type": "object",
            "properties": {
                "memory_id": { "type": "string" },
                "reason": { "type": "string" }
            },
            "required": ["memory_id", "reason"]
        }),
        handler: memory_forget_handler,
    });
}
```

#### 引导模型主动使用记忆工具

在系统 prompt 中添加记忆使用引导：

```rust
const MEMORY_TOOL_GUIDANCE: &str = r#"
## Memory Management

You have access to long-term memory tools. Use them proactively:

**When to STORE memory:**
- Architecture decisions and their rationale
- User preferences discovered during interaction
- Recurring patterns (e.g., "this project uses X pattern for Y")
- Key facts that took effort to discover

**When to RECALL memory:**
- Before making architecture decisions (check for prior decisions)
- When context feels incomplete after a compact
- When encountering something that "feels familiar"

**When to FORGET:**
- When stored information contradicts new discoveries
- When a decision has been reversed
"#;
```

### 4.2.3 验证标准

- [ ] 长任务中模型主动调用 `memory_store` 至少 3 次
- [ ] compact 后模型调用 `memory_recall` 恢复关键上下文
- [ ] 记忆工具不增加 > 5% 的 token 开销

---

# 附录

## A. 参考文献汇总

| # | 论文/文章 | 来源 | 时间 | Phase |
|---|----------|------|------|:-----:|
| 1 | *Measuring AI Ability to Complete Long Tasks* | METR | 2025.03 | 背景 |
| 2 | *ACON: Optimizing Context Compression for Long-horizon LLM Agents* | Microsoft, arXiv:2510.00615 | 2025.10 | P3 |
| 3 | *Effective Context Engineering for AI Agents* | Anthropic | 2025.09 | P1, P2 |
| 4 | *MemPO: Self-Memory Policy Optimization* | arXiv:2603.00680 | 2026.02 | 理论参考 |
| 5 | *Agentic Memory: Unified LTM and STM* | arXiv:2601.01885, ACL 2026 | 2026.01 | P4 |
| 6 | *A-MEM: Agentic Memory for LLM Agents* | NeurIPS 2025, arXiv:2502.12110 | 2025.02 | P3 |
| 7 | *HiAgent: Hierarchical Working Memory* | ACL 2025 | 2025 | P2 |
| 8 | *Agentic Plan Caching: Test-Time Memory* | NeurIPS 2025, arXiv:2506.14852 | 2025 | P2 |
| 9 | *A Self-Improving Coding Agent (SICA)* | ICLR 2025 | 2025.04 | P4 |
| 10 | *Run Long Horizon Tasks with Codex* | OpenAI Developer Blog | 2026.02 | P1 |
| 11 | *GLM-5.1: Towards Long-Horizon Tasks* | 智谱 AI | 2026.04 | P1 |
| 12 | *Memory for Autonomous LLM Agents* (Survey) | arXiv:2603.07670 | 2026.03 | 综述 |
| 13 | *AgentMemory: Persistent Memory for AI Coding Agents* | github.com/rohitg00/agentmemory | 2026.05 | P1, P3 |

## B. 各 Phase 间的依赖关系

```
Phase 1 (持久化 + 压缩增强)
    │
    ├──→ Phase 2 (分层记忆 + 计划缓存)
    │       │
    │       ├──→ Phase 3 (记忆演化 + 自适应压缩)
    │       │       │
    │       │       └──→ Phase 4 (自改进 + 记忆统一)
    │       │
    │       └──→ Phase 4 (记忆工具化依赖分层记忆)
    │
    └──→ Phase 3 (ACON 需要 Phase 1 的轨迹收集)
```

## C. 关键指标追踪

| 指标 | 当前基线 | Phase 1 目标 | Phase 4 目标 |
|------|---------|:----------:|:----------:|
| 最大连续有效轮次 | ~50 | 100+ | 300+ |
| compact 后信息丢失率 | 未测量 | < 10% | < 3% |
| 相似任务 token 消耗 | 100% | 90% | 50% |
| 长任务目标偏离检出率 | 0% | > 80% | > 95% |
| 记忆召回准确率 | 未测量 | 80% | 95% |
