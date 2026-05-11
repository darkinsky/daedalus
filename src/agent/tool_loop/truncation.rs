//! Tool history truncation — budget-based dynamic truncation for tool-calling loops.
//!
//! Extracted from `tool_loop.rs` to reduce file size and improve separation of concerns.
//! This module handles the progressive truncation of older tool round results
//! to keep the context within the token budget.

use crate::llm::ToolRound;

/// Default number of recent tool rounds whose results are kept verbatim.
/// Used as fallback when no context budget is configured.
pub(crate) const DEFAULT_FULL_RESULT_RECENT_ROUNDS: usize = 3;

/// Default maximum character length for tool responses in "near" older rounds.
const DEFAULT_TRUNCATED_RESULT_MAX_CHARS: usize = 500;

/// Default maximum character length for tool responses in very old rounds.
const DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS: usize = 120;

/// Approximate token-to-character ratio for estimating token counts from
/// character lengths.
///
/// Set to 3 rather than 4 because tool history contains a high proportion
/// of JSON structure (`{"id":"...","type":"function","function":{...}}`),
/// where short keys and punctuation average ~3 chars per token. Using 4
/// underestimates the true token count, causing the truncation algorithm
/// to trigger too late — resulting in a steep "cliff" (e.g., 240K → 113K)
/// when it finally fires and has to truncate many rounds at once.
pub(crate) const CHARS_PER_TOKEN: usize = 3;

/// Configuration for how tool history is truncated before sending to the LLM.
///
/// ## Strategy: Budget-based dynamic truncation
///
/// Instead of truncating based on fixed round-distance thresholds (which
/// cause premature truncation when the context is far from full), this
/// uses a **token budget** approach:
///
/// 1. If total estimated history tokens < `budget_tokens` → keep everything
///    verbatim (no truncation at all).
/// 2. If over budget → progressively truncate from oldest rounds first,
///    using three tiers: moderate → aggressive → micro, until the total
///    fits within budget.
///
/// The most recent `min_recent_rounds` are **never** truncated, ensuring
/// the model always has full context for its immediate next decision.
#[derive(Debug, Clone)]
pub struct TruncationConfig {
    /// Token budget for the tool history portion of the context.
    /// When total estimated tokens are below this, no truncation occurs.
    /// A good default is ~60% of the model's context window.
    pub budget_tokens: usize,
    /// Minimum number of most-recent rounds that are never truncated.
    pub min_recent_rounds: usize,
    /// Truncation tier thresholds (in characters) applied from oldest first.
    /// tier 1 (moderate): first pass truncation limit.
    pub moderate_max_chars: usize,
    /// tier 2 (aggressive): second pass if still over budget.
    pub aggressive_max_chars: usize,
    /// tier 3 (micro): final pass for extreme over-budget situations.
    pub micro_max_chars: usize,
}

impl Default for TruncationConfig {
    fn default() -> Self {
        Self {
            budget_tokens: 40_000, // conservative for small context models
            min_recent_rounds: DEFAULT_FULL_RESULT_RECENT_ROUNDS,
            moderate_max_chars: DEFAULT_TRUNCATED_RESULT_MAX_CHARS,
            aggressive_max_chars: DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS,
            micro_max_chars: 60,
        }
    }
}

