//! Text normalization for MemPalace.
//!
//! Strips system tags, hook output, and UI chrome from text before
//! storing in drawers. Matches the original MemPalace normalize.py.
//!
//! All patterns are line-anchored. User prose that happens to mention
//! these strings inline is preserved verbatim.

use regex::Regex;
use once_cell::sync::Lazy;

/// Noise tag names to strip from content.
const NOISE_TAGS: &[&str] = &[
    "system-reminder",
    "command-message",
    "command-name",
    "task-notification",
    "user-prompt-submit-hook",
    "hook_output",
];

/// Line prefixes that identify noise lines.
const NOISE_LINE_PREFIXES: &[&str] = &[
    "CURRENT TIME:",
    "VERIFIED FACTS (do not contradict)",
    "AGENT SPECIALIZATION:",
    "Checking verified facts...",
    "Injecting timestamp...",
    "Starting background pipeline...",
    "Checking emotional weights...",
    "Auto-save reminder...",
    "Checking pipeline...",
    "MemPalace auto-save checkpoint.",
];

/// Pre-compiled regex patterns for noise tag stripping.
///
/// Compiled once at first use instead of on every `strip_noise()` call.
/// Each tag generates two patterns: single-line and multi-line.
static NOISE_TAG_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    let mut patterns = Vec::new();
    for tag in NOISE_TAGS {
        // Single-line tags: <tag ...>content</tag>
        let single = format!(
            r"(?m)^(?:> )?<{}(?:\s[^>]*)?>.*?</{}>[ \t]*\n?",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&single) {
            patterns.push(re);
        }
        // Multi-line tags: <tag ...>\n...\n</tag>
        let multi = format!(
            r"(?s)<{}(?:\s[^>]*)?>.*?</{}>[ \t]*\n?",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&multi) {
            patterns.push(re);
        }
    }
    patterns
});

/// Pre-compiled regex patterns for noise line prefix stripping.
static NOISE_PREFIX_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    NOISE_LINE_PREFIXES
        .iter()
        .filter_map(|prefix| {
            let pattern = format!(r"(?m)^(?:> )?{}.*\n?", regex::escape(prefix));
            Regex::new(&pattern).ok()
        })
        .collect()
});

static HOOK_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)^(?:> )?Ran \d+ (?:Stop|PreCompact|PreToolUse|PostToolUse|UserPromptSubmit|Notification|SessionStart|SessionEnd) hooks?.*\n?"
    ).unwrap()
});

static COLLAPSED_LINES_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^(?:> )?…\s*\+\d+ lines.*\n?").unwrap()
});

static TOKEN_EXPAND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*\[\d+\s+tokens?\]\s*\(ctrl\+o to expand\)").unwrap()
});

static MULTI_BLANK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\n{4,}").unwrap()
});

/// Remove system tags, hook output, and UI chrome from text.
///
/// All patterns are line-anchored. User prose that happens to mention
/// these strings inline is preserved verbatim.
pub fn strip_noise(text: &str) -> String {
    let mut result = text.to_string();

    // Strip noise tags (pre-compiled patterns)
    for re in NOISE_TAG_PATTERNS.iter() {
        result = re.replace_all(&result, "").to_string();
    }

    // Strip noise line prefixes (pre-compiled patterns)
    for re in NOISE_PREFIX_PATTERNS.iter() {
        result = re.replace_all(&result, "").to_string();
    }

    // Strip hook lines
    result = HOOK_LINE_RE.replace_all(&result, "").to_string();

    // Strip collapsed output markers
    result = COLLAPSED_LINES_RE.replace_all(&result, "").to_string();

    // Strip token expand chrome
    result = TOKEN_EXPAND_RE.replace_all(&result, "").to_string();

    // Collapse runs of blank lines
    result = MULTI_BLANK_RE.replace_all(&result, "\n\n\n").to_string();

    result.trim().to_string()
}

/// Chunk text into drawer-sized pieces.
///
/// Splits on paragraph boundaries, respecting min/max chunk sizes.
#[allow(dead_code)]
pub fn chunk_text(content: &str, min_size: usize, max_size: usize) -> Vec<ChunkResult> {
    let mut chunks = Vec::new();

    // Try paragraph-based chunking first
    let paragraphs: Vec<&str> = content
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if paragraphs.len() <= 1 && content.lines().count() > 20 {
        // Fallback: chunk by line groups
        let lines: Vec<&str> = content.lines().collect();
        for i in (0..lines.len()).step_by(25) {
            let end = (i + 25).min(lines.len());
            let group = lines[i..end].join("\n");
            if group.len() > min_size {
                chunks.push(ChunkResult {
                    content: group,
                    chunk_index: chunks.len(),
                });
            }
        }
        return chunks;
    }

    let mut current = String::new();
    for para in paragraphs {
        if current.len() + para.len() + 2 > max_size && !current.is_empty() {
            if current.len() >= min_size {
                chunks.push(ChunkResult {
                    content: current.clone(),
                    chunk_index: chunks.len(),
                });
            }
            current.clear();
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }

    if current.len() >= min_size {
        chunks.push(ChunkResult {
            content: current,
            chunk_index: chunks.len(),
        });
    }

    chunks
}

/// A chunk of text with its index.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ChunkResult {
    /// The chunk content.
    pub content: String,
    /// Index of this chunk within the source.
    pub chunk_index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_noise_tags() {
        let input = "Hello\n<system-reminder>some noise</system-reminder>\nWorld";
        let result = strip_noise(input);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
        assert!(!result.contains("system-reminder"));
    }

    #[test]
    fn test_strip_noise_prefixes() {
        let input = "Hello\nCURRENT TIME: 2026-04-16\nWorld";
        let result = strip_noise(input);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
        assert!(!result.contains("CURRENT TIME"));
    }

    #[test]
    fn test_chunk_text() {
        let content = "Para 1 content here.\n\nPara 2 content here.\n\nPara 3 content here.";
        let chunks = chunk_text(content, 10, 50);
        assert!(!chunks.is_empty());
    }
}
