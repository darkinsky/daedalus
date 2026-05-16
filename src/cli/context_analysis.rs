//! Context window usage analysis for the `/context` command.
//!
//! Provides detailed breakdown of how the context window is being used,
//! including system prompt, tools, memory, and message token distribution.

use std::collections::HashMap;

use crate::memory::estimate_tokens;

/// A category of context usage.
#[derive(Debug, Clone)]
pub struct ContextCategory {
    /// Human-readable name (e.g., "System prompt", "Messages").
    pub name: String,
    /// Estimated token count for this category.
    pub tokens: usize,
}

/// Breakdown of message tokens by type.
#[derive(Debug, Clone, Default)]
pub struct MessageBreakdown {
    /// Token count for user messages (excluding tool results).
    pub user_message_tokens: usize,
    /// Token count for assistant messages.
    pub assistant_message_tokens: usize,
    /// Token count for tool-context messages (tool results injected by middleware).
    pub tool_context_tokens: usize,
    /// Token count per tool name (tool results grouped by originating tool).
    pub tool_results_by_type: HashMap<String, usize>,
    /// Files read more than once in the conversation.
    pub duplicate_file_reads: Vec<DuplicateRead>,
}

/// A file that was read multiple times in the conversation.
#[derive(Debug, Clone)]
pub struct DuplicateRead {
    /// Approximate file path (extracted from tool context).
    pub path: String,
    /// Number of times read.
    pub count: usize,
    /// Estimated wasted tokens (count - 1) * avg_tokens_per_read.
    pub wasted_tokens: usize,
}

/// Optimization suggestion for the user.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// Severity: "warning" or "info".
    pub severity: &'static str,
    /// Human-readable suggestion text.
    pub message: String,
}

/// Complete context analysis result.
#[derive(Debug, Clone)]
pub struct ContextAnalysis {
    /// Token usage by category.
    pub categories: Vec<ContextCategory>,
    /// Total estimated tokens in use.
    pub total_tokens: usize,
    /// Model context window size.
    pub context_window: usize,
    /// Usage percentage (0-100).
    pub usage_percentage: f64,
    /// Detailed message breakdown.
    pub message_breakdown: MessageBreakdown,
    /// Optimization suggestions.
    pub suggestions: Vec<Suggestion>,
    /// Current pressure level name.
    pub pressure_level: String,
}

/// Analyze context usage from pre-built messages.
///
/// This is the main entry point called by the `/context` command handler.
/// It takes the messages that would be sent to the LLM and breaks them down.
pub fn analyze(
    messages: &[crate::llm::ChatMessage],
    context_window: usize,
    tool_count: usize,
) -> ContextAnalysis {
    use crate::llm::ChatRole;

    let mut system_tokens = 0usize;
    let mut user_tokens = 0usize;
    let mut assistant_tokens = 0usize;
    let mut tool_context_tokens = 0usize;
    let mut tool_results_by_type: HashMap<String, usize> = HashMap::new();

    // Track file reads for duplicate detection
    let mut file_read_counts: HashMap<String, (usize, usize)> = HashMap::new(); // path → (count, total_tokens)

    for msg in messages {
        let content = if msg.content_parts.is_empty() {
            &msg.content
        } else {
            // For multimodal, estimate text parts only
            &msg.content
        };
        let tokens = estimate_tokens(content);

        match msg.role {
            ChatRole::System => {
                system_tokens += tokens;
            }
            ChatRole::User => {
                user_tokens += tokens;
            }
            ChatRole::Assistant => {
                assistant_tokens += tokens;
            }
            ChatRole::Tool => {
                tool_context_tokens += tokens;

                // Try to categorize tool results by tool name
                // Tool context messages often start with a header like "## read_file ..."
                if let Some(tool_name) = extract_tool_name(content) {
                    *tool_results_by_type.entry(tool_name.clone()).or_default() += tokens;

                    // Track file reads for duplicate detection
                    if tool_name == "read_file" || tool_name == "grep_search" {
                        if let Some(path) = extract_file_path(content) {
                            let entry = file_read_counts.entry(path).or_insert((0, 0));
                            entry.0 += 1;
                            entry.1 += tokens;
                        }
                    }
                }
            }
        }
    }

    // Estimate tool definitions overhead (~50 tokens per tool on average)
    let tool_definition_tokens = tool_count * 50;

    // Build categories
    let mut categories = Vec::new();

    if system_tokens > 0 {
        categories.push(ContextCategory {
            name: "System prompt".to_string(),
            tokens: system_tokens,
        });
    }
    if tool_definition_tokens > 0 {
        categories.push(ContextCategory {
            name: "Tool definitions".to_string(),
            tokens: tool_definition_tokens,
        });
    }
    if user_tokens > 0 {
        categories.push(ContextCategory {
            name: "User messages".to_string(),
            tokens: user_tokens,
        });
    }
    if assistant_tokens > 0 {
        categories.push(ContextCategory {
            name: "Assistant messages".to_string(),
            tokens: assistant_tokens,
        });
    }
    if tool_context_tokens > 0 {
        categories.push(ContextCategory {
            name: "Tool results".to_string(),
            tokens: tool_context_tokens,
        });
    }

    let total_tokens = system_tokens + tool_definition_tokens + user_tokens
        + assistant_tokens + tool_context_tokens;
    let usage_percentage = if context_window > 0 {
        (total_tokens as f64 / context_window as f64) * 100.0
    } else {
        0.0
    };

    // Build duplicate reads
    let duplicate_file_reads: Vec<DuplicateRead> = file_read_counts
        .into_iter()
        .filter(|(_, (count, _))| *count > 1)
        .map(|(path, (count, total_tokens))| {
            let avg = total_tokens / count;
            DuplicateRead {
                path,
                count,
                wasted_tokens: avg * (count - 1),
            }
        })
        .collect();

    let message_breakdown = MessageBreakdown {
        user_message_tokens: user_tokens,
        assistant_message_tokens: assistant_tokens,
        tool_context_tokens,
        tool_results_by_type,
        duplicate_file_reads,
    };

    // Determine pressure level
    let pressure_level = if usage_percentage > 97.0 {
        "Critical".to_string()
    } else if usage_percentage > 93.0 {
        "High".to_string()
    } else if usage_percentage > 80.0 {
        "Warning".to_string()
    } else {
        "Normal".to_string()
    };

    // Generate suggestions
    let suggestions = generate_suggestions(
        usage_percentage,
        &message_breakdown,
        total_tokens,
        context_window,
    );

    ContextAnalysis {
        categories,
        total_tokens,
        context_window,
        usage_percentage,
        message_breakdown,
        suggestions,
        pressure_level,
    }
}

