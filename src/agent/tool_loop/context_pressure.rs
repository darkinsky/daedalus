//! Context pressure awareness — monitors and reacts to context window usage.
//!
//! Extracted from `tool_loop.rs` to reduce file size and improve separation of concerns.
//! This module handles estimating context usage, generating budget hints,
//! and forcing final responses when the context budget is exceeded.

use anyhow::Result;

use crate::llm::{
    ChatMessage, LlmApi, TokenUsage, ToolResponse, ToolRound,
};
use crate::tools::{ToolEvent, ToolEventCallback};

use super::truncation::{estimate_history_chars, estimate_json_len, TruncationConfig, CHARS_PER_TOKEN};
use super::{LoopConfig, LoopContext, LoopOutcome, LoopResult};

// ── Context Health (Context Rot detection) ──

/// Context health assessment — quantifies how "rotted" the context is.
///
/// Context Rot occurs when the context window fills with stale information
/// (old tool outputs, resolved issues, superseded code snippets), diluting
/// the attention weight on fresh, relevant information. This struct captures
/// the key signals that indicate rot is occurring.
#[derive(Debug, Clone)]
pub(crate) struct ContextHealth {
    /// Percentage of estimated total tokens occupied by tool history (0-100).
    pub tool_history_pct: u8,
    /// Number of tool-loop rounds since the session started (proxy for staleness).
    pub round_number: usize,
    /// Estimated "staleness" — ratio of old content (rounds older than
    /// `STALENESS_THRESHOLD` rounds ago) to total tool history chars.
    /// Range: 0.0 (all fresh) to 1.0 (all stale).
    pub staleness_ratio: f32,
    /// Overall context usage percentage (0-100).
    pub context_usage_pct: u8,
    /// Severity level derived from the above signals.
    pub severity: ContextHealthSeverity,
}

/// Severity levels for context health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ContextHealthSeverity {
    /// Context is healthy — no action needed.
    Healthy,
    /// Early signs of rot — inject a gentle reminder.
    Mild,
    /// Significant rot — inject a strong warning with specific metrics.
    Moderate,
    /// Severe rot — strongly recommend /compact.
    Severe,
}

/// Rounds older than this threshold (relative to current round) are
/// considered "stale" for staleness_ratio calculation.
const STALENESS_THRESHOLD: usize = 8;

/// Assess the health of the current context.
///
/// Computes tool_history_pct, staleness_ratio, and derives a severity level.
/// This replaces the previous ad-hoc `round_number >= 8 && context_usage_pct >= 40`
/// check with a structured, multi-signal assessment.
pub(crate) fn assess_context_health(
    tool_history: &[ToolRound],
    round_number: usize,
    context_usage_pct: u8,
    context_window_tokens: Option<usize>,
) -> ContextHealth {
    let cw = context_window_tokens.unwrap_or(128_000).max(1);

    // 1. Tool history percentage of total context
    let history_chars = estimate_history_chars(tool_history);
    let history_tokens = history_chars / CHARS_PER_TOKEN;
    let tool_history_pct = ((history_tokens * 100) / cw).min(100) as u8;

    // 2. Staleness ratio — what fraction of tool history chars comes from
    //    rounds older than STALENESS_THRESHOLD rounds ago?
    let staleness_ratio = if tool_history.is_empty() || round_number <= STALENESS_THRESHOLD {
        0.0
    } else {
        let total_len = tool_history.len();
        let stale_cutoff = total_len.saturating_sub(STALENESS_THRESHOLD);
        let stale_chars: usize = tool_history[..stale_cutoff].iter().map(|round| {
            let call_chars: usize = round.calls.iter()
                .map(|c| c.function_name.len() + estimate_json_len(&c.arguments) + 80)
                .sum();
            let resp_chars: usize = round.responses.iter()
                .map(|r| r.content.len())
                .sum();
            call_chars + resp_chars
        }).sum();

        if history_chars == 0 {
            0.0
        } else {
            (stale_chars as f32 / history_chars as f32).min(1.0)
        }
    };

    // 3. Derive severity from multiple signals
    let severity = derive_severity(
        tool_history_pct,
        staleness_ratio,
        round_number,
        context_usage_pct,
    );

    ContextHealth {
        tool_history_pct,
        round_number,
        staleness_ratio,
        context_usage_pct,
        severity,
    }
}

