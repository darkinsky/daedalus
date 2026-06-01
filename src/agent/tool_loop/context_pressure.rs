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

use super::truncation::{estimate_history_chars, TruncationConfig, CHARS_PER_TOKEN};
use super::{LoopConfig, LoopContext, LoopOutcome, LoopResult};

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
