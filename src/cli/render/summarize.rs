//! Turn raw tool-call arguments into human-readable one-liners.
//!
//! Used by the tool-event formatter to show the user what a tool is about
//! to do (e.g. `read_file src/foo.rs:100-200`) before the result arrives.
//! Pulled into `cli::render` via `#[path]`.

use crossterm::style::{Color, Stylize};

use crate::tools::truncate_chars;
use crate::tools::format_size;

use super::tool_output::unfold_bash_command;

/// Build a one-line human-readable summary of a tool call's arguments.
///
/// Returns `None` when nothing useful can be extracted (e.g. the arguments
/// are empty or the tool is unknown and carries no obvious fields).
pub(super) fn summarize_tool_args(
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    let str_field = |key: &str| -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let u64_field = |key: &str| -> Option<u64> { args.get(key).and_then(|v| v.as_u64()) };
    let bool_field = |key: &str| -> Option<bool> { args.get(key).and_then(|v| v.as_bool()) };

    match tool_name {
        "read_file" => {
            let path = str_field("path")?;
            match (u64_field("offset"), u64_field("limit")) {
                (Some(off), Some(lim)) => Some(format!("{}:{}-{}", path, off, off + lim)),
                (Some(off), None) => Some(format!("{} (from line {})", path, off)),
                (None, Some(lim)) => Some(format!("{} (first {} lines)", path, lim)),
                (None, None) => Some(path),
            }
        }
        "edit_file" => {
            let path = str_field("path")?;
            if bool_field("replace_all").unwrap_or(false) {
                Some(format!("{}  (replace_all)", path))
            } else {
                Some(path)
            }
        }
        "multi_edit" => {
            let path = str_field("path")?;
            let n = args
                .get("edits")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            Some(format!("{}  ({} edit{})", path, n, if n == 1 { "" } else { "s" }))
        }
        "write_file" => {
            let path = str_field("path")?;
            let bytes = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| c.len())
                .unwrap_or(0);
            Some(format!("{}  ({})", path, format_size(bytes as u64)))
        }
        "grep_search" => {
            let pattern = str_field("pattern")?;
            let scope = str_field("path").unwrap_or_else(|| ".".to_string());
            let mut tags: Vec<&str> = Vec::new();
            if !bool_field("use_regex").unwrap_or(true) {
                tags.push("literal");
            }
            if !bool_field("case_sensitive").unwrap_or(true) {
                tags.push("icase");
            }
            let suffix = if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join(", "))
            };
            Some(format!("{:?} in {}{}", truncate_chars(&pattern, 60), scope, suffix))
        }
        "search_files" => {
            let pattern = str_field("pattern")?;
            let scope = str_field("path").unwrap_or_else(|| ".".to_string());
            Some(format!("{} in {}", pattern, scope))
        }
        "list_directory" | "get_file_info" => str_field("path"),
        "bash" => {
            let command = args.get("command").and_then(|v| v.as_str())?;
            let cwd = args.get("working_directory").and_then(|v| v.as_str()).map(String::from);
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .or_else(|| args.get("timeout").and_then(|v| v.as_u64()));
            Some(summarize_bash(command, cwd, timeout))
        }
        "use_skill" => str_field("name").or_else(|| str_field("skill_name")),
        "spawn_subagent" | "spawn_team" => summarize_subagent(args),
        _ => {
            // Generic fallback: compact single-line JSON preview.
            let compact = serde_json::to_string(args).ok()?;
            if compact == "{}" || compact == "null" {
                None
            } else {
                Some(truncate_chars(&compact, 120))
            }
        }
    }
}

/// Render a bash-tool argument bundle as a shell-like block.
fn summarize_bash(cmd: &str, cwd: Option<String>, timeout: Option<u64>) -> String {
    let unfolded = unfold_bash_command(cmd);
    let mut out = format!("$ {}", unfolded);
    if let Some(dir) = cwd {
        out.push('\n');
        out.push_str(&format!("  [cwd: {}]", dir));
    }
    if let Some(t) = timeout {
        out.push('\n');
        out.push_str(&format!("  [timeout: {}s]", t));
    }
    out
}

/// Render a subagent or team spawn into a multi-line `agent / task` block.
fn summarize_subagent(args: &serde_json::Value) -> Option<String> {
    let str_field = |key: &str| -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };

    let mut header_parts: Vec<String> = Vec::new();
    if let Some(name) = str_field("agent_name").or_else(|| str_field("name")) {
        header_parts.push(format!("agent: {}", name));
    }
    if let Some(team) = args.get("agents").and_then(|v| v.as_array()) {
        let names: Vec<String> = team
            .iter()
            .filter_map(|it| {
                it.get("agent_name").and_then(|v| v.as_str()).map(|s| s.to_string())
            })
            .collect();
        if !names.is_empty() {
            header_parts.push(format!("team: [{}]", names.join(", ")));
        }
    }
    let task = str_field("task").unwrap_or_default();
    let mut out = if header_parts.is_empty() {
        String::new()
    } else {
        header_parts.join("  ")
    };
    if !task.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let normalized = task.replace("\r\n", "\n").replace('\r', "\n");
        let mut first = true;
        for line in normalized.split('\n') {
            if first {
                out.push_str(&format!("task: {}", line));
                first = false;
            } else {
                out.push('\n');
                out.push_str(&format!("      {}", line));
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Build a tiny inline diff preview for editing tools.
///
/// For `edit_file` / `multi_edit`, shows the first `old_string` → `new_string`
/// replacement as two colored lines. Each string is trimmed to a single
/// line and truncated to keep the UI tidy. For `multi_edit`, only the first
/// edit is previewed, with a trailing "(+N more)" hint when there are
/// additional edits.
pub(super) fn edit_diff_preview(
    tool_name: &str,
    args: &serde_json::Value,
) -> Vec<String> {
    const MAX_CHARS: usize = 100;
    let mut out: Vec<String> = Vec::new();

    let format_pair = |old: &str, new: &str, extra: Option<String>| -> Vec<String> {
        let old_preview =
            truncate_chars(old.lines().next().unwrap_or("").trim_end(), MAX_CHARS);
        let new_preview =
            truncate_chars(new.lines().next().unwrap_or("").trim_end(), MAX_CHARS);
        let mut lines = vec![
            format!("{} {}", "-".with(Color::Red), old_preview.with(Color::Red)),
            format!("{} {}", "+".with(Color::Green), new_preview.with(Color::Green)),
        ];
        if let Some(tail) = extra {
            lines.push(tail.with(Color::DarkGrey).to_string());
        }
        lines
    };

    match tool_name {
        "edit_file" => {
            let old_s = args.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
            let new_s = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            if !old_s.is_empty() || !new_s.is_empty() {
                out.extend(format_pair(old_s, new_s, None));
            }
        }
        "multi_edit" => {
            if let Some(edits) = args.get("edits").and_then(|v| v.as_array()) {
                if let Some(first) = edits.first() {
                    let old_s = first
                        .get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_s = first
                        .get("new_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let extra = if edits.len() > 1 {
                        Some(format!(
                            "(+{} more edit{})",
                            edits.len() - 1,
                            if edits.len() - 1 == 1 { "" } else { "s" }
                        ))
                    } else {
                        None
                    };
                    out.extend(format_pair(old_s, new_s, extra));
                }
            }
        }
        _ => {}
    }

    out
}


