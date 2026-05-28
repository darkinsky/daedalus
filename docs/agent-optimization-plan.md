
# 🏛️ Daedalus 顶级 Agent 优化方案

> 目标：打造具备自进化、长时程任务处理、智能上下文管理能力的顶级 AI Agent
>
> 基于 Claude Code、Codex、Manus 等产品实践，以及 2025-2026 年前沿论文设计

---

## 一、现状分析

Daedalus 已具备扎实的基础架构：

| 模块 | 现状 | 成熟度 |
|------|------|--------|
| Memory | 6种策略（SlidingWindow/Cheatsheet/Agentic/Wiki/ACE/MemPalace） | ⭐⭐⭐⭐⭐ |
| SubAgent | 支持并行、隔离（Worktree）、权限控制、共享上下文 | ⭐⭐⭐⭐ |
| Middleware | 洋葱模型双管道（Turn级 + Tool级） | ⭐⭐⭐⭐⭐ |
| Context Pressure | 多信号健康评估（staleness_ratio, rot detection） | ⭐⭐⭐⭐ |
| Skill | 渐进式加载、注册表 | ⭐⭐⭐ |
| Tracing | 完整的 span 追踪 | ⭐⭐⭐⭐ |

---

## 二、十二大优化方向

### 方向 1：自进化引擎（Self-Evolution Engine）

**核心思想**：Agent 从静态部署进化为"越用越强"的自适应系统。通过轨迹记录 → 经验评估 → 技能提炼的闭环，实现持续自我优化。

**关键能力**：
- Trajectory Store：记录每次 tool_loop 的完整执行路径（工具调用序列、token 消耗、结果质量）
- Offline Distiller：后台异步分析成功轨迹，提炼为新的 Skill 或 Prompt 片段
- A/B Variant Testing：对 system prompt 的关键段落生成变体，通过 eval 选择最优
- Nudge Engine：定时复盘最近 N 次会话，发现重复模式并自动固化

**参考文献**：
- [1] *A Comprehensive Survey of Self-Evolving AI Agents* (arXiv:2507.21046, 王梦迪团队, 2025)
- [2] *SE-Agent: Self-Evolution Trajectory Optimization* (arXiv:2508.02085, 2025)
- [3] Hermes Agent Self-Improving 机制 (2025)

---

### 方向 2：分层上下文压缩（Hierarchical Context Compression）

**核心思想**：三级渐进式压缩策略，根据 ContextHealth 严重程度自动选择压缩级别。

**三级压缩策略**：
- **L1 Token级**：基于自信息过滤，去除低信息量 token，对工具输出中的冗余进行裁剪
- **L2 消息级**：将旧轮次的完整对话替换为摘要，保留结构丢弃细节
- **L3 语义级**：多轮相关讨论合并为知识条目，知识图谱化

**关键创新**：
- 结构感知压缩：对代码类输出保留 AST 骨架，对日志类输出只保留 error/warning
- 注意力引导：在压缩后的摘要前加入标记，让模型知道这是摘要
- 可逆压缩：保留原始内容的 hash，需要时可通过工具恢复

**参考文献**：
- [4] *Selective Context* (NeurIPS 2024) — 基于自信息的 token 级压缩
- [5] 腾讯 TencentDB Agent Memory 短期记忆压缩 (2026.05 开源)
- [6] Claude Code Compressor 机制 (Anthropic, 2025)

---

### 方向 3：动态 Skill 学习与自动生成

**核心思想**：从静态 Skill 加载升级为动态学习系统，实现模式检测 → 草案生成 → 用户确认 → 热加载的完整流程。

**关键能力**：
- Pattern Detector：在 reflect_on_turn 中检测重复操作模式
- Skill Generator：将检测到的模式自动生成 SKILL.md 草案
- Progressive Disclosure：三层加载（metadata 常驻 → core 按需 → reference 深度查询时）
- Cross-session Skill Sharing：项目级 Skill 可被团队共享

**参考文献**：
- [7] Anthropic Agent Skills (2025.10)
- [8] *2026 Agent Skills 技术与安全白皮书*
- [9] Codex App Skill 打包机制 (OpenAI, 2026)

---

### 方向 4：智能上下文工程（Context Engineering Pipeline）

**核心思想**：Context 不是被动填充，而是主动工程化构建。构建查询改写 → 检索编排 → 相关性评分 → 预算分配的完整流程。

