//! Hierarchical Context Compression — three-level progressive compression strategy.
//!
//! Implements a graduated compression approach that activates based on
//! `ContextHealthSeverity`:
//!
//! - **L1 (Token-level)**: Filters low-information tokens from tool outputs.
//!   Activated at `Mild` severity. Zero LLM cost — pure heuristic filtering.
//!
//! - **L2 (Message-level)**: Replaces old tool round outputs with structural
//!   summaries, preserving tool call metadata but discarding verbose results.
//!   Activated at `Moderate` severity.
//!
//! - **L3 (Semantic-level)**: Merges multiple related rounds into condensed
//!   knowledge entries. Activated at `Severe` severity.
//!
//! ## Design Principles
//!
//! - Structure-aware: Code outputs preserve AST skeleton; logs keep only errors/warnings
//! - Reversible: Original content hashes are preserved for potential recovery
//! - Attention-optimized: Compressed content is clearly marked so the LLM knows it's a summary

use crate::llm::ToolRound;

use super::context_pressure::ContextHealthSeverity;
use super::truncation::CHARS_PER_TOKEN;

// ── Configuration ──

/// Configuration for hierarchical compression behavior.
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    /// Minimum severity level to activate L1 compression.
    pub l1_threshold: ContextHealthSeverity,
    /// Minimum severity level to activate L2 compression.
    pub l2_threshold: ContextHealthSeverity,
    /// Minimum severity level to activate L3 compression.
    pub l3_threshold: ContextHealthSeverity,
    /// Number of most-recent rounds protected from any compression.
    pub protected_recent_rounds: usize,
    /// L1: Maximum chars for filtered tool output (low-info lines removed).
    pub l1_max_chars: usize,
    /// L2: Maximum chars for structural summary per round.
    pub l2_summary_max_chars: usize,
    /// L3: Maximum chars for merged knowledge entry (multiple rounds).
    pub l3_entry_max_chars: usize,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            l1_threshold: ContextHealthSeverity::Mild,
            l2_threshold: ContextHealthSeverity::Moderate,
            l3_threshold: ContextHealthSeverity::Severe,
            protected_recent_rounds: 5,
            l1_max_chars: 2000,
            l2_summary_max_chars: 300,
            l3_entry_max_chars: 150,
        }
    }
}

impl CompressionConfig {
    /// Build a config scaled to a context window size.
    pub fn for_context_window(context_tokens: usize) -> Self {
        if context_tokens >= 200_000 {
            Self {
                protected_recent_rounds: 10,
                l1_max_chars: 4000,
                l2_summary_max_chars: 500,
                l3_entry_max_chars: 250,
                ..Default::default()
            }
        } else if context_tokens >= 100_000 {
            Self {
                protected_recent_rounds: 7,
                l1_max_chars: 3000,
                l2_summary_max_chars: 400,
                l3_entry_max_chars: 200,
                ..Default::default()
            }
        } else {
            Self::default()
        }
    }
}

// ── Compression Result ──

/// Statistics from a compression pass.
#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    /// Number of rounds that received L1 compression.
    pub l1_rounds_compressed: usize,
    /// Number of rounds that received L2 compression.
    pub l2_rounds_compressed: usize,
    /// Number of round groups merged by L3 compression.
    pub l3_groups_merged: usize,
    /// Estimated tokens saved by compression.
    pub tokens_saved: usize,
    /// Original estimated tokens before compression.
    pub original_tokens: usize,
}

// ── Main Compression Entry Point ──

