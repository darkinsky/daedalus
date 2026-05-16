//! Context compression (compact) algorithm.
//!
//! This module contains the pure algorithmic logic for compressing conversation
//! history. It operates on `CompactInput` → `CompactOutput` without needing
//! direct access to `SlidingWindowMemory` fields, preserving encapsulation.

use crate::llm::{ChatMessage, LlmApi};

use super::config::SlidingWindowConfig;
use super::memory::CompactResult;

/// Content prefix that identifies a compact boundary message.
///
/// When compact runs, it produces a summary message with this prefix.
/// On subsequent compacts, we look for this marker to find the boundary
/// and only compress messages *after* it (incremental compression).
pub(super) const COMPACT_BOUNDARY_PREFIX: &str = "[Previous conversation context \u{2014}";

/// Input data for the compact algorithm.
///
/// Borrows from `SlidingWindowMemory` fields so the algorithm can run
/// as a pure function without mutating the struct directly.
pub(super) struct CompactInput<'a> {
    pub messages: &'a [ChatMessage],
    pub config: &'a SlidingWindowConfig,
}

/// Output produced by the compact algorithm.
///
/// The caller (`SlidingWindowMemory::compact`) applies these results
/// to its own fields — this is the only place that mutates state.
pub(super) struct CompactOutput {
    /// The new message list after compression.
    pub new_messages: Vec<ChatMessage>,
    /// The new consolidation cursor value.
    pub new_consolidation_cursor: usize,
    /// Statistics about the compact operation.
    pub result: CompactResult,
}