/// Derive context health severity from multiple signals.
///
/// Uses a weighted scoring approach rather than simple threshold checks,
/// so that multiple mild signals can combine into a higher severity.
fn derive_severity(
    tool_history_pct: u8,
    staleness_ratio: f32,
    round_number: usize,
    context_usage_pct: u8,
) -> ContextHealthSeverity {
    // Score each signal on a 0-3 scale
    let history_score = match tool_history_pct {
        0..=15 => 0,
        16..=30 => 1,
        31..=45 => 2,
        _ => 3,
    };

    let staleness_score = if staleness_ratio < 0.2 {
        0
    } else if staleness_ratio < 0.5 {
        1
    } else if staleness_ratio < 0.75 {
        2
    } else {
        3
    };

    let round_score = match round_number {
        0..=5 => 0,
        6..=12 => 1,
        13..=20 => 2,
        _ => 3,
    };

    let usage_score = match context_usage_pct {
        0..=39 => 0,
        40..=59 => 1,
        60..=79 => 2,
        _ => 3,
    };

    // Weighted total: tool_history and staleness are the strongest signals
    let total = history_score * 3 + staleness_score * 3 + round_score * 2 + usage_score * 2;

    match total {
        0..=4 => ContextHealthSeverity::Healthy,
        5..=10 => ContextHealthSeverity::Mild,
        11..=18 => ContextHealthSeverity::Moderate,
        _ => ContextHealthSeverity::Severe,
    }
}

/// Generate a context health hint message based on the health assessment.
///
/// Returns `None` if the context is healthy (no hint needed).
/// Unlike the simple `context_budget_hint`, this provides specific metrics
/// to help the LLM understand *why* its context is degrading.
pub(crate) fn context_health_hint(health: &ContextHealth) -> Option<String> {
    match health.severity {
        ContextHealthSeverity::Healthy => None,
        ContextHealthSeverity::Mild => {
            Some(format!(
                "\n[CONTEXT HEALTH] Mild context aging detected \
                 (tool history: {}% of context, staleness: {:.0}%, round: {}). \
                 Be concise in responses and tool usage to maintain output quality.",
                health.tool_history_pct,
                health.staleness_ratio * 100.0,
                health.round_number,
            ))
        }
        ContextHealthSeverity::Moderate => {
            Some(format!(
                "\n[CONTEXT HEALTH WARNING] Significant context rot detected:\n\
                 - Tool history occupies {}% of context (threshold: 30%)\n\
                 - {:.0}% of tool history is stale (>{} rounds old)\n\
                 - Round {}, context {}% full\n\
                 Action: Be very concise. Avoid re-reading files already explored. \
                 Focus on synthesizing existing findings. \
                 The user can run /compact to refresh the context window.",
                health.tool_history_pct,
                health.staleness_ratio * 100.0,
                STALENESS_THRESHOLD,
                health.round_number,
                health.context_usage_pct,
            ))
        }
        ContextHealthSeverity::Severe => {
            Some(format!(
                "\n[CONTEXT HEALTH CRITICAL] Severe context rot — output quality is likely degraded:\n\
                 - Tool history: {}% of context\n\
                 - Staleness: {:.0}% of history is outdated\n\
                 - Round {}, context {}% full\n\
                 STRONGLY RECOMMENDED: The user should run /compact to compress stale context. \
                 Until then, do NOT read new files or make exploratory tool calls. \
                 Only use tools if absolutely critical. Synthesize what you already know.",
                health.tool_history_pct,
                health.staleness_ratio * 100.0,
                health.round_number,
                health.context_usage_pct,
            ))
        }
    }
}

/// Estimate the current context usage as a percentage (0-100).
///
/// Combines the pre-built messages (system + history + user) with the
/// tool history to estimate total token consumption relative to the
/// context window size.
///
/// Uses CJK-aware token estimation for messages (which may contain natural
/// language) and code-mode estimation for tool history (which is JSON-heavy).
///
/// Returns 0 if `context_window_tokens` is `None` (feature disabled).
pub(crate) fn estimate_context_usage_pct(
    messages: &[ChatMessage],
    tool_history: &[ToolRound],
    context_window_tokens: Option<usize>,
) -> u8 {
    let cw = match context_window_tokens {
        Some(cw) if cw > 0 => cw,
        _ => return 0,
    };

    // Estimate tokens from pre-built messages using CJK-aware estimation.
    // Each message has ~20 chars of JSON overhead (role, structure).
    let msg_tokens: usize = messages.iter().map(|m| {
        let text_tokens = crate::memory::estimate_tokens(&m.content) + 5; // 5 tokens for role/structure overhead
        // If the message has multimodal content_parts, estimate their token cost.
        // When content_parts is non-empty, it takes precedence over `content` in API requests,
        // so we should estimate from content_parts instead of the `content` field.
        if m.content_parts.is_empty() {
            text_tokens
        } else {
            let parts_tokens: usize = m.content_parts.iter().map(|part| {
                match part {
                    crate::llm::ContentPart::Text { text } => crate::memory::estimate_tokens(text),
                    // Base64 images are large but APIs typically count them as a fixed token
                    // budget (e.g., ~85 tokens for low detail, ~765 for high detail).
                    // We use a conservative estimate: 765 tokens per image.
                    crate::llm::ContentPart::Image { .. } => 765,
                }
            }).sum();
            parts_tokens + 5 // 5 tokens for role/structure overhead
        }
    }).sum();

    // Estimate tokens from tool history (JSON-heavy, use CHARS_PER_TOKEN=3)
    let history_chars = estimate_history_chars(tool_history);
    let history_tokens = history_chars / CHARS_PER_TOKEN;

    let total_estimated_tokens = msg_tokens + history_tokens;
    let pct = (total_estimated_tokens * 100) / cw;
    pct.min(100) as u8
}