/// Generate optimization suggestions based on context analysis.
fn generate_suggestions(
    usage_pct: f64,
    breakdown: &MessageBreakdown,
    total_tokens: usize,
    _context_window: usize,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. Capacity warning
    if usage_pct > 80.0 {
        suggestions.push(Suggestion {
            severity: "warning",
            message: format!(
                "Context usage at {:.0}% — consider running /compact to compress history",
                usage_pct
            ),
        });
    }

    // 2. Large tool results
    if total_tokens > 0 {
        for (tool, tokens) in &breakdown.tool_results_by_type {
            let pct = (*tokens as f64 / total_tokens as f64) * 100.0;
            if pct > 20.0 {
                suggestions.push(Suggestion {
                    severity: "info",
                    message: format!(
                        "'{}' results occupy {:.0}% of context — use more targeted queries",
                        tool, pct
                    ),
                });
            }
        }
    }

    // 3. Duplicate file reads
    for dup in &breakdown.duplicate_file_reads {
        if dup.count > 2 {
            suggestions.push(Suggestion {
                severity: "info",
                message: format!(
                    "'{}' read {} times — ~{} tokens wasted on duplicates",
                    dup.path, dup.count, dup.wasted_tokens
                ),
            });
        }
    }

    // 4. Tool context bloat
    if total_tokens > 0 {
        let tool_pct = (breakdown.tool_context_tokens as f64 / total_tokens as f64) * 100.0;
        if tool_pct > 60.0 {
            suggestions.push(Suggestion {
                severity: "info",
                message: format!(
                    "Tool results occupy {:.0}% of context — consider /compact to summarize old results",
                    tool_pct
                ),
            });
        }
    }

    suggestions
}

/// Try to extract a tool name from tool-context message content.
///
/// Tool context messages from the middleware typically include markers like
/// `[tool: read_file]` or start with the tool name.
fn extract_tool_name(content: &str) -> Option<String> {
    // Pattern 1: "[tool: xxx]" marker (injected by some middleware paths)
    if let Some(start) = content.find("[tool: ") {
        let rest = &content[start + 7..];
        if let Some(end) = rest.find(']') {
            return Some(rest[..end].to_string());
        }
    }

    // Pattern 2: First line starts with a known tool name
    let first_line = content.lines().next().unwrap_or("");
    let known_tools = [
        "read_file", "edit_file", "multi_edit", "write_file",
        "bash", "grep_search", "list_directory", "search_files",
        "get_file_info", "take_note",
    ];
    for tool in &known_tools {
        if first_line.starts_with(tool) || first_line.contains(&format!("**{}**", tool)) {
            return Some(tool.to_string());
        }
    }

    None
}

/// Try to extract a file path from tool result content.
fn extract_file_path(content: &str) -> Option<String> {
    // Look for common patterns: "path/to/file" or `/abs/path`
    for line in content.lines().take(3) {
        let trimmed = line.trim();
        if (trimmed.starts_with('/') || trimmed.starts_with("src/") || trimmed.contains('.'))
            && !trimmed.contains(' ')
            && trimmed.len() < 200
        {
            return Some(trimmed.to_string());
        }
    }
    None
}