impl TruncationConfig {
    /// Build a truncation config scaled to a context window size (in tokens).
    ///
    /// Allocates ~60% of the context window to tool history budget.
    /// The remaining 40% is reserved for system prompt, user message,
    /// tool definitions, and the model's output.
    pub fn for_context_window(context_tokens: usize) -> Self {
        let budget = context_tokens * 60 / 100;

        if context_tokens >= 200_000 {
            // Large context (200K+): generous budget, gentle truncation
            Self {
                budget_tokens: budget,
                min_recent_rounds: 10,
                moderate_max_chars: 6000,
                aggressive_max_chars: 2000,
                micro_max_chars: 500,
            }
        } else if context_tokens >= 100_000 {
            // Medium context (100K-200K)
            Self {
                budget_tokens: budget,
                min_recent_rounds: 6,
                moderate_max_chars: 3000,
                aggressive_max_chars: 1000,
                micro_max_chars: 200,
            }
        } else {
            // Small context (<100K)
            Self {
                budget_tokens: budget,
                min_recent_rounds: DEFAULT_FULL_RESULT_RECENT_ROUNDS,
                moderate_max_chars: DEFAULT_TRUNCATED_RESULT_MAX_CHARS,
                aggressive_max_chars: DEFAULT_MICRO_TRUNCATED_RESULT_MAX_CHARS,
                micro_max_chars: 60,
            }
        }
    }

    /// Tighten the truncation config based on context pressure.
    ///
    /// When context usage exceeds 60%, progressively reduce the truncation
    /// budget to free up space for the model's output and new tool calls.
    /// This implements "Intra-Turn MicroCompact" — the tool history budget
    /// shrinks as context pressure increases.
    pub fn tighten_for_pressure(&self, context_usage_pct: u8) -> Self {
        if context_usage_pct <= 60 {
            return self.clone();
        }

        let pressure = (context_usage_pct as f64 - 60.0) / 40.0; // 0.0 ~ 1.0
        let reduction = (self.budget_tokens as f64 * pressure * 0.5) as usize;
        let new_budget = self.budget_tokens.saturating_sub(reduction);

        tracing::debug!(
            original_budget = self.budget_tokens,
            new_budget,
            pressure = %format!("{:.2}", pressure),
            "Tightening truncation budget due to context pressure"
        );

        TruncationConfig {
            budget_tokens: new_budget,
            // Under extreme pressure (>80%), reduce protected rounds to
            // free more space. Floor at 3 to always keep immediate context.
            min_recent_rounds: if context_usage_pct > 80 {
                3_usize.max(self.min_recent_rounds / 2)
            } else {
                self.min_recent_rounds
            },
            // Tighten truncation limits under pressure
            moderate_max_chars: if context_usage_pct > 70 {
                self.moderate_max_chars / 2
            } else {
                self.moderate_max_chars
            },
            aggressive_max_chars: if context_usage_pct > 70 {
                self.aggressive_max_chars / 2
            } else {
                self.aggressive_max_chars
            },
            micro_max_chars: self.micro_max_chars,
        }
    }
}