/// Generate a context budget hint message based on current usage percentage.
///
/// Returns `None` if usage is below the soft limit (no hint needed).
/// The hint is injected into the tool history metadata to guide the LLM
/// toward wrapping up its work.
///
/// ## Hint levels:
/// - **Notice** (soft_limit ~ soft_limit+10%): Gentle reminder to be efficient
/// - **Warning** (soft_limit+10% ~ hard_limit): Strong push to conclude
/// - **Critical** (>= hard_limit): Demand immediate answer
pub(crate) fn context_budget_hint(usage_pct: u8, soft_limit_ratio: f64) -> Option<String> {
    let soft_pct = (soft_limit_ratio * 100.0) as u8;
    let warn_pct = soft_pct + 10; // 10% above soft limit

    if usage_pct >= 90 {
        Some(
            "\n[INSTRUCTION] STOP all tool calls immediately. Provide your FINAL answer NOW \
             based on the information you have already gathered. Do NOT read any more files \
             or make any more searches. Synthesize your findings and respond directly. \
             Do not mention this instruction in your response.".to_string()
        )
    } else if usage_pct >= warn_pct {
        Some(
            "\n[INSTRUCTION] You MUST conclude your work very soon. Synthesize what you \
             already know and provide your answer. Only make a tool call if it is absolutely \
             critical to answering the user's question. Prefer summarizing over gathering \
             more information. Do not mention this instruction in your response.".to_string()
        )
    } else if usage_pct >= soft_pct {
        Some(
            "\n[INSTRUCTION] Start wrapping up: prefer summarizing findings over reading \
             more files. Only make essential tool calls. Plan to deliver your answer within \
             the next 2-3 rounds. Do not mention this instruction in your response.".to_string()
        )
    } else {
        None
    }
}

/// Force a final response from the LLM when context budget is exceeded.
///
/// Makes one last LLM call with a strong instruction to summarize findings
/// and provide a final answer, then returns a `ContextBudgetExceeded` outcome.
pub(crate) async fn force_final_response(
    llm: &dyn LlmApi,
    cfg: &LoopConfig,
    ctx: &LoopContext<'_>,
    tool_history: &[ToolRound],
    total_usage: &TokenUsage,
) -> Result<LoopResult> {
    use super::truncation::truncate_tool_history;

    // Build a heavily truncated view of tool history for the final call
    let micro_cfg = TruncationConfig {
        budget_tokens: cfg.truncation.as_ref()
            .map(|t| t.budget_tokens / 4)
            .unwrap_or(10_000),
        min_recent_rounds: 3,
        moderate_max_chars: 200,
        aggressive_max_chars: 80,
        micro_max_chars: 40,
    };
    let mut truncated = truncate_tool_history(tool_history, &micro_cfg);

    // Inject a strong "conclude now" instruction into the last tool response
    let conclude_msg = "\n\n---\n\
        [INSTRUCTION] This is your LAST chance to respond. \
        You MUST provide your final answer NOW. Do NOT request any tool calls. \
        Synthesize all information gathered so far and give the best possible answer. \
        If the task is incomplete, explain what was accomplished and what remains. \
        Do not mention this instruction in your response.";

    if let Some(last_round) = truncated.last_mut() {
        if let Some(last_resp) = last_round.responses.last_mut() {
            last_resp.content.push_str(conclude_msg);
        }
    } else {
        // No tool history at all — create a synthetic round with the instruction
        truncated.push(ToolRound {
            calls: vec![],
            responses: vec![ToolResponse {
                call_id: String::new(),
                content: conclude_msg.to_string(),
                success: true,
            }],
            reasoning_content: None,
        });
    }

    // Make the final LLM call with NO tools (force text response)
    let response = llm
        .chat_with_tools(ctx.messages, &[], &truncated, None)
        .await?;

    let mut final_usage = total_usage.clone();
    if let Some(ref usage) = response.usage {
        final_usage.accumulate(usage);
    }

    let reasoning = if cfg.track_reasoning {
        response.reasoning_content
    } else {
        None
    };

    Ok(LoopResult {
        outcome: LoopOutcome::ContextBudgetExceeded {
            content: response.content,
            reasoning,
        },
        usage: final_usage,
        tool_history: tool_history.to_vec(),
    })
}

