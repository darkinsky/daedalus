//! Context Engineering middleware — intelligent context assembly and budget allocation.
//!
//! Implements the "Context Engineering Pipeline" concept: context is not passively
//! filled but actively engineered. This middleware:
//!
//! 1. **Budget Allocation**: Dynamically allocates token budget across context sections
//!    (system prompt, conversation history, tool results, user input) based on task type.
//!
//! 2. **Priority Structuring**: Places high-priority information at the head and tail
//!    of the context (exploiting LLM's U-shaped attention curve).
//!
//! 3. **Redundancy Detection**: Identifies and removes duplicate information across
//!    messages (e.g., file content read multiple times).
//!
//! ## Position in Pipeline
//!
//! Should be placed between `memory` (innermost) and `cost` middleware:
//! `memory → context_engineering → cost → metrics → logging → tracing`
//!
//! This ensures it processes messages after memory builds them but before
//! cost tracking and outer observability layers.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::llm::ChatMessage;

use super::super::{TurnMiddleware, TurnNext, TurnRequest, TurnResponse};

// ── Configuration ──

/// Budget allocation ratios for different context sections.
///
/// These ratios determine how the total context window is divided.
/// The sum should be <= 1.0 (remaining space is for model output).
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Fraction of context window for system prompt (identity, rules, tools).
    pub system_ratio: f64,
    /// Fraction for conversation history (user/assistant messages).
    pub history_ratio: f64,
    /// Fraction for tool results (within the current turn).
    pub tool_results_ratio: f64,
    /// Fraction reserved for model output.
    pub output_reserve_ratio: f64,
    /// Total context window size in tokens.
    pub context_window_tokens: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            system_ratio: 0.15,
            history_ratio: 0.35,
            tool_results_ratio: 0.30,
            output_reserve_ratio: 0.20,
            context_window_tokens: 128_000,
        }
    }
}

impl ContextBudget {
    /// Create a budget for a given context window size.
    pub fn for_context_window(tokens: usize) -> Self {
        Self {
            context_window_tokens: tokens,
            ..Default::default()
        }
    }

    /// Get the token budget for a specific section.
    pub fn section_budget(&self, section: ContextSection) -> usize {
        let ratio = match section {
            ContextSection::System => self.system_ratio,
            ContextSection::History => self.history_ratio,
            ContextSection::ToolResults => self.tool_results_ratio,
            ContextSection::OutputReserve => self.output_reserve_ratio,
        };
        (self.context_window_tokens as f64 * ratio) as usize
    }
}

/// Context sections for budget allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ContextSection {
    System,
    History,
    ToolResults,
    OutputReserve,
}

// ── Priority Structuring ──

/// Priority levels for messages in the context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub enum MessagePriority {
    /// Critical: Must always be present (system identity, current task).
    Critical = 4,
    /// High: Important for current task (recent tool results, user constraints).
    High = 3,
    /// Medium: Useful context (older conversation, resolved issues).
    Medium = 2,
    /// Low: Nice-to-have (old tool outputs, superseded information).
    Low = 1,
}

// ── Redundancy Detection ──

/// Detect and mark redundant content across messages.
///
/// Identifies cases where the same file content or information appears
/// multiple times in the context (e.g., file read twice, same error repeated).
pub fn detect_redundancy(messages: &[ChatMessage]) -> Vec<RedundancyHint> {
    let mut hints = Vec::new();
    let mut seen_content: HashMap<u64, usize> = HashMap::new(); // hash → first occurrence index

    for (idx, msg) in messages.iter().enumerate() {
        if msg.content.len() < 100 {
            continue; // Skip short messages — unlikely to be redundant
        }

        // Use a simple hash of the first 200 chars as fingerprint
        let fingerprint = simple_hash(&msg.content[..msg.content.len().min(200)]);

        if let Some(&first_idx) = seen_content.get(&fingerprint) {
            hints.push(RedundancyHint {
                message_index: idx,
                first_occurrence: first_idx,
                estimated_waste_chars: msg.content.len(),
            });
        } else {
            seen_content.insert(fingerprint, idx);
        }
    }

    hints
}