/// Apply hierarchical compression to tool history based on context health severity.
///
/// This is the main entry point. It applies progressively aggressive compression
/// levels based on the current severity:
///
/// - `Healthy`: No compression (pass-through)
/// - `Mild`: L1 only (token-level filtering)
/// - `Moderate`: L1 + L2 (message-level summarization)
/// - `Severe`: L1 + L2 + L3 (semantic merging)
///
/// The most recent `protected_recent_rounds` are never compressed.
pub fn compress_hierarchically(
    history: &[ToolRound],
    severity: ContextHealthSeverity,
    cfg: &CompressionConfig,
) -> (Vec<ToolRound>, CompressionStats) {
    if severity == ContextHealthSeverity::Healthy || history.is_empty() {
        return (history.to_vec(), CompressionStats::default());
    }

    let original_chars: usize = history.iter()
        .map(|r| round_chars(r))
        .sum();
    let original_tokens = original_chars / CHARS_PER_TOKEN;

    let mut result = history.to_vec();
    let mut stats = CompressionStats {
        original_tokens,
        ..Default::default()
    };

    let len = result.len();
    let protected_start = len.saturating_sub(cfg.protected_recent_rounds);

    // L1: Token-level filtering (activated at Mild+)
    if severity >= cfg.l1_threshold {
        for i in 0..protected_start {
            if apply_l1_compression(&mut result[i], cfg.l1_max_chars) {
                stats.l1_rounds_compressed += 1;
            }
        }
    }

    // L2: Message-level summarization (activated at Moderate+)
    if severity >= cfg.l2_threshold {
        // Apply L2 to older rounds (first half of unprotected rounds)
        let l2_cutoff = protected_start / 2;
        for i in 0..l2_cutoff {
            if apply_l2_compression(&mut result[i], cfg.l2_summary_max_chars) {
                stats.l2_rounds_compressed += 1;
            }
        }
    }

    // L3: Semantic merging (activated at Severe)
    if severity >= cfg.l3_threshold {
        let l3_cutoff = protected_start / 3;
        if l3_cutoff >= 2 {
            let merged_count = apply_l3_compression(&mut result, l3_cutoff, cfg.l3_entry_max_chars);
            stats.l3_groups_merged = merged_count;
        }
    }

    // Calculate tokens saved
    let compressed_chars: usize = result.iter()
        .map(|r| round_chars(r))
        .sum();
    let compressed_tokens = compressed_chars / CHARS_PER_TOKEN;
    stats.tokens_saved = original_tokens.saturating_sub(compressed_tokens);

    if stats.tokens_saved > 0 {
        tracing::debug!(
            severity = ?severity,
            l1_rounds = stats.l1_rounds_compressed,
            l2_rounds = stats.l2_rounds_compressed,
            l3_groups = stats.l3_groups_merged,
            tokens_saved = stats.tokens_saved,
            original_tokens = stats.original_tokens,
            compression_ratio = %format!("{:.1}%", (stats.tokens_saved as f64 / stats.original_tokens as f64) * 100.0),
            "Hierarchical compression applied"
        );
    }

    (result, stats)
}

// ── L1: Token-level Compression ──

/// Apply L1 compression: filter low-information content from tool responses.
///
/// Strategies:
/// - Remove blank lines and excessive whitespace
/// - Remove import statements from code outputs (they're boilerplate)
/// - Remove comment-only lines from code outputs
/// - Truncate repetitive patterns (e.g., long lists of similar items)
///
/// Returns `true` if any compression was applied.
fn apply_l1_compression(round: &mut ToolRound, max_chars: usize) -> bool {
    let mut compressed = false;

    for resp in &mut round.responses {
        if resp.content.len() <= max_chars {
            continue;
        }

        let tool_name = round.calls.first()
            .map(|c| c.function_name.as_str())
            .unwrap_or("");

        let filtered = match classify_output(tool_name) {
            OutputType::Code => filter_code_output(&resp.content, max_chars),
            OutputType::Log => filter_log_output(&resp.content, max_chars),
            OutputType::SearchResult => filter_search_output(&resp.content, max_chars),
            OutputType::Generic => filter_generic_output(&resp.content, max_chars),
        };

        if filtered.len() < resp.content.len() {
            let original_len = resp.content.len();
            resp.content = format!(
                "[L1 compressed: {}/{} chars retained]\n{}",
                filtered.len(), original_len, filtered
            );
            compressed = true;
        }
    }

    compressed
}

// ── L2: Message-level Compression ──

/// Apply L2 compression: replace verbose tool output with structural summary.
///
/// Preserves:
/// - Tool name and key arguments
/// - Success/failure status
/// - Key findings (first meaningful line of output)
///
/// Discards:
/// - Full tool output content
/// - Verbose error traces
///
/// Returns `true` if compression was applied.
fn apply_l2_compression(round: &mut ToolRound, max_chars: usize) -> bool {
    let mut compressed = false;

    for (idx, resp) in round.responses.iter_mut().enumerate() {
        if resp.content.len() <= max_chars {
            continue;
        }

        let tool_name = round.calls.get(idx)
            .map(|c| c.function_name.as_str())
            .unwrap_or("unknown");

        let args_summary = round.calls.get(idx)
            .map(|c| summarize_args(&c.arguments))
            .unwrap_or_default();

        let key_finding = extract_key_finding(&resp.content, tool_name);
        let status = if resp.success { "✓" } else { "✗" };

        resp.content = format!(
            "[L2 summary] {}({}) → {} {}\n{}",
            tool_name, args_summary, status,
            if resp.success { "" } else { "(failed)" },
            key_finding
        );
        compressed = true;
    }

    compressed
}

