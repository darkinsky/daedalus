//! Consecutive duplicate tool-call detector.
//!
//! Guards both the Lead agent's tool-calling loop (`src/agent/chat.rs`) and
//! the subagent runner's loop (`src/subagent/runner.rs`) against the common
//! "LLM gets stuck calling the same tool with the same arguments over and
//! over" failure mode.
//!
//! Behavior:
//! - A tool call's *fingerprint* is `"<tool_name>|<canonical_json(arguments)>"`.
//! - The detector counts how many **consecutive rounds** each fingerprint
//!   has appeared in. A fingerprint that does not appear in the next round
//!   has its counter reset.
//! - When a fingerprint's consecutive count reaches `WARN_THRESHOLD` (3),
//!   a warning is emitted — both as a `tracing::warn!` log and as extra
//!   text appended to that tool's response so the LLM can see it and change
//!   course.
//! - When a fingerprint's consecutive count reaches `STOP_THRESHOLD` (5),
//!   the loop is told to stop.

use std::collections::HashMap;

use crate::llm::{ToolCall, ToolResponse};

/// Warn the LLM starting from this many consecutive identical tool calls.
pub const WARN_THRESHOLD: usize = 3;

/// Forcefully stop the tool-calling loop at this many consecutive identical calls.
pub const STOP_THRESHOLD: usize = 5;

/// What the caller should do after recording a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateAction {
    /// No issue — carry on normally.
    Ok,
    /// At least one fingerprint has hit the warning threshold.
    /// The caller should surface the messages back to the LLM.
    Warn(Vec<DuplicateWarning>),
    /// At least one fingerprint has hit the stop threshold.
    /// The caller must terminate the tool-calling loop.
    Stop(DuplicateWarning),
}

/// Details about a fingerprint that tripped the warning threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateWarning {
    /// The fingerprint that repeated (`"<tool>|<args>"`).
    pub fingerprint: String,
    /// The tool name extracted from the fingerprint.
    pub tool_name: String,
    /// How many consecutive rounds this fingerprint has appeared in.
    pub count: usize,
}

impl DuplicateWarning {
    /// Human-readable warning message suitable for feeding back to the LLM.
    pub fn warn_message(&self) -> String {
        format!(
            "\n\n[duplicate-call guard] This exact call to `{}` with these arguments \
             has been made {} times in a row. Please re-examine the prior results and \
             try a different approach (different tool, different arguments, or answer \
             directly). If you repeat it again, the loop will be force-stopped.",
            self.tool_name, self.count
        )
    }

    /// Human-readable stop message (used in the error returned when the
    /// loop is aborted).
    pub fn stop_message(&self) -> String {
        format!(
            "Aborting tool-calling loop: `{}` has been called {} times in a row with \
             identical arguments. Likely a non-productive loop.",
            self.tool_name, self.count
        )
    }
}

/// Tracks consecutive-round counts per fingerprint.
#[derive(Debug, Default)]
pub struct DuplicateDetector {
    /// fingerprint → consecutive-rounds count.
    counts: HashMap<String, usize>,
}

impl DuplicateDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a round of tool calls and return what the caller should do.
    ///
    /// After recording, fingerprints that did **not** appear in `tool_calls`
    /// are removed (their streak is broken), and those that did appear have
    /// their count incremented.
    pub fn record_round(&mut self, tool_calls: &[ToolCall]) -> DuplicateAction {
        // Build fingerprints for this round (dedup within the round — a single
        // round calling the same fingerprint twice still counts as "appeared
        // once" for streak purposes).
        let mut this_round: HashMap<String, String> = HashMap::new();
        for call in tool_calls {
            let fp = fingerprint(call);
            this_round.entry(fp).or_insert_with(|| call.function_name.clone());
        }

        // Increment counts for fingerprints that appeared; drop the rest.
        let mut new_counts: HashMap<String, usize> = HashMap::new();
        for (fp, _tool) in &this_round {
            let prev = self.counts.get(fp).copied().unwrap_or(0);
            new_counts.insert(fp.clone(), prev + 1);
        }
        self.counts = new_counts;

        // Scan for stop (highest count takes precedence) and warn fingerprints.
        let mut stop_candidate: Option<DuplicateWarning> = None;
        let mut warns: Vec<DuplicateWarning> = Vec::new();

        for (fp, count) in &self.counts {
            let tool_name = this_round.get(fp).cloned().unwrap_or_default();
            let warning = DuplicateWarning {
                fingerprint: fp.clone(),
                tool_name,
                count: *count,
            };
            if *count >= STOP_THRESHOLD {
                if stop_candidate.as_ref().map(|w| w.count).unwrap_or(0) < *count {
                    stop_candidate = Some(warning);
                }
            } else if *count >= WARN_THRESHOLD {
                warns.push(warning);
            }
        }

        if let Some(stop) = stop_candidate {
            return DuplicateAction::Stop(stop);
        }
        if !warns.is_empty() {
            return DuplicateAction::Warn(warns);
        }
        DuplicateAction::Ok
    }
}