**关键能力**：
- Query Rewriting：对模糊用户输入进行消歧义和意图扩展
- Budget-aware Assembly：根据任务类型动态调整各部分的 token 预算
- Priority Structuring：高优先级信息放在 context 的头部和尾部（利用 LLM 的 U 型注意力曲线）
- Grounding with Tools：在构建 context 时主动调用工具获取实时信息

**参考文献**：
- [10] Andrej Karpathy "Context Engineering" (2025.07)
- [11] *Context Engineering: 2026年真正重要的6种技术*
- [12] Manus Agent 上下文构建策略 (2026)

---

### 方向 5：多层记忆协同（Unified Memory Orchestration）

**核心思想**：将现有6种互斥的 Memory 策略整合为工作记忆/情景记忆/程序记忆三层协同架构。

**三层架构**：
- **工作记忆 (Session)**：SlidingWindow（对话历史）+ DynamicCheatsheet（当前任务要点）
- **情景记忆 (Cross-session)**：MemPalace（空间化知识）+ Agentic（知识图谱）
- **程序记忆 (Permanent)**：ACE Playbook（操作规程）+ Wiki（项目知识库）+ Skills（固化能力）

**关键创新**：
- 统一编排层：协调多种记忆策略的读写，智能组装上下文
- 跨层更新：写入时同时更新多层记忆，异步提取关键经验
- 模式固化：检测重复模式自动从情景记忆晋升为程序记忆

**参考文献**：
- [13] Claude Code 三层记忆架构 (Anthropic, 2025)
- [14] 腾讯 Agent Memory（短期压缩 + 长期个性化）(2026)
- [15] *Agent "记忆断片"如何破局？Memory正成为AI的新战场* (2025)

---

### 方向 6：自适应任务分解与并行编排

**核心思想**：在现有 SubAgent 基础上增强任务分析 → DAG 生成 → 并行调度 → 冲突解决能力。

**关键能力**：
- Task Complexity Scoring：基于任务描述预估复杂度，决定是否需要分解
- Dependency Graph：自动分析子任务间的依赖关系，最大化并行度
- Adaptive Re-planning：当某个子任务失败时，自动调整剩余计划
- Progressive Merge：不等所有子任务完成，已完成的先合并，减少等待

**参考文献**：
- [16] Codex App 多智能体并行 + 工作树隔离 (OpenAI, 2026.02)
- [17] Claude Code Sub-Agents + Task 工具 (Anthropic, 2025)
- [18] *LLM智能体环境Scaling综述* (2026.01, 20页)

---

### 方向 7：Long-Horizon Task 架构（长时程任务）

**核心思想**：解决长时程任务（Deep Research、大型重构、多文件项目生成）中的规划漂移、上下文遗忘、错误累积和资源浪费问题。

**核心挑战**：
1. 规划漂移：执行过程中偏离原始目标
2. 上下文遗忘：执行到后期忘记前期决策
3. 错误累积：早期小错误在后续步骤中被放大
4. 资源浪费：无法预估任务复杂度，token 消耗不可控

**关键能力**：
- Planner-Executor 分离：Planner 用强推理模型（如 o3），Executor 用快速模型（如 Sonnet）
- Milestone Verification：每个里程碑有明确的验证条件（测试通过、文件存在、输出匹配等）
- Budget-Aware Planning：预估每个子任务的 token 消耗，超预算时自动降级或拆分
- Lifecycle Memory：任务蓝图始终保持在 context 头部，防止遗忘
- Goal Drift Detection：定期检测当前执行方向是否偏离原始目标
- Checkpoint & Rollback：支持回滚到任意里程碑状态

**参考文献**：
- [19] *Plan-and-Act* (arXiv:2503.09572, 2025.03) — Planner + Executor 分离架构
- [20] *PaperGuru Lifecycle-Aware Memory* (2026.05) — 66.05% on PaperBench
- [21] Harrison Chase × 红杉资本对话 (2026.01) — "2026年是Long Horizon Agent的元年"
- [22] *LLM智能体环境Scaling综述* (2026.01) — 环境交互中的长期决策

---

### 方向 8：Test-Time Compute Scaling（推理时计算扩展）

**核心思想**：不是所有任务都需要同等计算量。根据任务复杂度自动选择计算策略，简单任务快速响应，复杂任务投入更多"思考预算"。

**复杂度分级策略**：
- **Simple**：直接响应，1次调用，~500 tokens
- **Medium**：ReAct Loop，3-5次迭代，~5K tokens
- **Complex**：深度推理，多轮验证+回溯，~50K tokens
- **Ultra-Complex**：多Agent协作，并行+投票，~200K tokens

