# Code Review Trace 分析 — v6（2026-04-25）

## 1. v6 运行概览

| 指标 | 值 |
|------|-----|
| Trace ID | `a711b6c4-4ecc-496a-983f-21dc5d7ea40d` |
| 总耗时 | 357s（~5.9 min） |
| 总 token | 2,351,465 |
| 缓存 token | 212,326（9.1%） |
| subagent | 1（code-reviewer，22 轮） |
| 审查报告质量 | A（0 误报、5 个新发现、15/15 准确） |

## 2. 六版全量对比

| 指标 | v2 | v3 | v4 | v5 | v6 | 趋势 |
|------|:-:|:-:|:-:|:-:|:-:|:-:|
| 总耗时 | 877s | 609s | 706s | 393s | **357s** | ✅ |
| 总 token | 6.21M | 6.68M | 8.72M | 3.38M | **2.35M** | ✅✅ |
| 轮次 | 100 | 70 | 74 | 20 | **22** | ✅ |
| 峰值 tokens | 66K | 153K | 230K | 240K | **137K** | ⚠️ |
| 稳态 tokens | 40-55K | 80-100K | 90-126K | 113-135K | **123-134K** | ✅ |
| 并行度 | 1.3 | 2.3 | 2.3 | 4.1 | **3.6** | ✅ |
| 唯一文件 | 23 | 47 | 34 | 35 | **41** | ✅ |
| 最大重复 | 20 | 11 | 6 | 5 | **8** | ⚠️ |
| take_note | 0 | 0 | 6 | 0 | **0** | 🔴 |
| grep_search | 少 | 21 | 0 | 0 | **16** | ✅ |
| 缓存率 | — | — | 3.65% | 1.55% | **9.1%** | ✅ |
| 语言 | 差 | — | 50%混 | 100%中 | **100%中** | ✅ |
| 报告质量 | — | — | B+ | A- | **A** | ✅ |

## 3. 改善确认

### 3.1 截断断崖彻底消除

`CHARS_PER_TOKEN=3` 修复效果：

```
v4: 230K → 73K  (-157K, -68%)  ← 灾难性断崖
v5: 240K → 113K (-127K, -53%)  ← 仍然陡峭
v6: 137K → 130K ( -7K,  -5%)  ← 几乎无断崖 ✅
```

上下文从未飙到 200K+，在 ~137K 就开始温和截断。

### 3.2 grep_search 回归（16 次）

v5 为 0，v6 为 16。搜索了 `unwrap()|expect(`、`let _ =`、`from_utf8_lossy` 等反模式。prompt 引导生效。

### 3.3 Cache 断点首次验证成功

每轮 cached 数据首次可见（span 级输出修复生效）：

```
第  7 轮:  87K prompt, cached=6,091     ← 首次命中（system+user）
第  8 轮:  99K prompt, cached=6,091
第  9 轮: 110K prompt, cached=75,708    ← tool history 断点生效！69%命中
第 10 轮: 121K prompt, cached=75,708    ← 63%命中
第 11 轮: 131K prompt, cached=0         ← 截断后 cache 失效 ⚠️
...后续: cached=0~6,091（仅 system+user）
```

第 9-10 轮证明策略有效，但截断破坏了缓存连续性。

### 3.4 语言一致性

`build_constraints_section` 语言指令生效，3 条中间 output 全部中文。

## 4. 发现的问题

### 4.1 🔴 `take_note` 不在 code-reviewer 工具列表中

**根因**：`code-reviewer.md` 第 8 行：
```yaml
tools: read_file, list_directory, search_files, grep_search, get_file_info, bash
```

`take_note` 不在列表中。subagent 的 `build_filtered_tools` 只注入列表中的工具——所以模型根本没有 `take_note` 可用。v4 之所以能调用是因为 v4 trace 运行在工具列表修改之前。

**修复**：`tools:` 行添加 `take_note`。

**优先级**：P0

### 4.2 🔴 Cache 在截断后失效

截断改变了老轮次的 response 内容（添加 `...(truncated, N bytes)` 后缀），前缀不再匹配 → cache miss。

当前 cache 断点标记在 `rposition(|r| r.contains("...(truncated,"))` —— 即**最后一个**截断轮次。但这个轮次的截断级别可能在下轮从 moderate 变成 aggressive，内容再次改变。

**修复方案**：改为标记**第一个**截断轮次（最老的，已经到 micro 级，内容不会再变）。

**预期效果**：缓存率从 9.1% → 40-50%。

**优先级**：P0

### 4.3 🟠 上下文峰值只到 137K（过早截断）

`CHARS_PER_TOKEN=3` 过于保守。120K token 预算，按 3 chars/token 估算 = 360K chars 即触发截断。实际 JSON+代码混合的真实比率约 3.5 chars/token。

**修复**：用 `total * 2 / 7` 替代 `total / 3`（等效 3.5 chars/token）。

**优先级**：P1

### 4.4 🟠 venus_provider.rs 重叠读取 8 次

980 行文件被分成 8 个窗口读取（offset=1,70,201,300,350,499,500,870），大量重叠。模型过于保守地使用 offset/limit，对 <1000 行的文件应直接全文读取。

**修复**：code-reviewer prompt 中添加"文件 <1000 行时直接全文读取"。

**优先级**：P2

## 5. 待修复项汇总

| # | 问题 | 状态 | 优先级 |
|---|------|------|--------|
| 4.1 | `take_note` 不在 code-reviewer 工具列表 | ❌ 待修 | P0 |
| 4.2 | Cache 断点应改为最老截断轮次 | ❌ 待修 | P0 |
| 4.3 | CHARS_PER_TOKEN=3 过于保守 | ❌ 待修 | P1 |
| 4.4 | 大文件重叠读取 | ❌ 待修 | P2 |
