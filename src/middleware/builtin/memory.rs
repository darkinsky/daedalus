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

        // ── Delegate to core ──
        let response = next.run(request).await?;

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
            mem.maybe_consolidate(&*self.llm).await;
        }

        Ok(response)
    }

    fn name(&self) -> &str {
        "memory"
    }
}

/// Build a summary of tool calls and results for storing in memory.
pub fn summarize_tool_history(history: &[crate::llm::ToolRound]) -> String {
    let mut parts = Vec::new();
    for (round_idx, round) in history.iter().enumerate() {
        for (i, call) in round.calls.iter().enumerate() {
            let result = round
                .responses
                .get(i)
                .map(|r| r.content.as_str())
                .unwrap_or("(no result)");
            parts.push(format!(
                "[Tool call round {}: {}({}) -> {}]",
                round_idx + 1,
                call.function_name,
                truncate_at_char_boundary(&call.arguments.to_string(), 200),
                truncate_at_char_boundary(result, 500),
            ));
        }
    }
    parts.join("\n")
}
