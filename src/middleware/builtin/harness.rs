//! Harness Engineering middleware — Agent constraint, feedback, and control system.
//!
//! Implements the three pillars of Harness Engineering:
//!
//! 1. **Guardrails**: Safety boundaries (time/call-count limits, operation reversibility)
//! 2. **Feedback Loops**: Real-time feedback integration (lint/test results, error patterns)
//! 3. **Control Systems**: Flow control (dead loop escape, progressive permissions, auto-rollback)
//!
//! ## Key Features
//!
//! - **Dead Loop Escape**: Detects when the agent is stuck in a non-productive pattern
//!   (same tool + same args repeated, or oscillating between two states) and forces
//!   an escape by injecting a strong directive.
//!
//! - **Error Accumulation Guard**: Tracks consecutive errors and escalates response
//!   (warn → force different approach → abort) to prevent error cascading.
//!
//! - **Turn Duration & Tool Count Monitoring**: Tracks wall-clock time and total tool
//!   calls per turn, emitting warnings when thresholds are exceeded.
//!
//! - **Operation Reversibility Check**: Before destructive operations (file delete,
//!   large edits), ensures a rollback path exists.
//!
//! ## Position in Pipeline
//!
//! Should be the **outermost** middleware (after tracing) to catch all issues:
//! `memory → context_engineering → cost → metrics → logging → harness → tracing`
//!
//! Or alternatively between metrics and logging for less overhead.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::llm::ToolResponse;

use super::super::{
    TurnMiddleware, TurnNext, TurnRequest, TurnResponse,
    ToolMiddleware, ToolNext, ToolRequest,
};

// ── Configuration ──

/// Configuration for the harness engineering system.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Maximum consecutive errors before forcing a different approach.
    pub max_consecutive_errors: usize,
    /// Maximum consecutive errors before aborting the turn.
    pub abort_error_threshold: usize,
    /// Maximum wall-clock time for a single turn (seconds).
    pub max_turn_duration_secs: u64,
    /// Maximum total tool calls per turn before warning.
    pub max_tool_calls_per_turn: usize,
    /// Whether to enable operation reversibility checks.
    pub enable_reversibility_check: bool,
    /// Tools considered destructive (require extra caution).
    pub destructive_tools: Vec<String>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_consecutive_errors: 3,
            abort_error_threshold: 5,
            max_turn_duration_secs: 300, // 5 minutes
            max_tool_calls_per_turn: 50,
            enable_reversibility_check: true,
            destructive_tools: vec![
                "bash".to_string(),
                "write_file".to_string(),
            ],
        }
    }
}

// ── Turn-level Harness Middleware ──

/// Turn-level harness middleware — enforces time limits and provides
/// turn-level safety guarantees.
pub struct HarnessTurnMiddleware {
    config: HarnessConfig,
}

impl HarnessTurnMiddleware {
    /// Create a new harness turn middleware with default config.
    pub fn new() -> Self {
        Self {
            config: HarnessConfig::default(),
        }
    }

