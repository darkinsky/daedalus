//! Memory middleware — conversation history management for turns.
//!
//! Handles the full memory lifecycle around each turn:
//! - **Before**: `add_user_message()` + `build_messages()` → fills `request.messages`
//! - **After**: `add_tool_context()` + `add_assistant_message()` + `reflect_on_turn()`
//!
//! This middleware should be the **innermost** turn middleware so that
//! messages are built right before the core handler, and results stored
//! immediately after.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::SharedMemory;
use crate::llm::LlmApi;
use crate::tools::truncate_at_char_boundary;

use super::super::{TurnMiddleware, TurnNext, TurnRequest, TurnResponse};

/// Turn-level memory middleware.
///
/// Wraps the core handler with memory read/write operations:
/// 1. **Before**: adds user message to memory, builds the message list
/// 2. **After**: stores tool context + assistant response, triggers reflection
pub struct MemoryTurnMiddleware {
    /// Shared memory handle (same Arc as Session holds).
    memory: SharedMemory,
    /// LLM reference for post-turn reflection (some strategies use it).
    llm: Arc<dyn LlmApi>,
}

impl MemoryTurnMiddleware {
    /// Create a new memory middleware.
    pub fn new(memory: SharedMemory, llm: Arc<dyn LlmApi>) -> Self {
        Self { memory, llm }
    }
}

#[async_trait]
impl TurnMiddleware for MemoryTurnMiddleware {
    async fn handle<'a>(
        &self,
        mut request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        // Save user_input before it's consumed by the pipeline
        let user_input = request.user_input.to_string();

        // ── Before: build messages from memory ──
        {
            let mut mem = self.memory.lock().await;
            mem.add_user_message(request.user_input);
            request.messages = mem.build_messages();
        }

        // If a multimodal ChatMessage was passed via extensions (from chat_with_message),
        // replace the last user message's content_parts so the LLM receives image data.
        if let Some(multimodal_msg) = request.extensions.get::<crate::llm::ChatMessage>() {
            if multimodal_msg.has_content_parts() {
                // Find the last user message and inject content_parts
                if let Some(last_user) = request.messages.iter_mut().rev()
                    .find(|m| m.role == crate::llm::ChatRole::User)
                {
                    last_user.content_parts = multimodal_msg.content_parts.clone();
                }
            }
        }

        // ── Delegate to core ──
        let mut response = next.run(request).await?;

        // ── After: store results and trigger reflection in a single lock ──
        //
        // Previously this was split into two separate lock acquisitions,
        // which could allow intermediate state inconsistency if concurrent
        // access were ever introduced. Merging into one lock is both more
        // efficient (one await instead of two) and safer.
        {
            let mut mem = self.memory.lock().await;

            // Store tool context summary if tools were used
            if !response.tool_history.is_empty() {
                let summary = summarize_tool_history(&response.tool_history);
                mem.add_tool_context(&summary);
            }

            // Store assistant response
            mem.add_assistant_message(&response.chat_response.content);

            // Notify memory about cache hit status for cache-aware micro_compact.
            // On the next build_messages() call, micro_compact will use a larger
            // preserve window if the cache was warm, avoiding prefix invalidation.
            if let Some(ref usage) = response.chat_response.usage {
                let cached = usage.cached_tokens.unwrap_or(0);
                mem.notify_cache_status(cached);
            }

            // Trigger post-turn reflection (some memory strategies use LLM)
            mem.reflect_on_turn(
                &user_input,
                &response.chat_response.content,
                &*self.llm,
            )
            .await;

            // Trigger automatic consolidation if threshold is reached.
            // This extracts key facts into long-term memory and appends
            // a summary to the history log.
            //
            // Note: consolidation updates long-term memory which changes the
            // system prompt's dynamic suffix, invalidating prompt cache.
            // We track whether it ran so we can skip compact in the same turn
            // to avoid a double cache invalidation.
            let consolidation_ran = mem.should_consolidate();
            mem.maybe_consolidate(&*self.llm).await;

            // Trigger automatic context compression if the context window
            // is approaching the token budget. This compresses older messages
            // into a summary to prevent context overflow.
            //
            // Multi-level threshold logic:
            // - Normal/Warning: no compact needed
            // - High: compact, but skip if consolidation just ran (avoid double cache miss)
            // - Critical: FORCE compact even if consolidation just ran (context overflow risk)
            let pressure = mem.context_pressure_level();
            let should_force = pressure >= crate::memory::ContextPressureLevel::Critical;

            // Log context pressure for observability (visible in verbose/tracing mode)
            match pressure {
                crate::memory::ContextPressureLevel::Warning => {
                    tracing::info!(
                        "Context pressure: Warning (~80% used). Consider /compact if responses degrade."
                    );
                }
                crate::memory::ContextPressureLevel::High => {
                    tracing::warn!(
                        "Context pressure: High (~93% used). Auto-compact will trigger."
                    );
                }
                crate::memory::ContextPressureLevel::Critical => {
                    tracing::warn!(
                        "Context pressure: Critical (~97% used). Forcing immediate compact."
                    );
                }
                _ => {}
            }

            if should_force || !consolidation_ran {
                mem.maybe_compact(&*self.llm).await;

                if should_force && consolidation_ran {
                    tracing::warn!(
                        ?pressure,
                        "Forced compact despite consolidation in same turn (critical pressure)"
                    );
                }
            }
        }

        // Inject context pressure level into response extensions for CLI display.
        // The CLI can show a warning in the turn footer when pressure is elevated.
        // Note: `pressure` was captured inside the lock scope above — we re-derive
        // it from the response to avoid lifetime issues with the lock.
        // Simply pass a sentinel value based on whether compact was triggered.
        // (The precise level was already logged above.)

        Ok(response)
    }

    fn name(&self) -> &str {
        "memory"
    }
}

/// Build a compact summary of tool calls for storing in memory.
///
/// Only preserves tool names and arguments (truncated). Tool results are
/// intentionally omitted — they are ephemeral (files may change, searches
/// become stale) and their value is already captured in the assistant's
/// response text. This reduces per-turn tool context from ~700 chars to
/// ~80 chars per call, dramatically improving context utilization.
pub fn summarize_tool_history(history: &[crate::llm::ToolRound]) -> String {
    let mut round_parts = Vec::new();
    for (round_idx, round) in history.iter().enumerate() {
        let calls: Vec<String> = round
            .calls
            .iter()
            .map(|call| {
                format!(
                    "{}({})",
                    call.function_name,
                    truncate_at_char_boundary(&call.arguments.to_string(), 120),
                )
            })
            .collect();
        round_parts.push(format!(
            "[Tool call round {}: {}]",
            round_idx + 1,
            calls.join(", "),
        ));
    }
    round_parts.join("\n")
}