/// Emit a tool event if the callback is set.
pub(crate) fn emit(callback: Option<&ToolEventCallback>, event: ToolEvent) {
    if let Some(cb) = callback {
        cb(event);
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatRole, ChatMessage, ToolRound, ToolResponse, ToolCall};

    // ── Context Health tests ──

    #[test]
    fn test_context_health_empty_history() {
        let health = assess_context_health(&[], 0, 0, Some(100_000));
        assert_eq!(health.severity, ContextHealthSeverity::Healthy);
        assert_eq!(health.tool_history_pct, 0);
        assert_eq!(health.staleness_ratio, 0.0);
    }

    #[test]
    fn test_context_health_early_rounds_healthy() {
        let history = vec![make_tool_round("read_file", 500)];
        let health = assess_context_health(&history, 2, 10, Some(100_000));
        assert_eq!(health.severity, ContextHealthSeverity::Healthy);
    }

    #[test]
    fn test_context_health_moderate_with_stale_history() {
        // 20 rounds of tool history, each with 5K chars of output
        let history: Vec<ToolRound> = (0..20)
            .map(|_| make_tool_round("read_file", 5_000))
            .collect();
        let health = assess_context_health(&history, 20, 60, Some(100_000));
        assert!(
            health.severity >= ContextHealthSeverity::Mild,
            "Expected at least Mild, got {:?}",
            health.severity
        );
        assert!(health.staleness_ratio > 0.3, "Expected staleness > 0.3, got {}", health.staleness_ratio);
    }

    #[test]
    fn test_context_health_severe_with_high_usage() {
        // 30 rounds, large tool outputs, high context usage
        let history: Vec<ToolRound> = (0..30)
            .map(|_| make_tool_round("read_file", 10_000))
            .collect();
        let health = assess_context_health(&history, 30, 85, Some(100_000));
        assert!(
            health.severity >= ContextHealthSeverity::Moderate,
            "Expected at least Moderate, got {:?}",
            health.severity
        );
    }

    #[test]
    fn test_context_health_hint_none_when_healthy() {
        let health = ContextHealth {
            tool_history_pct: 5,
            round_number: 2,
            staleness_ratio: 0.0,
            context_usage_pct: 10,
            severity: ContextHealthSeverity::Healthy,
        };
        assert!(context_health_hint(&health).is_none());
    }

    #[test]
    fn test_context_health_hint_moderate_has_metrics() {
        let health = ContextHealth {
            tool_history_pct: 35,
            round_number: 15,
            staleness_ratio: 0.6,
            context_usage_pct: 55,
            severity: ContextHealthSeverity::Moderate,
        };
        let hint = context_health_hint(&health).unwrap();
        assert!(hint.contains("35%"), "Should contain tool_history_pct");
        assert!(hint.contains("60%"), "Should contain staleness ratio");
        assert!(hint.contains("/compact"), "Should mention /compact");
    }

    #[test]
    fn test_context_health_hint_severe_has_strong_language() {
        let health = ContextHealth {
            tool_history_pct: 50,
            round_number: 25,
            staleness_ratio: 0.8,
            context_usage_pct: 80,
            severity: ContextHealthSeverity::Severe,
        };
        let hint = context_health_hint(&health).unwrap();
        assert!(hint.contains("CRITICAL"), "Should contain CRITICAL");
        assert!(hint.contains("STRONGLY RECOMMENDED"), "Should contain strong recommendation");
    }

    #[test]
    fn test_derive_severity_weighted_scoring() {
        // All signals low → Healthy
        assert_eq!(derive_severity(10, 0.1, 3, 20), ContextHealthSeverity::Healthy);
        // Mixed signals → Mild or Moderate
        assert!(derive_severity(25, 0.4, 10, 50) >= ContextHealthSeverity::Mild);
        // All signals high → Severe
        assert_eq!(derive_severity(50, 0.9, 25, 85), ContextHealthSeverity::Severe);
    }

    fn make_tool_round(tool_name: &str, response_len: usize) -> ToolRound {
        ToolRound {
            calls: vec![ToolCall {
                call_id: "1".to_string(),
                function_name: tool_name.to_string(),
                arguments: serde_json::json!({"path": "/foo/bar.rs"}),
            }],
            responses: vec![ToolResponse {
                call_id: "1".to_string(),
                content: "x".repeat(response_len),
                success: true,
            }],
            reasoning_content: None,
        }
    }

    #[test]
    fn test_estimate_context_usage_pct_disabled() {
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "Hello".to_string(),
            content_parts: vec![],
            cache_control: None,
            preserved: false,
        }];
        assert_eq!(estimate_context_usage_pct(&messages, &[], None), 0);
    }

    #[test]
    fn test_estimate_context_usage_pct_empty() {
        let messages: Vec<ChatMessage> = vec![];
        let pct = estimate_context_usage_pct(&messages, &[], Some(100_000));
        assert_eq!(pct, 0);
    }

    #[test]
    fn test_estimate_context_usage_pct_basic() {
        let content = "x".repeat(30_000); // 30K ASCII chars → 7500 tokens (30K/4) + 5 overhead
        let messages = vec![ChatMessage {
            role: ChatRole::System,
            content,
            content_parts: vec![],
            cache_control: None,
            preserved: false,
        }];
        let pct = estimate_context_usage_pct(&messages, &[], Some(100_000));
        assert!(pct >= 6 && pct <= 10, "Expected ~7.5%, got {}%", pct);
    }

    #[test]
    fn test_estimate_context_usage_pct_with_tool_history() {
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "short".to_string(),
            content_parts: vec![],
            cache_control: None,
            preserved: false,
        }];
        let tool_history = vec![ToolRound {
            calls: vec![ToolCall {
                call_id: "1".to_string(),
                function_name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "/foo/bar.rs"}),
            }],
            responses: vec![ToolResponse {
                call_id: "1".to_string(),
                content: "x".repeat(60_000),
                success: true,
            }],
            reasoning_content: None,
        }];
        let pct = estimate_context_usage_pct(&messages, &tool_history, Some(100_000));
        assert!(pct >= 18 && pct <= 25, "Expected ~20%, got {}%", pct);
    }

    #[test]
    fn test_estimate_context_usage_pct_caps_at_100() {
        let content = "x".repeat(500_000);
        let messages = vec![ChatMessage {
            role: ChatRole::System,
            content,
            content_parts: vec![],
            cache_control: None,
            preserved: false,
        }];
        let pct = estimate_context_usage_pct(&messages, &[], Some(10_000));
        assert_eq!(pct, 100);
    }

    #[test]
    fn test_context_budget_hint_below_threshold() {
        assert!(context_budget_hint(50, 0.7).is_none());
        assert!(context_budget_hint(69, 0.7).is_none());
    }

    #[test]
    fn test_context_budget_hint_notice_level() {
        let hint = context_budget_hint(70, 0.7);
        assert!(hint.is_some());
        let text = hint.unwrap();
        assert!(text.contains("[INSTRUCTION]"));
        assert!(text.contains("wrapping up"));
    }

    #[test]
    fn test_context_budget_hint_warning_level() {
        let hint = context_budget_hint(82, 0.7);
        assert!(hint.is_some());
        let text = hint.unwrap();
        assert!(text.contains("[INSTRUCTION]"));
        assert!(text.contains("conclude"));
    }

    #[test]
    fn test_context_budget_hint_critical_level() {
        let hint = context_budget_hint(90, 0.7);
        assert!(hint.is_some());
        let text = hint.unwrap();
        assert!(text.contains("[INSTRUCTION]"));
        assert!(text.contains("STOP"));
        assert!(text.contains("FINAL answer"));
    }

    #[test]
    fn test_context_budget_hint_custom_soft_limit() {
        let hint = context_budget_hint(50, 0.5);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("[INSTRUCTION]"));

        let hint = context_budget_hint(62, 0.5);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("conclude"));
    }

    #[test]
    fn test_dynamic_truncation_tightening() {
        let base_cfg = TruncationConfig::for_context_window(200_000);
        let original_budget = base_cfg.budget_tokens;

        let tightened = base_cfg.tighten_for_pressure(80);

        assert!(tightened.budget_tokens < original_budget);
        assert!(tightened.budget_tokens > original_budget / 2);
        let expected_ratio = 0.75;
        let actual_ratio = tightened.budget_tokens as f64 / original_budget as f64;
        assert!(
            (actual_ratio - expected_ratio).abs() < 0.05,
            "Expected ~75% of original budget, got {:.1}%",
            actual_ratio * 100.0
        );
    }
}