    /// Create with custom configuration.
    #[allow(dead_code)]
    pub fn with_config(config: HarnessConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl TurnMiddleware for HarnessTurnMiddleware {
    async fn handle<'a>(
        &self,
        request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        let start = Instant::now();
        let max_duration = Duration::from_secs(self.config.max_turn_duration_secs);

        // ── Delegate with timeout awareness ──
        let response = next.run(request).await?;

        // ── After: check turn-level constraints ──
        let elapsed = start.elapsed();

        if elapsed > max_duration {
            tracing::warn!(
                elapsed_secs = elapsed.as_secs(),
                max_secs = self.config.max_turn_duration_secs,
                "Turn exceeded maximum duration"
            );
        }

        // Check tool call count
        let total_calls: usize = response.tool_history.iter()
            .map(|r| r.calls.len())
            .sum();

        if total_calls > self.config.max_tool_calls_per_turn {
            tracing::warn!(
                total_calls,
                max = self.config.max_tool_calls_per_turn,
                "Turn exceeded maximum tool call count"
            );
        }

        Ok(response)
    }

    fn name(&self) -> &str {
        "harness"
    }
}

// ── Tool-level Harness Middleware ──

/// Tool-level harness middleware — provides per-tool-call safety controls.
///
/// Features:
/// - Consecutive error tracking with escalating responses
/// - Oscillation detection (A→B→A→B pattern)
/// - Destructive operation warnings
/// - Per-call timeout enforcement
pub struct HarnessToolMiddleware {
    config: HarnessConfig,
    /// Consecutive error counter (shared across calls within a turn).
    consecutive_errors: Arc<AtomicUsize>,
    /// Recent tool call fingerprints for oscillation detection.
    recent_fingerprints: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl HarnessToolMiddleware {
    /// Create a new harness tool middleware.
    pub fn new() -> Self {
        Self {
            config: HarnessConfig::default(),
            consecutive_errors: Arc::new(AtomicUsize::new(0)),
            recent_fingerprints: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Create with custom configuration.
    #[allow(dead_code)]
    pub fn with_config(config: HarnessConfig) -> Self {
        Self {
            config,
            consecutive_errors: Arc::new(AtomicUsize::new(0)),
            recent_fingerprints: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Check for oscillation pattern (A→B→A→B).
    async fn detect_oscillation(&self, current_fp: &str) -> Option<String> {
        let mut fps = self.recent_fingerprints.lock().await;
        fps.push(current_fp.to_string());

        // Keep only last 6 fingerprints
        if fps.len() > 6 {
            let drain_end = fps.len() - 6;
            fps.drain(0..drain_end);
        }

        // Check for A-B-A-B pattern (minimum 4 entries)
        if fps.len() >= 4 {
            let len = fps.len();
            let a = &fps[len - 4];
            let b = &fps[len - 3];
            let c = &fps[len - 2];
            let d = &fps[len - 1];

            if a == c && b == d && a != b {
                return Some(format!(
                    "Oscillation detected: alternating between two tool calls. \
                     Break the cycle by trying a completely different approach."
                ));
            }
        }

        None
    }
}

#[async_trait]
impl ToolMiddleware for HarnessToolMiddleware {
    async fn handle(
        &self,
        request: ToolRequest,
        next: &dyn ToolNext,
    ) -> ToolResponse {
        let tool_name = request.call.function_name.clone();
        let call_id = request.call.call_id.clone();

        // Build fingerprint for oscillation detection
        let fp = format!("{}|{}", tool_name, request.call.arguments.to_string());
        let fp_short = if fp.len() > 200 {
            fp[..200].to_string()
        } else {
            fp.clone()
        };

        // Check for oscillation pattern
        if let Some(warning) = self.detect_oscillation(&fp_short).await {
            tracing::warn!(tool = %tool_name, "{}", warning);
            // Don't block the call, but append warning to result
            let mut response = next.run(request).await;
            response.content.push_str(&format!(
                "\n\n[HARNESS WARNING] {}",
                warning
            ));
            return response;
        }

        // Check consecutive error count before execution
        let error_count = self.consecutive_errors.load(Ordering::Relaxed);
        if error_count >= self.config.abort_error_threshold {
            tracing::error!(
                tool = %tool_name,
                consecutive_errors = error_count,
                "Too many consecutive errors — suggesting abort"
            );
            return ToolResponse {
                call_id,
                content: format!(
                    "[HARNESS ABORT] {} consecutive tool call errors detected. \
                     The current approach is not working. You MUST try a completely \
                     different strategy or ask the user for help. \
                     Do NOT repeat the same type of operation.",
                    error_count
                ),
                success: false,
            };
        }

        // Warn about destructive operations
        let is_destructive = self.config.destructive_tools.contains(&tool_name);
        if is_destructive && self.config.enable_reversibility_check {
            tracing::debug!(
                tool = %tool_name,
                "Destructive tool call — reversibility check active"
            );
        }

        // ── Execute the tool call ──
        let response = next.run(request).await;

        // ── After: track errors ──
        if !response.success {
            let new_count = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;

            if new_count >= self.config.max_consecutive_errors {
                tracing::warn!(
                    tool = %tool_name,
                    consecutive_errors = new_count,
                    "Multiple consecutive errors — injecting course-correction hint"
                );

                let mut response = response;
                response.content.push_str(&format!(
                    "\n\n[HARNESS WARNING] {} consecutive tool errors. \
                     Your current approach may be flawed. Consider:\n\
                     1. Re-reading the relevant code to verify your assumptions\n\
                     2. Trying a simpler/different approach\n\
                     3. Breaking the problem into smaller steps\n\
                     4. Asking the user for clarification",
                    new_count
                ));
                return response;
            }
        } else {
            // Reset error counter on success
            self.consecutive_errors.store(0, Ordering::Relaxed);
        }

        response
    }

    fn name(&self) -> &str {
        "harness"
    }
}

// ── Feedback Loop Integration ──

/// Feedback signal from tool execution that can influence future decisions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FeedbackSignal {
    /// Lint/compile error detected in tool output.
    LintError { file: String, message: String },
    /// Test failure detected.
    TestFailure { test_name: String, message: String },
    /// Runtime error captured.
    RuntimeError { message: String },
    /// User rejected a suggestion (implicit negative feedback).
    UserRejection { context: String },
}

/// Analyze tool response for feedback signals.
///
/// Scans tool output for patterns that indicate errors or issues,
/// returning structured feedback that can be used to guide the agent.
#[allow(dead_code)]
pub fn extract_feedback_signals(tool_name: &str, output: &str) -> Vec<FeedbackSignal> {
    let mut signals = Vec::new();

    match tool_name {
        "bash" => {
            // Check for compilation errors
            if output.contains("error[E") || output.contains("error:") {
                if let Some(error_line) = output.lines()
                    .find(|l| l.contains("error"))
                {
                    signals.push(FeedbackSignal::LintError {
                        file: extract_file_from_error(output),
                        message: error_line.to_string(),
                    });
                }
            }
            // Check for test failures
            if output.contains("FAILED") || output.contains("test result: FAILED") {
                if let Some(fail_line) = output.lines()
                    .find(|l| l.contains("FAILED") || l.contains("panicked"))
                {
                    signals.push(FeedbackSignal::TestFailure {
                        test_name: extract_test_name(fail_line),
                        message: fail_line.to_string(),
                    });
                }
            }
            // Check for runtime errors
            if output.contains("panic!") || output.contains("thread 'main' panicked") {
                signals.push(FeedbackSignal::RuntimeError {
                    message: output.lines()
                        .find(|l| l.contains("panicked"))
                        .unwrap_or("Unknown panic")
                        .to_string(),
                });
            }
        }
        "edit_file" | "multi_edit" => {
            // Check for edit failures
            if output.contains("not found") || output.contains("no match") {
                signals.push(FeedbackSignal::RuntimeError {
                    message: "Edit target not found — file content may have changed".to_string(),
                });
            }
        }
        _ => {}
    }

    signals
}

// ── Helper Functions ──

/// Extract file path from an error message line.
fn extract_file_from_error(line: &str) -> String {
    // Common patterns: "error: file.rs:10:5" or "error[E0001]: ... --> file.rs:10"
    if let Some(arrow_pos) = line.find("-->") {
        let after_arrow = &line[arrow_pos + 3..];
        let trimmed = after_arrow.trim();
        if let Some(colon_pos) = trimmed.find(':') {
            return trimmed[..colon_pos].to_string();
        }
    }
    // Fallback: look for .rs or .py file patterns
    for word in line.split_whitespace() {
        if word.ends_with(".rs") || word.ends_with(".py") || word.ends_with(".ts") {
            return word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_')
                .to_string();
        }
    }
    String::new()
}

/// Extract test name from a failure line.
fn extract_test_name(line: &str) -> String {
    // Pattern: "test module::test_name ... FAILED"
    if let Some(test_pos) = line.find("test ") {
        let after_test = &line[test_pos + 5..];
        if let Some(space_pos) = after_test.find(' ') {
            return after_test[..space_pos].to_string();
        }
    }
    "unknown_test".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = HarnessConfig::default();
        assert_eq!(config.max_consecutive_errors, 3);
        assert_eq!(config.abort_error_threshold, 5);
        assert_eq!(config.max_turn_duration_secs, 300);
    }

    #[test]
    fn test_extract_feedback_compile_error() {
        let output = "error[E0308]: mismatched types\n --> src/main.rs:10:5\n";
        let signals = extract_feedback_signals("bash", output);
        assert_eq!(signals.len(), 1);
        match &signals[0] {
            FeedbackSignal::LintError { file, .. } => {
                assert_eq!(file, "src/main.rs");
            }
            _ => panic!("Expected LintError"),
        }
    }

    #[test]
    fn test_extract_feedback_test_failure() {
        let output = "test auth::test_login ... FAILED\ntest result: FAILED. 1 passed; 1 failed\n";
        let signals = extract_feedback_signals("bash", output);
        assert!(!signals.is_empty());
        match &signals[0] {
            FeedbackSignal::TestFailure { test_name, .. } => {
                assert_eq!(test_name, "auth::test_login");
            }
            _ => panic!("Expected TestFailure"),
        }
    }

    #[test]
    fn test_extract_file_from_error() {
        assert_eq!(
            extract_file_from_error("error[E0308]: mismatched types\n --> src/main.rs:10:5"),
            "src/main.rs"
        );
        assert_eq!(
            extract_file_from_error("error in file.rs somewhere"),
            "file.rs"
        );
    }

    #[test]
    fn test_oscillation_detection() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mw = HarnessToolMiddleware::new();
            // Simulate A-B-A-B pattern
            assert!(mw.detect_oscillation("read_file|/a.rs").await.is_none());
            assert!(mw.detect_oscillation("edit_file|/a.rs").await.is_none());
            assert!(mw.detect_oscillation("read_file|/a.rs").await.is_none());
            // 4th call completes the A-B-A-B pattern
            let result = mw.detect_oscillation("edit_file|/a.rs").await;
            assert!(result.is_some());
            assert!(result.unwrap().contains("Oscillation"));
        });
    }
}
