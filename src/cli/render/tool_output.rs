//! Bash command unfolding + tool output truncation helpers.
//!
//! These helpers are dedicated to rendering tool-related output in the
//! terminal. Pulled into `cli::render` via `#[path]` so the logic stays
//! testable and small without introducing a full nested module tree
//! (which would otherwise conflict with the single-file `render.rs`).

/// Maximum number of lines to display for tool output before truncating.
pub(super) const TOOL_OUTPUT_MAX_LINES: usize = 10;
/// Number of lines to show from the beginning when truncating.
pub(super) const TOOL_OUTPUT_HEAD_LINES: usize = 5;
/// Number of lines to show from the end when truncating.
pub(super) const TOOL_OUTPUT_TAIL_LINES: usize = 3;

/// Unfold a bash command: preserve the full text (no truncation), but insert a
/// newline + 4-space indent after each top-level logical separator so humans
/// can read and copy long pipelines.
///
/// Handled separators (in priority order): `&&`, `||`, `;`, and single `|`.
/// Separators inside single / double quotes or escaped are left untouched so
/// we don't break strings like `echo "a && b"`.
pub(super) fn unfold_bash_command(cmd: &str) -> String {
    // Collapse CR/LF first so embedded newlines in the source command also get
    // re-indented consistently.
    let normalized = cmd.replace("\r\n", "\n").replace('\r', "\n");

    let mut out = String::with_capacity(normalized.len() + 16);
    let bytes = normalized.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        let c = bytes[i];

        // Non-ASCII byte (UTF-8 continuation / leading byte). Copy the whole
        // char via `char_indices` to stay UTF-8 safe, then advance.
        if c >= 0x80 {
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
                end += 1;
            }
            out.push_str(&normalized[i..end]);
            i = end;
            continue;
        }

        // Handle escape: copy next byte verbatim.
        if c == b'\\' && !in_single && i + 1 < bytes.len() {
            out.push(c as char);
            let mut end = i + 2;
            while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
                end += 1;
            }
            out.push_str(&normalized[i + 1..end]);
            i = end;
            continue;
        }

        if c == b'\'' && !in_double {
            in_single = !in_single;
            out.push('\'');
            i += 1;
            continue;
        }
        if c == b'"' && !in_single {
            in_double = !in_double;
            out.push('"');
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            // Try two-byte separators first.
            if i + 1 < bytes.len() {
                let pair = &bytes[i..i + 2];
                if pair == b"&&" || pair == b"||" {
                    out.push(bytes[i] as char);
                    out.push(bytes[i + 1] as char);
                    out.push('\n');
                    out.push_str("    ");
                    i += 2;
                    while i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                    continue;
                }
            }
            // Single-byte separators.
            if c == b';' {
                out.push(';');
                out.push('\n');
                out.push_str("    ");
                i += 1;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                continue;
            }
            if c == b'|' {
                out.push('|');
                out.push('\n');
                out.push_str("    ");
                i += 1;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                continue;
            }
            if c == b'\n' {
                out.push('\n');
                out.push_str("    ");
                i += 1;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                continue;
            }
        }

        out.push(c as char);
        i += 1;
    }

    out
}

/// Format tool output lines with smart truncation (head + tail).
///
/// Returns a `Vec<String>` of formatted lines, ready to be printed.
/// Mimics Claude Code's approach: if the output exceeds `TOOL_OUTPUT_MAX_LINES`,
/// show the first `TOOL_OUTPUT_HEAD_LINES` lines, a "... (N lines hidden) ..."
/// indicator, and the last `TOOL_OUTPUT_TAIL_LINES` lines.
pub(super) fn format_truncated_output(lines: &[&str]) -> Vec<String> {
    let line_count = lines.len();
    let mut result = Vec::new();

    if line_count == 0 {
        return result;
    }

    if line_count <= TOOL_OUTPUT_MAX_LINES {
        for line in lines {
            result.push(line.to_string());
        }
    } else {
        for line in &lines[..TOOL_OUTPUT_HEAD_LINES] {
            result.push(line.to_string());
        }
        let hidden = line_count - TOOL_OUTPUT_HEAD_LINES - TOOL_OUTPUT_TAIL_LINES;
        result.push(format!("... ({} lines hidden) ...", hidden));
        for line in &lines[line_count - TOOL_OUTPUT_TAIL_LINES..] {
            result.push(line.to_string());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfold_splits_on_logical_separators() {
        let out = unfold_bash_command("cat a.txt && cat b.txt | sort -u ; echo done");
        let expected = "cat a.txt &&\n    cat b.txt |\n    sort -u ;\n    echo done";
        assert_eq!(out, expected);
    }

    #[test]
    fn unfold_preserves_quoted_separators() {
        let out = unfold_bash_command(r#"echo "a && b" | grep 'x | y'"#);
        let expected = "echo \"a && b\" |\n    grep 'x | y'";
        assert_eq!(out, expected);
    }

    #[test]
    fn unfold_is_utf8_safe() {
        let cmd = "ls /数据/中文 && echo 完成";
        let out = unfold_bash_command(cmd);
        assert_eq!(out, "ls /数据/中文 &&\n    echo 完成");
    }

    #[test]
    fn unfold_ignores_standalone_pipe_inside_double_pipe() {
        let out = unfold_bash_command("foo || bar");
        assert_eq!(out, "foo ||\n    bar");
    }

    #[test]
    fn truncate_output_short_passthrough() {
        let lines: Vec<&str> = vec!["a", "b", "c"];
        let formatted = format_truncated_output(&lines);
        assert_eq!(formatted, vec!["a", "b", "c"]);
    }

    #[test]
    fn truncate_output_long_head_tail() {
        let lines: Vec<String> = (0..20).map(|i| format!("L{}", i)).collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let formatted = format_truncated_output(&refs);
        assert_eq!(formatted.len(), TOOL_OUTPUT_HEAD_LINES + 1 + TOOL_OUTPUT_TAIL_LINES);
        assert!(formatted[TOOL_OUTPUT_HEAD_LINES].contains("lines hidden"));
    }
}