**扩展策略**：
- Direct：直接响应，不额外计算
- Best-of-N：生成N个候选，选最优
- Iterative Refinement：生成→验证→修正循环
- Tree Search：MCTS 风格的方案探索
- Multi-Agent Voting：多Agent投票

**关键创新**：
- Complexity-Aware Routing：根据任务复杂度自动选择计算策略
- Verifier-Guided Search：用轻量级验证器（lint、test）引导搜索方向
- Budget Elasticity：预算可在子任务间动态流转
- Early Termination：验证器确认结果正确时立即停止额外计算

**参考文献**：
- [23] *AgentTTS* (NeurIPS 2025) — 多阶段任务的 TTS 建模为组合优化问题
- [24] *Trae Agent* (字节跳动, 2025.08) — 软件工程 Agent 的 Test-time Scaling
- [25] *PRISM* (ICML 2026) — 离散扩散模型的高效 TTS
- [26] *Scaling LLM Test-Time Compute Optimally* (2025)

---

### 方向 9：Harness Engineering（Agent 约束与控制系统）

**核心思想**：从 Prompt Engineering → Context Engineering → Harness Engineering 的演进。Harness 关注"在什么环境中做事"，包含约束、反馈和控制三大支柱。

**三大支柱**：

**Guardrails（安全边界）**：
- 文件系统沙箱
- 命令白名单
- Token 预算硬限
- 操作可逆性检查

**Feedback Loops（实时反馈）**：
- Lint/Test 即时反馈
- 类型检查反馈
- 运行时错误捕获
- 用户隐式反馈（如拒绝/修改建议）

**Control Systems（流程控制）**：
- 审批门控：高风险操作需要确认
- 渐进式权限：随信任度提升解锁更多能力
- 自动回滚：检测到破坏性操作时自动恢复
- 死循环检测：相同工具+相同参数连续调用N次时触发逃逸

**参考文献**：
- [27] Harness Engineering 新范式 (2026)
- [28] Anthropic Claude Code Harness 设计 (2025)
- [29] OpenAI Codex Sandbox + Approval 机制 (2026)

---

### 方向 10：Agentic Plan Caching（计划缓存与复用）

**核心思想**：相似任务不需要每次从零规划。缓存成功的执行计划，下次遇到相似任务时直接复用/微调，大幅降低 token 消耗和响应延迟。

**关键能力**：
- Plan Cache Lookup：基于语义相似度匹配历史成功计划
- Plan Adaptation：将缓存计划适配到当前上下文
- Success Rate Tracking：跟踪每个缓存计划的成功率，低于阈值自动淘汰
- Parameter Template：计划步骤使用参数模板（含占位符），支持泛化复用

**工作流程**：
1. 新任务到达 → 语义检索 Plan Cache
2. 命中 → 取出缓存计划 → 适配当前上下文 → 执行
3. 未命中 → 完整规划 → 执行 → 成功后缓存

**参考文献**：
- [30] *Agentic Plan Caching: Test-Time Memory for Fast and Cost-Efficient LLM Agents* (NeurIPS 2025)

---

### 方向 11：A-Mem 自组织记忆网络

**核心思想**：受 Zettelkasten（卡片盒笔记法）启发，记忆不是孤立存储，而是形成自组织的关联网络，支持多跳推理。

**结构化笔记**：每个记忆包含：
- 原始内容 + 时间戳
- LLM 生成的关键词、标签、上下文描述
- 向量嵌入
- 关联集（与其他记忆的链接）

**关键特性**：
- 动态关联：新记忆加入时，自动通过语义相似度 + LLM 分析建立链接
- 记忆进化：新信息融入时，触发相关旧记忆的更新（关键词、标签、上下文）
- 多跳检索：查询时不仅返回直接匹配，还沿着关联链路返回相关记忆
- 遗忘曲线：长期未访问的记忆逐渐降低权重，但不删除

**参考文献**：
- [31] *A-Mem: Agentic Memory for LLM Agents* (NeurIPS 2025, GitHub: WujiangXu/AgenticMemory)

---

### 方向 12：Agentic RL — 强化学习驱动的 Agent 优化

**核心思想**：不依赖人工设计规则，而是通过 RL 让 Agent 自己学会何时该用哪个工具、何时该停止搜索、何时该请求人工帮助、如何分配 token 预算。

**在 Daedalus 中的应用**（不需要真正训练模型，用 RL 思想优化决策策略）：
- 工具选择偏好权重：基于历史成功率调整
- 搜索深度偏好：何时停止探索
- 并行度偏好：何时启动子Agent
- 求助阈值：何时请求人工介入