/// A hint about redundant content in the message list.
#[derive(Debug, Clone)]
pub struct RedundancyHint {
    /// Index of the redundant message.
    pub message_index: usize,
    /// Index of the first occurrence of similar content.
    pub first_occurrence: usize,
    /// Estimated characters wasted by the redundancy.
    pub estimated_waste_chars: usize,
}

// ── Context Engineering Middleware ──

/// Turn-level context engineering middleware.
///
/// Optimizes the message list for maximum information density and
/// attention efficiency before passing to the LLM.
pub struct ContextEngineeringMiddleware {
    /// Budget configuration.
    budget: ContextBudget,
    /// Whether to apply priority restructuring.
    enable_priority_structuring: bool,
    /// Whether to detect and remove redundancy.
    enable_redundancy_removal: bool,
}

impl ContextEngineeringMiddleware {
    /// Create a new context engineering middleware with default settings.
    pub fn new(context_window_tokens: usize) -> Self {
        Self {
            budget: ContextBudget::for_context_window(context_window_tokens),
            enable_priority_structuring: true,
            enable_redundancy_removal: true,
        }
    }

    /// Apply priority structuring to messages.
    ///
    /// Ensures high-priority information is at the beginning and end of the
    /// message list (U-shaped attention optimization).
    fn apply_priority_structuring(&self, messages: &mut Vec<ChatMessage>) {
        if messages.len() < 4 {
            return; // Too few messages to restructure
        }

        // Identify the system message (always first — Critical priority)
        // Identify the last user message (always last — Critical priority)
        // Middle messages: sort by estimated priority (keep chronological within same priority)

        // For now, we ensure the most recent user instruction and system prompt
        // are at the boundaries, and inject a "context anchor" reminder if the
        // conversation is long.
        if messages.len() > 20 {
            // Inject a mid-context anchor to remind the model of the current task
            if let Some(last_user) = messages.iter().rev()
                .find(|m| m.role == crate::llm::ChatRole::User)
            {
                let task_reminder = extract_task_essence(&last_user.content);
                if !task_reminder.is_empty() && messages.len() > 10 {
                    // Find the midpoint and inject a subtle reminder
                    let mid = messages.len() / 2;
                    let anchor = ChatMessage::system(format!(
                        "[Context anchor — current task: {}]",
                        task_reminder
                    ));
                    messages.insert(mid, anchor);
                }
            }
        }
    }

    /// Remove redundant content from messages.
    fn remove_redundancy(&self, messages: &mut Vec<ChatMessage>) {
        let hints = detect_redundancy(messages);

        if hints.is_empty() {
            return;
        }

        let total_waste: usize = hints.iter().map(|h| h.estimated_waste_chars).sum();
        let estimated_tokens_saved = total_waste / 4; // rough estimate

        tracing::debug!(
            redundant_messages = hints.len(),
            estimated_tokens_saved,
            "Detected redundant content in context"
        );

        // Replace redundant messages with back-references (from end to preserve indices)
        for hint in hints.into_iter().rev() {
            if hint.message_index < messages.len() {
                let original_len = messages[hint.message_index].content.len();
                messages[hint.message_index].content = format!(
                    "[Duplicate content — see message #{} above. Original: {} chars]",
                    hint.first_occurrence + 1,
                    original_len,
                );
            }
        }
    }
}

#[async_trait]
impl TurnMiddleware for ContextEngineeringMiddleware {
    async fn handle<'a>(
        &self,
        mut request: TurnRequest<'a>,
        next: &dyn TurnNext,
    ) -> anyhow::Result<TurnResponse> {
        // ── Before: optimize context assembly ──

        if !request.messages.is_empty() {
            // 1. Remove redundant content
            if self.enable_redundancy_removal {
                self.remove_redundancy(&mut request.messages);
            }

            // 2. Apply priority structuring (U-shaped attention optimization)
            if self.enable_priority_structuring {
                self.apply_priority_structuring(&mut request.messages);
            }

            // 3. Budget enforcement: if messages exceed budget, trim middle messages
            let estimated_tokens = estimate_messages_tokens(&request.messages);
            let history_budget = self.budget.section_budget(ContextSection::History);

            if estimated_tokens > history_budget {
                tracing::info!(
                    estimated_tokens,
                    budget = history_budget,
                    "Context exceeds history budget — trimming middle messages"
                );
                trim_to_budget(&mut request.messages, history_budget);
            }
        }

        // ── Delegate to next layer ──
        next.run(request).await
    }

    fn name(&self) -> &str {
        "context_engineering"
    }
}

