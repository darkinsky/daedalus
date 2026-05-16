//! Shared utility functions and types for memory strategies.
//!
//! Contains token estimation, text truncation, directive parsing,
//! and the reusable `MessageBuffer` for strategies with sliding windows.

use crate::llm::ChatMessage;

// ── Token estimation ──

/// Approximate characters per token for ASCII-heavy text.
#[allow(dead_code)]
pub(crate) const CHARS_PER_TOKEN: usize = 4;

/// Estimate the number of tokens in a text string, accounting for CJK characters
/// and code/JSON structure.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    estimate_tokens_with_mode(text, TokenEstimationMode::Auto)
}

/// Estimation mode for token counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenEstimationMode {
    /// Auto-detect content type (prose vs code/JSON) and use appropriate ratio.
    Auto,
    /// Force code/JSON ratio (~3 chars/token for ASCII).
    Code,
    /// Force prose ratio (~4 chars/token for ASCII).
    #[allow(dead_code)]
    Prose,
}

/// Estimate tokens with a specific estimation mode.
pub(crate) fn estimate_tokens_with_mode(text: &str, mode: TokenEstimationMode) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_chars: usize = 0;
    let mut other_chars: usize = 0;
    let mut code_indicator_chars: usize = 0;

    for c in text.chars() {
        if is_cjk(c) {
            cjk_chars += 1;
        } else {
            other_chars += 1;
            if is_code_indicator(c) {
                code_indicator_chars += 1;
            }
        }
    }

    let cjk_tokens = (cjk_chars * 2 + 2) / 3;

    let ascii_cpt = match mode {
        TokenEstimationMode::Code => 3,
        TokenEstimationMode::Prose => 4,
        TokenEstimationMode::Auto => {
            if other_chars > 0 && code_indicator_chars * 100 / other_chars > 15 {
                3
            } else {
                4
            }
        }
    };

    let other_tokens = if ascii_cpt > 0 { other_chars / ascii_cpt } else { other_chars };

    cjk_tokens + other_tokens
}

fn is_code_indicator(c: char) -> bool {
    matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ':' | ';' | ',' | '"' | '\'' | '=' | '<' | '>' | '/' | '\\' | '|' | '&' | '!' | '#' | '.' | '_')
}

/// Check whether a character is in a CJK Unicode block.
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   |
        '\u{3400}'..='\u{4DBF}'   |
        '\u{F900}'..='\u{FAFF}'   |
        '\u{3040}'..='\u{309F}'   |
        '\u{30A0}'..='\u{30FF}'   |
        '\u{AC00}'..='\u{D7AF}'   |
        '\u{1100}'..='\u{11FF}'   |
        '\u{3130}'..='\u{318F}'
    )
}

// ── Text utilities ──

/// Truncate rendered text to fit within a token budget, cutting at a line boundary.
pub(crate) fn truncate_to_token_budget(text: String, max_tokens: usize, truncation_suffix: &str) -> String {
    if estimate_tokens(&text) <= max_tokens {
        return text;
    }
    let max_chars = max_tokens * 2;
    let truncated: String = text.chars().take(max_chars).collect();
    let cut_point = truncated.rfind('\n').unwrap_or(truncated.len());
    format!("{}\n\n{}", &truncated[..cut_point], truncation_suffix)
}

/// Strip a directive prefix (e.g., `NEW:`, `ADD:`, `UPDATE:`) case-insensitively.
pub(crate) fn strip_directive_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len()
        && line[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

// ── Message buffer ──

/// Default maximum number of messages to send to the LLM.
pub(crate) const DEFAULT_MAX_MESSAGES: usize = 100;

/// Reusable message buffer for memory strategies that manage their own
/// conversation history with a sliding window.
pub(crate) struct MessageBuffer {
    messages: Vec<ChatMessage>,
    max_messages: usize,
}

impl MessageBuffer {
    pub fn new(max_messages: usize) -> Self {
        Self { messages: Vec::new(), max_messages }
    }

    pub fn add_user(&mut self, content: &str) {
        self.messages.push(ChatMessage::user(content));
    }

    pub fn add_assistant(&mut self, content: &str) {
        self.messages.push(ChatMessage::assistant(content));
    }

    pub fn windowed(&self) -> &[ChatMessage] {
        if self.messages.len() <= self.max_messages {
            &self.messages[..]
        } else {
            &self.messages[self.messages.len() - self.max_messages..]
        }
    }

    #[allow(dead_code)]
    pub fn turn_count(&self) -> usize {
        self.messages.len() / 2
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    pub fn build_messages_with_system(&self, system_prompt: String) -> Vec<ChatMessage> {
        use crate::llm::CacheControl;
        let window = self.windowed();
        let mut messages = Vec::with_capacity(1 + window.len());
        messages.push(
            ChatMessage::system(system_prompt)
                .with_cache_control(CacheControl::Ephemeral)
        );
        messages.extend(window.iter().cloned());
        messages
    }
}