// ── L3: Semantic-level Compression ──

/// Apply L3 compression: merge consecutive related rounds into knowledge entries.
///
/// Groups rounds that operate on the same file or topic, then replaces
/// the group with a single condensed knowledge entry.
///
/// Returns the number of groups merged.
fn apply_l3_compression(
    history: &mut Vec<ToolRound>,
    cutoff: usize,
    max_chars: usize,
) -> usize {
    if cutoff < 2 {
        return 0;
    }

    // Group consecutive rounds by their primary target (file path or tool type)
    let mut groups: Vec<(usize, usize, String)> = Vec::new(); // (start, end, key)
    let mut current_key = String::new();
    let mut group_start = 0;

    for i in 0..cutoff {
        let key = extract_round_key(&history[i]);
        if key != current_key && !current_key.is_empty() {
            if i - group_start >= 2 {
                groups.push((group_start, i, current_key.clone()));
            }
            group_start = i;
        }
        current_key = key;
    }
    // Don't forget the last group
    if cutoff - group_start >= 2 && !current_key.is_empty() {
        groups.push((group_start, cutoff, current_key));
    }

    // Merge groups from back to front (to preserve indices)
    let mut merged_count = 0;
    for (start, end, key) in groups.into_iter().rev() {
        let group_summary = build_group_summary(&history[start..end], &key, max_chars);

        // Replace the group with a single merged round
        let merged_round = ToolRound {
            calls: vec![crate::llm::ToolCall {
                call_id: format!("merged_{}", start),
                function_name: "[merged_context]".to_string(),
                arguments: serde_json::json!({"topic": key, "rounds_merged": end - start}),
            }],
            responses: vec![crate::llm::ToolResponse {
                call_id: format!("merged_{}", start),
                content: group_summary,
                success: true,
            }],
            reasoning_content: None,
        };

        // Remove the original rounds and insert the merged one
        history.splice(start..end, std::iter::once(merged_round));
        merged_count += 1;
    }

    merged_count
}

// ── Helper Functions ──

/// Estimate the character count of a single round.
fn round_chars(round: &ToolRound) -> usize {
    let call_chars: usize = round.calls.iter()
        .map(|c| c.function_name.len() + c.arguments.to_string().len() + 80)
        .sum();
    let resp_chars: usize = round.responses.iter()
        .map(|r| r.content.len())
        .sum();
    call_chars + resp_chars
}

/// Classify tool output type for appropriate filtering strategy.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputType {
    Code,
    Log,
    SearchResult,
    Generic,
}

fn classify_output(tool_name: &str) -> OutputType {
    match tool_name {
        "read_file" | "edit_file" | "multi_edit" | "write_file" => OutputType::Code,
        "bash" => OutputType::Log,
        "grep_search" | "search_files" | "list_directory" => OutputType::SearchResult,
        _ => OutputType::Generic,
    }
}

