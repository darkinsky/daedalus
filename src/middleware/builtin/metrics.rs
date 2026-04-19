//! Metrics middleware — timing and round counting for turns.
//!
//! Records wall-clock elapsed time for each turn and injects a `TurnMetrics`
//! struct into `TurnResponse.extensions`. This decouples timing from the CLI
//! layer (`repl.rs`) so that all execution paths (REPL, print mode, future
//! API) get consistent metrics.
//!
//! ## What it measures
//!
//! - **elapsed**: Total wall-clock time from request entry to response exit.
//! - **tool_rounds**: Number of tool-calling rounds (from `tool_history`).
//! - **tool_calls**: Total number of individual tool calls across all rounds.

use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::super::{TurnMiddleware, TurnNext, TurnRequest, TurnResponse};

/// Metrics collected for a single turn.
///
/// Injected into `TurnResponse.extensions` by `MetricsTurnMiddleware`.
/// The CLI or any outer layer can read this via `response.extensions.get::<TurnMetrics>()`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TurnMetrics {
    /// Total wall-clock time for the turn (including all LLM calls and tool executions).
    pub elapsed: Duration,
    /// Number of tool-calling rounds in this turn (0 if no tools were used).
    pub tool_rounds: usize,
    /// Total number of individual tool calls across all rounds.
    pub tool_calls: usize,
}

impl TurnMetrics {
    /// Elapsed time in seconds (convenience for display).
    #[allow(dead_code)]
    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed.as_secs_f64()
    }
}

/// Turn-level metrics middleware.
///
/// Wraps the entire turn in a timer and counts tool rounds/calls from
/// the response. The resulting `TurnMetrics` is injected into
/// `TurnResponse.extensions`.
///
/// Should be placed **outside cost and memory** but **inside logging/tracing**
/// so that the timing includes the full pipeline minus observability overhead.
pub struct MetricsTurnMiddleware;

impl MetricsTurnMiddleware {
    /// Create a new metrics middleware.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TurnMiddleware for MetricsTurnMiddleware {
    async fn handle<'a>(
        &self,
        request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        let start = Instant::now();

        // ── Delegate to inner layers ──
        let mut response = next.run(request).await?;

        // ── After: compute metrics ──
        let elapsed = start.elapsed();
        let tool_rounds = response.tool_history.len();
        let tool_calls: usize = response.tool_history.iter()
            .map(|r| r.calls.len())
            .sum();

        let metrics = TurnMetrics {
            elapsed,
            tool_rounds,
            tool_calls,
        };

        tracing::debug!(
            elapsed_ms = elapsed.as_millis() as u64,
            tool_rounds = tool_rounds,
            tool_calls = tool_calls,
            "Turn metrics recorded"
        );

        // Inject metrics into extensions for outer layers to read
        response.extensions.insert(metrics);

        Ok(response)
    }

    fn name(&self) -> &str {
        "metrics"
    }
}
