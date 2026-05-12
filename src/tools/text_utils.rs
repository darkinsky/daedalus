//! Shared UTF-8-safe text utilities.
//!
//! These helpers are used by many layers (CLI rendering, tool history
//! summaries, subagent previews) that all need to truncate strings without
//! splitting a multi-byte character. Keeping one canonical implementation
//! here replaces three near-duplicate copies that previously lived in
//! `cli::render`, `agent::chat`, and `subagent::tool`.

/// Truncate a string to at most `max_chars` Unicode scalar values,
/// appending `"…"` when truncation actually occurred.
///
/// Safe for any UTF-8 input (e.g. Chinese, emoji) — never slices a
/// multi-byte character.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_pos, _)) => format!("{}…", &s[..byte_pos]),
        None => s.to_string(),
    }
}

/// Return a sub-slice of `s` of at most `max_bytes` bytes, ending on a
/// valid UTF-8 character boundary.
///
/// Unlike `truncate_chars`, this version:
/// - operates on **byte** budgets (useful for token-estimate guards)
/// - returns a borrow (no allocation, no "…" suffix)
///
/// Used by `summarize_tool_history` where we want a hard byte cap on
/// each tool call argument entry before joining them.
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    match s.char_indices().take_while(|(i, _)| *i <= max_bytes).last() {
        Some((i, _)) => &s[..i],
        None => &s[..0],
    }
}

/// Truncate a string to at most `max_chars` Unicode scalar values for
/// single-line display, replacing newlines with spaces and appending "..."
/// when truncation occurs.
///
/// Used by the tracing subsystem for YAML/JSON span serialization where
/// multi-line content must be collapsed into a single line.
pub fn truncate_for_display(s: &str, max_chars: usize) -> String {
    let clean: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if clean.chars().count() <= max_chars {
        clean
    } else {
        let truncated: String = clean.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Conditionally truncate based on the `full_content` flag.
///
/// When `full_content` is true, only replaces newlines (for single-line display)
/// but does not truncate. When false, truncates to `max_chars` with "..." suffix.
pub fn maybe_truncate_for_display(s: &str, max_chars: usize, full_content: bool) -> String {
    if full_content {
        s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect()
    } else {
        truncate_for_display(s, max_chars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_ascii() {
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn truncate_chars_multibyte() {
        // Each Chinese char is 3 bytes in UTF-8 but counts as 1 char here.
        assert_eq!(truncate_chars("你好世界", 2), "你好…");
        assert_eq!(truncate_chars("你好", 10), "你好");
    }

    #[test]
    fn truncate_at_boundary_ascii() {
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
        assert_eq!(truncate_at_char_boundary("hi", 10), "hi");
    }

    #[test]
    fn truncate_at_boundary_never_splits_multibyte() {
        let s = "ab你好";
        // Byte 4 is in the middle of '你' (3 bytes starting at index 2).
        // Must fall back to the last valid boundary before index 4 → "ab".
        let out = truncate_at_char_boundary(s, 4);
        assert!(s.starts_with(out));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert_eq!(out, "ab");
    }
}