**奖励信号设计**：
- 任务完成度 (+10)
- Token 效率 (+3)
- 工具调用精准度 (+2)
- 超时惩罚 (-5)
- 错误累积惩罚 (-3)

**参考文献**：
- [32] *The Landscape of Agentic Reinforcement Learning for LLMs: A Survey* (2025.11)
- [33] *AGILE* (字节跳动) — 端到端 RL 优化 Agent 的多种能力
- [34] *LLM智能体环境Scaling综述* (2026.01) — RL 训练范式

---

## 三、优先级与对比

| 方向 | 论文支撑 | 与现有架构契合度 | 实现难度 | 预期收益 | 优先级 |
|------|----------|-----------------|----------|----------|--------|
| 1. 自进化引擎 | ⭐⭐⭐⭐ | ⭐⭐⭐ | 高 | 🔥🔥🔥🔥🔥 | P2 |
| 2. 分层上下文压缩 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 低 | 🔥🔥🔥🔥 | **P0** |
| 3. 动态 Skill 学习 | ⭐⭐⭐ | ⭐⭐⭐⭐ | 中 | 🔥🔥🔥🔥 | P1 |
| 4. 智能上下文工程 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 低 | 🔥🔥🔥🔥 | **P0** |
| 5. 多层记忆协同 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 中 | 🔥🔥🔥🔥🔥 | P1 |
| 6. 自适应任务分解 | ⭐⭐⭐ | ⭐⭐⭐⭐ | 中 | 🔥🔥🔥 | P2 |
| 7. Long-Horizon Task | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 中 | 🔥🔥🔥🔥🔥 | **P0** |
| 8. Test-Time Scaling | ⭐⭐⭐⭐ | ⭐⭐⭐ | 中 | 🔥🔥🔥🔥 | P1 |
| 9. Harness Engineering | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 低 | 🔥🔥🔥🔥🔥 | **P0** |
| 10. Plan Caching | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 低 | 🔥🔥🔥 | P1 |
| 11. A-Mem 自组织记忆 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 中 | 🔥🔥🔥🔥 | P1 |
| 12. Agentic RL | ⭐⭐⭐⭐⭐ | ⭐⭐ | 高 | 🔥🔥🔥🔥🔥 | P2 |

---

## 四、实施路线图

### Phase 0：基础增强（立即启动，2-3周）

**目标**：利用现有 Middleware 架构，最小改动获得最大收益

| 任务 | 说明 | 依赖 |
|------|------|------|
| 分层上下文压缩 | 在现有 ContextHealth 基础上增加三级压缩策略 | 无 |
| Context Engineering MW | 新增中间件，实现 token 预算分配和优先级排列 | 无 |
| Harness Engineering | 新增死循环检测、自动回滚、渐进式权限中间件 | 无 |

### Phase 1：核心能力建设（第3-6周）

**目标**：解决"Agent能做大事"的核心能力

| 任务 | 说明 | 依赖 |
|------|------|------|
| Long-Horizon Controller | 里程碑规划、目标漂移检测、检查点管理 | Phase 0 |
| Memory Orchestrator | 三层记忆协同编排层 | Phase 0 |
| Plan Caching | 计划缓存与语义匹配复用 | Phase 0 |

### Phase 2：智能进化（第6-10周）

**目标**：让 Agent "越用越聪明"

| 任务 | 说明 | 依赖 |
|------|------|------|
| A-Mem 自组织记忆 | 记忆关联网络、多跳检索 | Phase 1 Memory Orchestrator |
| 动态 Skill 学习 | 模式检测、Skill 自动生成 | Phase 1 |
| Test-Time Scaling | 复杂度评估、策略路由 | Phase 1 Long-Horizon |

### Phase 3：精细化优化（第10-14周）

**目标**：精细化优化，需要 eval 基础设施支撑

| 任务 | 说明 | 依赖 |
|------|------|------|
| 自进化引擎 | 轨迹记录、离线蒸馏、A/B测试 | Phase 2 |
| 自适应任务分解 | DAG生成、并行调度、冲突解决 | Phase 1 |
| Eval 基础设施 | 内部评估套件，量化优化效果 | 无 |

### Phase 4：长期投资（第14周+）

**目标**：数据积累到一定量后启动

| 任务 | 说明 | 依赖 |
|------|------|------|
| Agentic RL | 基于历史轨迹的策略优化 | Phase 3 Eval + 自进化引擎 |
| 跨项目知识迁移 | 从一个项目学到的经验迁移到新项目 | Phase 2 A-Mem |