/// Run the compact algorithm on the given messages.
///
/// This is the core implementation extracted from `SlidingWindowMemory::compact_with_range`.
/// It takes immutable input, calls the LLM, and returns the new state without
/// mutating anything — the caller is responsible for applying the output.
///
/// ## Arguments
/// * `input` — Borrowed message list and config.
/// * `llm` — The LLM provider for generating the summary.
/// * `custom_instruction` — Optional user instruction to focus the summary.
/// * `range` — Optional `(start, end)` range of message indices to compress.
/// * `estimated_tokens_before` — Pre-computed token count before compact.
/// * `estimate_tokens_fn` — Callback to estimate tokens for the new message list.
pub(super) async fn run_compact(
    input: CompactInput<'_>,
    llm: &dyn LlmApi,
    custom_instruction: Option<&str>,
    range: Option<(usize, usize)>,
    estimated_tokens_before: usize,
    estimate_tokens_fn: impl Fn(&[ChatMessage]) -> usize,
) -> anyhow::Result<CompactOutput> {
    let total_messages = input.messages.len();

    // Determine the compressible range.
    let (range_start, range_end) = match range {
        Some((s, e)) => (s.min(total_messages), e.min(total_messages)),
        None => {
            // Default: compress everything except the most recent N.
            let preserve_count = input.config.compact_preserve_recent.min(total_messages);
            (0, total_messages - preserve_count)
        }
    };

    if range_start >= range_end {
        return Ok(CompactOutput {
            new_messages: input.messages.to_vec(),
            new_consolidation_cursor: 0,
            result: CompactResult {
                messages_before: total_messages,
                messages_after: total_messages,
                estimated_tokens_before,
                estimated_tokens_after: estimated_tokens_before,
            },
        });
    }

    // Within the compressible range, find the compact boundary.
    let boundary_idx = input.messages[range_start..range_end]
        .iter()
        .rposition(|msg| msg.content.starts_with(COMPACT_BOUNDARY_PREFIX))
        .map(|idx| range_start + idx);

    // Extract previous summary and determine actual compress start.
    let (previous_summary, compress_start) = match boundary_idx {
        Some(idx) => {
            let summary = input.messages[idx].content.clone();
            (Some(summary), idx + 1)
        }
        None => (None, range_start),
    };

    // Separate messages into: to-compress vs semantically-preserved.
    // Messages with `preserved = true` within the compress range are
    // extracted and kept verbatim.
    let mut messages_to_compress = Vec::new();
    let mut preserved_in_range = Vec::new();

    for (i, msg) in input.messages[compress_start..range_end].iter().enumerate() {
        if msg.preserved {
            preserved_in_range.push((compress_start + i, msg.clone()));
        } else {
            messages_to_compress.push(msg);
        }
    }

    let preserved_semantic_count = preserved_in_range.len();

    // If there's nothing to compress, skip the LLM call.
    if messages_to_compress.is_empty() {
        return Ok(CompactOutput {
            new_messages: input.messages.to_vec(),
            new_consolidation_cursor: 0,
            result: CompactResult {
                messages_before: total_messages,
                messages_after: total_messages,
                estimated_tokens_before,
                estimated_tokens_after: estimate_tokens_fn(input.messages),
            },
        });
    }

    // Build text representation of messages to compress.
    let messages_text = messages_to_compress
        .iter()
        .map(|msg| format!("[{}]: {}", msg.role, msg.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let user_prompt = super::prompts::compact_user_prompt(
        &messages_text,
        custom_instruction,
        previous_summary.as_deref(),
    );

    let llm_messages = vec![
        ChatMessage::system(
            input.config.compact_custom_prompt.clone()
                .unwrap_or_else(|| super::prompts::COMPACT_SYSTEM_PROMPT.to_string())
        ),
        ChatMessage::user(user_prompt),
    ];

    let response = llm.chat(&llm_messages, None).await?;
    let summary = parse_compact_response(&response.content);

    // Rebuild the message list:
    // [before_range] + [compact_summary] + [preserved_in_range] + [after_range]
    let before_range: Vec<ChatMessage> = if range_start > 0 {
        input.messages[..range_start].to_vec()
    } else {
        Vec::new()
    };

    let after_range: Vec<ChatMessage> = input.messages[range_end..].to_vec();

    let compressed_count = messages_to_compress.len()
        + if boundary_idx.is_some() { 1 } else { 0 }; // boundary is also consumed

    let mut new_messages = Vec::new();
    new_messages.extend(before_range);
    new_messages.push(ChatMessage::user(format!(
        "[Previous conversation context \u{2014} {} messages compressed into summary]\n\n{}",
        compressed_count, summary,
    )));
    // Re-insert semantically preserved messages (in original order).
    for (_orig_idx, msg) in &preserved_in_range {
        new_messages.push(msg.clone());
    }
    new_messages.extend(after_range);

    let estimated_tokens_after = estimate_tokens_fn(&new_messages);

    let is_incremental = previous_summary.is_some();
    let is_partial = range.is_some();
    tracing::info!(
        messages_before = total_messages,
        messages_after = new_messages.len(),
        tokens_before = estimated_tokens_before,
        tokens_after = estimated_tokens_after,
        compressed = compressed_count,
        preserved_semantic = preserved_semantic_count,
        incremental = is_incremental,
        partial = is_partial,
        "Context compact complete"
    );

    Ok(CompactOutput {
        result: CompactResult {
            messages_before: total_messages,
            messages_after: new_messages.len(),
            estimated_tokens_before,
            estimated_tokens_after,
        },
        new_messages,
        new_consolidation_cursor: 1,
    })
}

/// Parse the compact LLM response, extracting the summary content.
///
/// Tries to extract content between `<compact_summary>` tags.
/// Falls back to using the entire response if tags are not found.
pub(super) fn parse_compact_response(response: &str) -> String {
    let response = response.trim();

    // Try to extract content between <compact_summary> tags
    if let Some(start) = response.find("<compact_summary>") {
        let content_start = start + "<compact_summary>".len();
        if let Some(end) = response[content_start..].find("</compact_summary>") {
            return response[content_start..content_start + end].trim().to_string();
        }
    }

    // Fallback: use the entire response
    response.to_string()
}