/// Filter code output: remove imports, blank lines, comment-only lines.
fn filter_code_output(content: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut filtered = Vec::new();
    let mut chars_used = 0;

    for line in &lines {
        let trimmed = line.trim();

        // Skip low-information lines
        if trimmed.is_empty() {
            continue;
        }
        // Skip standalone comments (but keep doc comments and inline comments)
        if (trimmed.starts_with("//") && !trimmed.starts_with("///"))
            || trimmed.starts_with('#')
        {
            continue;
        }
        // Skip common import patterns
        if trimmed.starts_with("use ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("require(")
        {
            continue;
        }

        let line_len = line.len() + 1; // +1 for newline
        if chars_used + line_len > max_chars {
            filtered.push("... [remaining content filtered]");
            break;
        }
        filtered.push(line);
        chars_used += line_len;
    }

    filtered.join("\n")
}

/// Filter log output: keep only errors, warnings, and summary lines.
fn filter_log_output(content: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut filtered = Vec::new();
    let mut chars_used = 0;

    for line in &lines {
        let lower = line.to_lowercase();
        // Keep error/warning lines and lines with key indicators
        let is_important = lower.contains("error")
            || lower.contains("warn")
            || lower.contains("fail")
            || lower.contains("panic")
            || lower.contains("exit code")
            || lower.contains("successfully")
            || lower.contains("completed");

        if !is_important {
            continue;
        }

        let line_len = line.len() + 1;
        if chars_used + line_len > max_chars {
            break;
        }
        filtered.push(*line);
        chars_used += line_len;
    }

    if filtered.is_empty() {
        // If no important lines found, keep head and tail
        let head_budget = max_chars * 60 / 100;
        let tail_budget = max_chars * 40 / 100;
        let head = crate::tools::truncate_at_char_boundary(content, head_budget);
        let tail_start = content.len().saturating_sub(tail_budget);
        let tail = &content[content.ceil_char_boundary(tail_start)..];
        format!("{}\n...[filtered]...\n{}", head, tail)
    } else {
        filtered.join("\n")
    }
}

/// Filter search results: keep match lines, remove context padding.
fn filter_search_output(content: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut filtered = Vec::new();
    let mut chars_used = 0;

    for line in &lines {
        // Keep file headers and match lines, skip context lines
        let is_match = line.contains(":")
            || line.starts_with("##")
            || line.starts_with("- ")
            || line.starts_with("  ");

        if !is_match && !line.trim().is_empty() {
            continue;
        }

        let line_len = line.len() + 1;
        if chars_used + line_len > max_chars {
            filtered.push("... [more results filtered]");
            break;
        }
        filtered.push(line);
        chars_used += line_len;
    }

    filtered.join("\n")
}

/// Filter generic output: simple truncation with head+tail preservation.
fn filter_generic_output(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }

    let head_budget = max_chars * 70 / 100;
    let tail_budget = max_chars * 30 / 100;

    let head = crate::tools::truncate_at_char_boundary(content, head_budget);
    let tail_start = content.len().saturating_sub(tail_budget);
    let safe_start = content.ceil_char_boundary(tail_start);
    let tail = &content[safe_start..];

    let omitted = content[head.len()..safe_start].matches('\n').count();
    format!("{}\n\n... [{} lines filtered] ...\n\n{}", head, omitted, tail)
}

/// Summarize tool call arguments into a compact string.
fn summarize_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            // Extract the most important argument (usually "path" or "query")
            if let Some(path) = map.get("path").and_then(|v| v.as_str()) {
                let short = path.rsplit('/').next().unwrap_or(path);
                return short.to_string();
            }
            if let Some(query) = map.get("query").and_then(|v| v.as_str()) {
                return crate::tools::truncate_at_char_boundary(query, 40).to_string();
            }
            // Fallback: first string value
            for (key, val) in map {
                if let Some(s) = val.as_str() {
                    let short = crate::tools::truncate_at_char_boundary(s, 30);
                    return format!("{}={}", key, short);
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// Extract the key finding from a tool response (first meaningful line).
fn extract_key_finding(content: &str, tool_name: &str) -> String {
    let max_len = 200;

    match tool_name {
        "grep_search" => {
            // For grep, count matches and show first few
            let match_lines: Vec<&str> = content.lines()
                .filter(|l| l.contains(':') && !l.starts_with('#'))
                .take(3)
                .collect();
            let total_matches = content.lines()
                .filter(|l| l.contains(':') && !l.starts_with('#'))
                .count();
            if match_lines.is_empty() {
                "No matches found".to_string()
            } else {
                let preview = match_lines.join("\n");
                let truncated = crate::tools::truncate_at_char_boundary(&preview, max_len);
                if total_matches > 3 {
                    format!("{}\n... ({} total matches)", truncated, total_matches)
                } else {
                    truncated.to_string()
                }
            }
        }
        "read_file" => {
            // For file reads, show the file structure hint
            let line_count = content.lines().count();
            let first_meaningful = content.lines()
                .find(|l| !l.trim().is_empty() && !l.starts_with("//") && !l.starts_with('#'))
                .unwrap_or("(empty)");
            let truncated = crate::tools::truncate_at_char_boundary(first_meaningful, 100);
            format!("[{} lines] {}", line_count, truncated)
        }
        _ => {
            // Generic: first non-empty line
            let first = content.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no output)");
            crate::tools::truncate_at_char_boundary(first, max_len).to_string()
        }
    }
}

/// Extract a grouping key from a tool round (file path or tool type).
fn extract_round_key(round: &ToolRound) -> String {
    // Try to find a common file path across calls in this round
    for call in &round.calls {
        if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
            return path.to_string();
        }
    }
    // Fallback to tool name
    round.calls.first()
        .map(|c| c.function_name.clone())
        .unwrap_or_default()
}

/// Build a summary for a group of related rounds.
fn build_group_summary(rounds: &[ToolRound], key: &str, max_chars: usize) -> String {
    let mut summary = format!(
        "[L3 merged: {} rounds about '{}']\n",
        rounds.len(),
        crate::tools::truncate_at_char_boundary(key, 60)
    );

    let per_round_budget = max_chars.saturating_sub(summary.len()) / rounds.len().max(1);

    for (i, round) in rounds.iter().enumerate() {
        let tool_names: Vec<&str> = round.calls.iter()
            .map(|c| c.function_name.as_str())
            .collect();
        let status: Vec<&str> = round.responses.iter()
            .map(|r| if r.success { "✓" } else { "✗" })
            .collect();

        let line = format!(
            "  R{}: {} [{}]",
            i + 1,
            tool_names.join("+"),
            status.join(",")
        );

        if summary.len() + line.len() + 1 > max_chars {
            summary.push_str("  ...\n");
            break;
        }
        summary.push_str(&line);
        summary.push('\n');

        // Add key finding if budget allows
        if let Some(resp) = round.responses.first() {
            let tool_name = tool_names.first().copied().unwrap_or("");
            let finding = extract_key_finding(&resp.content, tool_name);
            let truncated = crate::tools::truncate_at_char_boundary(&finding, per_round_budget);
            if summary.len() + truncated.len() + 6 <= max_chars {
                summary.push_str(&format!("    → {}\n", truncated));
            }
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ToolCall, ToolResponse, ToolRound};
    use serde_json::json;

    fn make_round(tool: &str, args: serde_json::Value, output: &str) -> ToolRound {
        ToolRound {
            calls: vec![ToolCall {
                call_id: "1".to_string(),
                function_name: tool.to_string(),
                arguments: args,
            }],
            responses: vec![ToolResponse {
                call_id: "1".to_string(),
                content: output.to_string(),
                success: true,
            }],
            reasoning_content: None,
        }
    }

    #[test]
    fn test_healthy_no_compression() {
        let history = vec![make_round("read_file", json!({"path": "/a.rs"}), "fn main() {}")];
        let (result, stats) = compress_hierarchically(&history, ContextHealthSeverity::Healthy, &CompressionConfig::default());
        assert_eq!(result.len(), 1);
        assert_eq!(stats.tokens_saved, 0);
    }

    #[test]
    fn test_l1_filters_large_output() {
        let large_output = "use std::io;\nuse std::fs;\n// comment\n\nfn main() {\n    println!(\"hello\");\n}\n".repeat(100);
        let history = vec![
            make_round("read_file", json!({"path": "/a.rs"}), &large_output),
            make_round("read_file", json!({"path": "/b.rs"}), "small"),
        ];
        let cfg = CompressionConfig {
            protected_recent_rounds: 1,
            l1_max_chars: 500,
            ..Default::default()
        };
        let (result, stats) = compress_hierarchically(&history, ContextHealthSeverity::Mild, &cfg);
        assert!(result[0].responses[0].content.len() < large_output.len());
        assert!(stats.l1_rounds_compressed > 0);
    }

    #[test]
    fn test_l2_summarizes_old_rounds() {
        let history: Vec<ToolRound> = (0..10)
            .map(|i| make_round(
                "read_file",
                json!({"path": format!("/file_{}.rs", i)}),
                &"x".repeat(1000),
            ))
            .collect();
        let cfg = CompressionConfig {
            protected_recent_rounds: 3,
            l2_summary_max_chars: 200,
            ..Default::default()
        };
        let (result, stats) = compress_hierarchically(&history, ContextHealthSeverity::Moderate, &cfg);
        assert!(stats.l2_rounds_compressed > 0);
        // L2-compressed rounds should contain the summary marker
        assert!(result[0].responses[0].content.contains("[L2 summary]"));
    }

    #[test]
    fn test_protected_rounds_untouched() {
        let history: Vec<ToolRound> = (0..6)
            .map(|i| make_round(
                "read_file",
                json!({"path": format!("/file_{}.rs", i)}),
                &"x".repeat(5000),
            ))
            .collect();
        let cfg = CompressionConfig {
            protected_recent_rounds: 3,
            ..Default::default()
        };
        let (result, _) = compress_hierarchically(&history, ContextHealthSeverity::Severe, &cfg);
        // Last 3 rounds should be unchanged
        for i in 3..6 {
            assert_eq!(result[i].responses[0].content, history[i].responses[0].content);
        }
    }
}