---

## 五、关键技术决策

| 决策点 | 建议 | 理由 |
|--------|------|------|
| 压缩 vs 检索 | 短期用压缩，长期建检索索引 | 压缩成本低立即可用，检索效果好但需基础设施 |
| 同步 vs 异步进化 | 进化学习必须异步 | 不能阻塞用户交互 |
| 本地 vs 远程记忆 | 程序记忆本地文件，情景记忆可选向量数据库 | 本地快，远程准 |
| Skill 生成安全性 | 自动生成的 Skill 必须经过沙箱验证 + 用户确认 | 防止错误模式固化 |
| Planner 模型选择 | Planner 用强推理模型，Executor 用快速模型 | 成本最优 |
| 评估基准 | 建立内部 eval suite（类似 SWE-bench） | 量化每次优化的效果 |

---

## 六、参考文献汇总

### 自进化与自我优化
1. *A Comprehensive Survey of Self-Evolving AI Agents* (arXiv:2507.21046, 王梦迪团队, 2025)
2. *SE-Agent: Self-Evolution Trajectory Optimization* (arXiv:2508.02085, 2025)
3. Hermes Agent Self-Improving 机制 (2025)

### 上下文压缩与管理
4. *Selective Context* (NeurIPS 2024) — 基于自信息的 token 级压缩
5. 腾讯 TencentDB Agent Memory 短期记忆压缩 (2026.05 开源)
6. Claude Code Compressor 机制 (Anthropic, 2025)

### Skill 学习
7. Anthropic Agent Skills (2025.10)
8. *2026 Agent Skills 技术与安全白皮书*
9. Codex App Skill 打包机制 (OpenAI, 2026)

### 上下文工程
10. Andrej Karpathy "Context Engineering" (2025.07)
11. *Context Engineering: 2026年真正重要的6种技术*
12. Manus Agent 上下文构建策略 (2026)

### 记忆系统
13. Claude Code 三层记忆架构 (Anthropic, 2025)
14. 腾讯 Agent Memory（短期压缩 + 长期个性化）(2026)
15. *Agent "记忆断片"如何破局？Memory正成为AI的新战场* (2025)
16. *A-Mem: Agentic Memory for LLM Agents* (NeurIPS 2025, GitHub: WujiangXu/AgenticMemory)

### 任务分解与并行
17. Codex App 多智能体并行 + 工作树隔离 (OpenAI, 2026.02)
18. Claude Code Sub-Agents + Task 工具 (Anthropic, 2025)

### Long-Horizon Task
19. *Plan-and-Act* (arXiv:2503.09572, 2025.03) — Planner + Executor 分离架构
20. *PaperGuru Lifecycle-Aware Memory* (2026.05) — 66.05% on PaperBench
21. Harrison Chase × 红杉资本对话 (2026.01) — "2026年是Long Horizon Agent的元年"
22. *LLM智能体环境Scaling综述* (2026.01, 20页)

### Test-Time Compute Scaling
23. *AgentTTS* (NeurIPS 2025) — 多阶段任务的 TTS 建模为组合优化问题
24. *Trae Agent* (字节跳动, 2025.08) — 软件工程 Agent 的 Test-time Scaling
25. *PRISM* (ICML 2026) — 离散扩散模型的高效 TTS
26. *Scaling LLM Test-Time Compute Optimally* (2025)

### Harness Engineering
27. Harness Engineering 新范式 (2026)
28. Anthropic Claude Code Harness 设计 (2025)
29. OpenAI Codex Sandbox + Approval 机制 (2026)

### Plan Caching
30. *Agentic Plan Caching: Test-Time Memory for Fast and Cost-Efficient LLM Agents* (NeurIPS 2025)

### 强化学习
31. *The Landscape of Agentic Reinforcement Learning for LLMs: A Survey* (2025.11)
32. *AGILE* (字节跳动) — 端到端 RL 优化 Agent 的多种能力

---

## 七、成功指标

| 指标 | 当前基线 | Phase 0 目标 | 最终目标 |
|------|----------|-------------|----------|
| 长任务完成率 | - | - | >80% |
| 平均 token 消耗/任务 | 基线 | -30% | -50% |
| 重复任务响应速度 | 基线 | -20% | -60% (Plan Cache) |
| 目标漂移率 | 未测量 | 建立基线 | <10% |
| Skill 自动生成准确率 | 0% | - | >70% |
| 用户满意度 | 基线 | +10% | +30% |