/// Compute the fingerprint for a single tool call.
///
/// Uses a canonicalized JSON representation of the arguments so that key
/// ordering differences don't prevent detection of otherwise-identical
/// calls.
pub fn fingerprint(call: &ToolCall) -> String {
    format!("{}|{}", call.function_name, canonicalize(&call.arguments))
}

/// Produce a deterministic string from a JSON value.
///
/// Objects have their keys sorted; arrays preserve order; scalars are
/// stringified via `to_string`.
fn canonicalize(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}={}", k, canonicalize(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonicalize).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Append a duplicate-call warning to the matching tool responses so the
/// LLM can actually see it in the next turn.
///
/// Only responses whose originating call matches one of the warned
/// fingerprints receive the appended message.
pub fn annotate_responses(
    tool_calls: &[ToolCall],
    responses: &mut [ToolResponse],
    warnings: &[DuplicateWarning],
) {
    if warnings.is_empty() {
        return;
    }
    for (call, response) in tool_calls.iter().zip(responses.iter_mut()) {
        let fp = fingerprint(call);
        if let Some(w) = warnings.iter().find(|w| w.fingerprint == fp) {
            response.content.push_str(&w.warn_message());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tc(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "cid".to_string(),
            function_name: name.to_string(),
            arguments: args,
        }
    }

    #[test]
    fn fingerprint_ignores_key_order() {
        let a = tc("read_file", json!({"path": "/a", "offset": 1}));
        let b = tc("read_file", json!({"offset": 1, "path": "/a"}));
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn streak_triggers_warn_then_stop() {
        let mut d = DuplicateDetector::new();
        let call = tc("grep_search", json!({"q": "foo"}));

        // Rounds 1..=2 — nothing yet.
        assert_eq!(d.record_round(&[call.clone()]), DuplicateAction::Ok);
        assert_eq!(d.record_round(&[call.clone()]), DuplicateAction::Ok);

        // Round 3 — warn.
        match d.record_round(&[call.clone()]) {
            DuplicateAction::Warn(ws) => {
                assert_eq!(ws.len(), 1);
                assert_eq!(ws[0].count, 3);
                assert_eq!(ws[0].tool_name, "grep_search");
            }
            other => panic!("expected Warn, got {:?}", other),
        }

        // Round 4 — still warn.
        match d.record_round(&[call.clone()]) {
            DuplicateAction::Warn(ws) => assert_eq!(ws[0].count, 4),
            other => panic!("expected Warn, got {:?}", other),
        }

        // Round 5 — stop.
        match d.record_round(&[call.clone()]) {
            DuplicateAction::Stop(w) => assert_eq!(w.count, 5),
            other => panic!("expected Stop, got {:?}", other),
        }
    }

    #[test]
    fn different_call_resets_streak() {
        let mut d = DuplicateDetector::new();
        let a = tc("read_file", json!({"path": "/a"}));
        let b = tc("read_file", json!({"path": "/b"}));

        d.record_round(&[a.clone()]);
        d.record_round(&[a.clone()]);
        // Switch to a different fingerprint — `a`'s streak is broken.
        d.record_round(&[b.clone()]);
        // Back to `a` — should start counting from 1 again.
        assert_eq!(d.record_round(&[a.clone()]), DuplicateAction::Ok);
    }

    #[test]
    fn annotate_attaches_warning_only_to_matching_response() {
        let calls = vec![
            tc("read_file", json!({"path": "/a"})),
            tc("list_directory", json!({"path": "/"})),
        ];
        let mut responses = vec![
            ToolResponse::new("cid1", "content-a"),
            ToolResponse::new("cid2", "content-b"),
        ];
        let warnings = vec![DuplicateWarning {
            fingerprint: fingerprint(&calls[0]),
            tool_name: "read_file".to_string(),
            count: 3,
        }];
        annotate_responses(&calls, &mut responses, &warnings);
        assert!(responses[0].content.contains("duplicate-call guard"));
        assert!(!responses[1].content.contains("duplicate-call guard"));
    }
}
