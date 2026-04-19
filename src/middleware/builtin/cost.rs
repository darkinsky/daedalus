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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::cli::cost::SessionCost;

use super::super::{TurnMiddleware, TurnNext, TurnRequest, TurnResponse};

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
