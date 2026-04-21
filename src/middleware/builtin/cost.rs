//! Cost tracking middleware — cumulative token usage accounting for turns.
//!
//! Extracts the token usage tracking from `cli/repl.rs` into the middleware
//! pipeline so that all execution paths (REPL, print mode, future API) share
//! the same accounting logic.
//!
//! The middleware reads `TurnResponse.usage` after delegation and accumulates
//! it into a shared `SessionCost`. The cost tracker is also injected into
//! `TurnResponse.extensions` so outer layers (e.g., the CLI) can read the
//! current session totals without holding a direct reference.
//!
//! ## `SessionCost`
//!
//! The `SessionCost` struct lives here (not in `cli/`) because it is used by
//! the middleware pipeline and the agent core — both of which sit below the CLI
//! layer. Placing it in `cli/` would create a reverse dependency.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::llm::TokenUsage;

use super::super::{TurnMiddleware, TurnNext, TurnRequest, TurnResponse};

// ════════════════════════════════════════════════════════════
// SessionCost — cumulative token usage tracker
// ════════════════════════════════════════════════════════════

/// Tracks cumulative token usage across a session.
///
/// Used by `CostTurnMiddleware` to accumulate per-turn usage and by the CLI
/// layer to display `/cost` summaries. Lives in the middleware module (not
/// `cli/`) to avoid a reverse dependency from `agent` → `cli`.
pub struct SessionCost {
    prompt_tokens: u64,
    completion_tokens: u64,
    requests: u64,
    /// Token usage from subagent executions (tracked separately).
    subagent_prompt_tokens: u64,
    subagent_completion_tokens: u64,
    subagent_invocations: u64,
}

impl SessionCost {
    pub fn new() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            requests: 0,
            subagent_prompt_tokens: 0,
            subagent_completion_tokens: 0,
            subagent_invocations: 0,
        }
    }

    /// Record token usage from a single request.
    ///
    /// Accepts an optional `&TokenUsage` directly, avoiding the need for
    /// callers to manually extract and unwrap individual token fields.
    pub fn add_usage(&mut self, usage: Option<&TokenUsage>) {
        if let Some(u) = usage {
            self.prompt_tokens += u.prompt_tokens.unwrap_or(0);
            self.completion_tokens += u.completion_tokens.unwrap_or(0);
        }
        self.requests += 1;
    }

    /// Record token usage from a subagent execution.
    pub fn add_subagent_usage(&mut self, usage: Option<&TokenUsage>) {
        if let Some(u) = usage {
            self.subagent_prompt_tokens += u.prompt_tokens.unwrap_or(0);
            self.subagent_completion_tokens += u.completion_tokens.unwrap_or(0);
        }
        self.subagent_invocations += 1;
    }

    /// Reset all counters (e.g. on `/new`).
    pub fn reset(&mut self) {
        self.prompt_tokens = 0;
        self.completion_tokens = 0;
        self.requests = 0;
        self.subagent_prompt_tokens = 0;
        self.subagent_completion_tokens = 0;
        self.subagent_invocations = 0;
    }

    pub fn prompt_tokens(&self) -> u64 {
        self.prompt_tokens
    }

    pub fn completion_tokens(&self) -> u64 {
        self.completion_tokens
    }

    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }

    pub fn subagent_prompt_tokens(&self) -> u64 {
        self.subagent_prompt_tokens
    }

    pub fn subagent_completion_tokens(&self) -> u64 {
        self.subagent_completion_tokens
    }

    pub fn subagent_total_tokens(&self) -> u64 {
        self.subagent_prompt_tokens + self.subagent_completion_tokens
    }

    pub fn subagent_invocations(&self) -> u64 {
        self.subagent_invocations
    }

    /// Grand total tokens (lead agent + all subagents).
    pub fn grand_total_tokens(&self) -> u64 {
        self.total_tokens() + self.subagent_total_tokens()
    }
}

/// Shared handle to a `SessionCost` tracker.
///
/// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) deliberately:
/// the lock is only held for trivial arithmetic (accumulating token counts),
/// so it never blocks long enough to warrant an async-aware mutex. This also
/// allows synchronous access from the CLI `/cost` command without an async runtime.
pub type SharedSessionCost = Arc<Mutex<SessionCost>>;

/// Turn-level cost tracking middleware.
///
/// After the inner pipeline completes, this middleware:
/// 1. Accumulates `TurnResponse.usage` into the shared `SessionCost`.
/// 2. Injects the `SharedSessionCost` into `TurnResponse.extensions`
///    so outer middleware or the caller can access session totals.
///
/// Should be placed **between memory (inner) and logging (outer)** so
/// that usage is recorded before logging emits its structured fields.
pub struct CostTurnMiddleware {
    cost: SharedSessionCost,
}

impl CostTurnMiddleware {
    /// Create a new cost middleware with a shared cost tracker.
    pub fn new(cost: SharedSessionCost) -> Self {
        Self { cost }
    }

    /// Create a new cost middleware and return both the middleware and
    /// the shared handle for external access (e.g., CLI `/cost` command).
    #[allow(dead_code)]
    pub fn new_with_handle() -> (Self, SharedSessionCost) {
        let cost = Arc::new(Mutex::new(SessionCost::new()));
        let mw = Self { cost: Arc::clone(&cost) };
        (mw, cost)
    }
}

#[async_trait]
impl TurnMiddleware for CostTurnMiddleware {
    async fn handle<'a>(
        &self,
        request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        // ── Delegate to inner layers ──
        let mut response = next.run(request).await?;

        // ── After: accumulate token usage ──
        {
            let usage = &response.usage;
            if let Ok(mut cost) = self.cost.lock() {
                cost.add_usage(Some(usage));
            }
        }

        // Inject the shared cost handle into extensions so the caller
        // (or outer middleware) can read session totals.
        response.extensions.insert(Arc::clone(&self.cost));

        tracing::debug!(
            prompt_tokens = response.usage.prompt_tokens,
            completion_tokens = response.usage.completion_tokens,
            "Cost middleware: usage accumulated"
        );

        Ok(response)
    }

    fn name(&self) -> &str {
        "cost"
    }
}