/// Build a context-efficient copy of tool history for the LLM.
///
/// ## Budget-based dynamic truncation
///
/// Unlike a fixed-tier approach (which truncates based on round distance
/// regardless of total size), this algorithm:
///
/// 1. **Estimates** the total token cost of the full history.
/// 2. If under budget → returns everything verbatim (zero truncation).
/// 3. If over budget → applies progressive truncation from the **oldest**
///    rounds first, through three tiers (moderate → aggressive → micro),
///    re-checking the budget after each tier.
///
/// The most recent `min_recent_rounds` are **never** truncated.
///
/// This ensures the context stays as close to the budget ceiling as possible
/// without premature truncation (the "150K peak then drop to 80K" problem).
///
/// Tool calls (function name + arguments) are always kept in full — they're
/// small and provide important structural context.
///
/// ## Information-density-aware truncation
///
/// Different tools produce outputs with different information densities:
/// - `grep_search`: Every line is a match — high density, preserve more
/// - `read_file`: Often contains imports/whitespace — lower density
/// - `bash`: Output varies, but structure info (head/tail) is most useful
///
/// The truncation limit is scaled by a per-tool multiplier to preserve
/// more content from high-density tools.
pub(crate) fn truncate_tool_history(history: &[ToolRound], cfg: &TruncationConfig) -> Vec<ToolRound> {
    if history.is_empty() {
        return Vec::new();
    }

    // Step 1: Estimate total tokens of the full (untruncated) history.
    let total_chars = estimate_history_chars(history);
    let estimated_tokens = total_chars / CHARS_PER_TOKEN;

    // Under budget? Return everything verbatim — no truncation needed.
    if estimated_tokens <= cfg.budget_tokens {
        return history.to_vec();
    }

    tracing::debug!(
        estimated_tokens,
        budget = cfg.budget_tokens,
        rounds = history.len(),
        "Tool history over budget, applying truncation"
    );

    // Step 2: Clone the history so we can mutate response content.
    let mut result: Vec<ToolRound> = history.to_vec();
    let len = result.len();

    // The most recent N rounds are protected from truncation.
    let protected_start = len.saturating_sub(cfg.min_recent_rounds);

    // Step 3: Truncate round-by-round from oldest to newest.
    //
    // For each unprotected round, try progressively harsher truncation
    // tiers until we're back within budget. This ensures we truncate
    // the *minimum* number of rounds needed, keeping newer rounds as
    // intact as possible.
    //
    // Previous approach applied each tier to ALL rounds simultaneously,
    // causing a "230K → 73K cliff". This per-round approach produces
    // a smooth decline: only the oldest 3-5 rounds get micro-truncated,
    // the rest stay at moderate or full.
    let tiers = [
        cfg.moderate_max_chars,
        cfg.aggressive_max_chars,
        cfg.micro_max_chars,
    ];

    for i in 0..protected_start {
        // Check budget before touching this round
        let current_chars = estimate_history_chars(&result);
        let current_tokens = current_chars / CHARS_PER_TOKEN;
        if current_tokens <= cfg.budget_tokens {
            break; // We're within budget — stop truncating.
        }

        // Pre-collect tool names for density-aware truncation (avoids borrow conflict)
        let tool_names: Vec<String> = result[i].calls.iter()
            .map(|c| c.function_name.clone())
            .collect();

        // Apply progressively harsher tiers to this single round
        for &tier_limit in &tiers {
            for (resp_idx, resp) in result[i].responses.iter_mut().enumerate() {
                // Information-density-aware truncation: scale the limit based
                // on the tool that produced this response. High-density tools
                // (grep_search) get more space; low-density tools (read_file)
                // get less.
                let tool_name = tool_names.get(resp_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let density_multiplier = tool_density_multiplier(tool_name);
                let effective_limit = (tier_limit as f64 * density_multiplier) as usize;

                if resp.content.len() > effective_limit {
                    // Use head+tail truncation for file content and bash output,
                    // which preserves both the beginning (imports/declarations)
                    // and end (implementations/results) of the output.
                    let truncated_content = if should_use_head_tail_truncation(tool_name) && effective_limit >= 200 {
                        truncate_head_tail(&resp.content, effective_limit)
                    } else {
                        let truncated = crate::tools::truncate_at_char_boundary(
                            &resp.content,
                            effective_limit,
                        );
                        format!(
                            "{}...(truncated, {} bytes total)",
                            truncated,
                            resp.content.len()
                        )
                    };
                    resp.content = truncated_content;
                }
            }

            // Re-check after each tier — stop as soon as we're within budget
            let after_chars = estimate_history_chars(&result);
            let after_tokens = after_chars / CHARS_PER_TOKEN;
            if after_tokens <= cfg.budget_tokens {
                break;
            }
        }
    }

    result
}

/// Return a truncation multiplier based on the tool's output information density.
///
/// Tools with higher information density (every line is valuable) get a higher
/// multiplier, meaning they retain more content during truncation. Tools with
/// lower density (lots of boilerplate, imports, whitespace) get a lower multiplier.
///
/// This ensures the same token budget preserves more useful information overall.
fn tool_density_multiplier(tool_name: &str) -> f64 {
    match tool_name {
        // High density: every line is a search match with context
        "grep_search" => 1.5,
        // High density: file listings are compact and structural
        "list_directory" | "search_files" => 1.3,
        // Medium-high: bash output varies but is often dense
        "bash" => 1.2,
        // Medium: file content has imports/whitespace padding
        "read_file" | "get_file_info" => 0.8,
        // Low density: edit results are confirmations, not information
        "edit_file" | "multi_edit" | "write_file" => 0.5,
        // Default: standard density
        _ => 1.0,
    }
}

/// Check if a tool produces output that benefits from head+tail truncation.
///
/// For tools like `read_file`, the beginning (imports, struct definitions) and
/// end (function implementations, tests) are both valuable. Simple head-only
/// truncation loses the tail, which often contains the most relevant code.
fn should_use_head_tail_truncation(tool_name: &str) -> bool {
    matches!(tool_name, "read_file" | "bash")
}

/// Truncate content preserving both head and tail portions.
///
/// Allocates 60% of the budget to the head (imports, declarations) and
/// 40% to the tail (implementations, tests). Inserts a clear separator
/// showing how many lines were omitted.
fn truncate_head_tail(content: &str, max_bytes: usize) -> String {
    let head_budget = max_bytes * 60 / 100;
    let tail_budget = max_bytes * 40 / 100;

    let head = crate::tools::truncate_at_char_boundary(content, head_budget);

    // Find the tail: start from the end, walk backwards to find a valid boundary
    let tail_start = if content.len() > tail_budget {
        let candidate = content.len() - tail_budget;
        // Find the next newline after candidate to avoid splitting a line
        content[candidate..].find('\n')
            .map(|pos| candidate + pos + 1)
            .unwrap_or(candidate)
    } else {
        0
    };
    let tail = &content[tail_start..];

    // Count omitted lines for the separator
    let head_end_line = head.matches('\n').count();
    let tail_start_line = content[..tail_start].matches('\n').count();
    let omitted_lines = tail_start_line.saturating_sub(head_end_line);

    format!(
        "{}\n\n... [{} lines omitted] ...\n\n{}",
        head, omitted_lines, tail
    )
}

/// Estimate the total character count of a tool history.
///
/// Counts tool call arguments + response content for each round.
/// This is a rough estimate used for budget comparison, not exact token counting.
///
/// For tool calls, we add a fixed overhead per call (~80 chars) to account
/// for the JSON wire format wrapping (`{"id":"...","type":"function",
/// "function":{"name":"...","arguments":...}}`). Without this, the estimate
/// is ~30% too low for tool-call-heavy histories.
pub(crate) fn estimate_history_chars(history: &[ToolRound]) -> usize {
    /// Fixed overhead per tool call for JSON structure (id, type, function wrapper).
    const TOOL_CALL_JSON_OVERHEAD: usize = 80;

    let mut total = 0;
    for round in history {
        for call in &round.calls {
            let args_len = estimate_json_len(&call.arguments);
            total += call.function_name.len() + args_len + TOOL_CALL_JSON_OVERHEAD;
        }
        for resp in &round.responses {
            total += resp.content.len();
        }
    }
    total
}

/// Estimate the serialized length of a JSON value without allocating.
///
/// This avoids `value.to_string()` which allocates a new String on every call.
/// The estimate is approximate but sufficient for budget comparison.
fn estimate_json_len(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => if *b { 4 } else { 5 },
        serde_json::Value::Number(n) => {
            // Most numbers are < 10 digits
            let s = n.to_string();
            s.len()
        }
        serde_json::Value::String(s) => s.len() + 2, // quotes
        serde_json::Value::Array(arr) => {
            let inner: usize = arr.iter().map(estimate_json_len).sum();
            inner + arr.len().saturating_sub(1) + 2 // commas + brackets
        }
        serde_json::Value::Object(map) => {
            let inner: usize = map.iter()
                .map(|(k, v)| k.len() + 2 + 1 + estimate_json_len(v)) // "key":value
                .sum();
            inner + map.len().saturating_sub(1) + 2 // commas + braces
        }
    }
}