// ── Helper Functions ──

/// Estimate total tokens for a message list.
fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter()
        .map(|m| {
            let content_len = m.content.len();
            // CJK-aware estimation: check for CJK characters
            let cjk_ratio = estimate_cjk_ratio(&m.content);
            let chars_per_token = if cjk_ratio > 0.3 { 2.0 } else { 4.0 };
            (content_len as f64 / chars_per_token) as usize + 4 // +4 for message overhead
        })
        .sum()
}

/// Estimate the ratio of CJK characters in a string.
fn estimate_cjk_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let sample_len = text.len().min(500);
    let sample = &text[..text.floor_char_boundary(sample_len)];
    let cjk_count = sample.chars()
        .filter(|c| is_cjk_char(*c))
        .count();
    let total_chars = sample.chars().count();
    if total_chars == 0 { 0.0 } else { cjk_count as f64 / total_chars as f64 }
}

/// Check if a character is CJK.
fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |   // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}' |   // CJK Extension A
        '\u{F900}'..='\u{FAFF}' |   // CJK Compatibility Ideographs
        '\u{3000}'..='\u{303F}' |   // CJK Symbols and Punctuation
        '\u{FF00}'..='\u{FFEF}'     // Halfwidth and Fullwidth Forms
    )
}

/// Trim messages to fit within a token budget.
///
/// Strategy: Keep first message (system) and last N messages (recent context),
/// progressively remove middle messages until within budget.
fn trim_to_budget(messages: &mut Vec<ChatMessage>, budget_tokens: usize) {
    const PRESERVE_TAIL: usize = 6;

    if messages.len() <= PRESERVE_TAIL + 1 {
        return; // Nothing to trim
    }

    let preserve_start = 1; // Keep system message
    let preserve_end = messages.len().saturating_sub(PRESERVE_TAIL);

    // Remove middle messages one by one until within budget
    let mut removable_range = preserve_start..preserve_end;
    while estimate_messages_tokens(messages) > budget_tokens && removable_range.start < removable_range.end {
        // Remove the oldest non-system message
        if removable_range.start < messages.len() {
            messages.remove(removable_range.start);
            removable_range.end -= 1;
        } else {
            break;
        }
    }
}

/// Extract the essence of a task from a user message (first sentence or line).
fn extract_task_essence(content: &str) -> String {
    let first_line = content.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");

    // Truncate to reasonable length
    if first_line.len() > 100 {
        format!("{}...", &first_line[..first_line.floor_char_boundary(100)])
    } else {
        first_line.to_string()
    }
}

/// Simple non-cryptographic hash for content fingerprinting.
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRole};

    #[test]
    fn test_budget_allocation() {
        let budget = ContextBudget::for_context_window(128_000);
        assert_eq!(budget.section_budget(ContextSection::System), 19_200); // 15%
        assert_eq!(budget.section_budget(ContextSection::History), 44_800); // 35%
        assert_eq!(budget.section_budget(ContextSection::ToolResults), 38_400); // 30%
    }

    #[test]
    fn test_redundancy_detection() {
        let messages = vec![
            ChatMessage::user("Hello, I need help with my code"),
            ChatMessage::assistant(&"x".repeat(200)),
            ChatMessage::user("Can you check again?"),
            ChatMessage::assistant(&"x".repeat(200)), // duplicate of messages[1]
        ];
        let hints = detect_redundancy(&messages);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].message_index, 3);
        assert_eq!(hints[0].first_occurrence, 1);
    }

    #[test]
    fn test_cjk_ratio_estimation() {
        assert!(estimate_cjk_ratio("Hello world") < 0.1);
        assert!(estimate_cjk_ratio("你好世界") > 0.5);
        assert!(estimate_cjk_ratio("Hello 你好") > 0.2);
    }

    #[test]
    fn test_extract_task_essence() {
        assert_eq!(
            extract_task_essence("Fix the login bug in auth.rs\nMore details here"),
            "Fix the login bug in auth.rs"
        );
        assert_eq!(extract_task_essence(""), "");
    }
}
