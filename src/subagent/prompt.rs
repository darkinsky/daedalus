//! Subagent prompt enhancement — injects tool guidance, environment context,
//! and safety constraints into the subagent's system prompt.
//!
//! ## Why this exists
//!
//! Claude Code's SubAgent (AgentTool/forkSubagent) builds a **complete but
//! compact** system prompt for each sub-agent that includes:
//!
//! 1. The agent's own identity/task description (from the definition)
//! 2. A tool inventory listing available tools and their descriptions
//! 3. Tool usage strategy (parallel execution, error handling, etc.)
//! 4. Environment context (OS, shell, CWD, project type)
//! 5. Safety constraints (no destructive operations, etc.)
//!
//! Without this, the subagent only receives the raw `definition.system_prompt`
//! and relies entirely on the API-level `tools` parameter for tool awareness.
//! While modern LLMs can use tools from API definitions alone, they perform
//! significantly better with explicit usage strategies in the system prompt.
//!
//! ## What this does NOT include
//!
//! - DAEDALUS.md / project rules (subagent context is isolated)
//! - Memory context (subagent has no conversation history)
//! - Main agent's soul/personality (subagent has its own identity)
//! - Full prompt builder sections (kept compact to save tokens)

use crate::tools::ToolInfo;

/// Build the effective system prompt for a subagent.
///
/// Appends tool guidance, environment context, and safety constraints
/// to the subagent's base system prompt. If no tools are available,
/// only environment context is appended.
///
/// This mirrors Claude Code's `forkSubagent` behavior: the sub-agent
/// gets a self-contained prompt with everything it needs to operate
/// effectively, without inheriting the parent agent's full prompt.
pub fn build_effective_prompt(
    base_prompt: &str,
    tool_infos: &[ToolInfo],
    has_tools: bool,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);

    // 1. The agent's own system prompt (identity + task description)
    parts.push(base_prompt.to_string());

    // 2. Environment context (always included)
    parts.push(build_environment_section());

    // 3. Tool guidance (only if tools are available)
    if has_tools && !tool_infos.is_empty() {
        parts.push(build_tool_guidance_section(tool_infos));
    }

    // 4. Safety constraints (always included)
    parts.push(build_constraints_section(has_tools));

    parts.join("\n\n")
}

/// Build the environment context section.
///
/// Provides the subagent with basic runtime information so it can
/// make informed decisions about file paths, shell commands, etc.
fn build_environment_section() -> String {
    let os = std::env::consts::OS;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    format!(
        "<environment>\n\
         Operating system: {os}\n\
         Default shell: {shell}\n\
         Current directory: {cwd}\n\
         Today's date: {today}\n\
         </environment>"
    )
}

/// Build the tool guidance section for subagents.
///
/// This is a **compact** version of the main agent's tool guidance,
/// optimized for token efficiency while preserving the key strategies
/// that improve tool usage quality.
fn build_tool_guidance_section(tool_infos: &[ToolInfo]) -> String {
    // Build tool inventory
    let tool_list: Vec<String> = tool_infos
        .iter()
        .map(|t| {
            format!(
                "- **{}**: {}",
                t.name,
                if t.description.is_empty() {
                    "No description"
                } else {
                    &t.description
                }
            )
        })
        .collect();

    let inventory = tool_list.join("\n");

    format!(
        "<tools>\n\
         ## Available Tools\n\n\
         {inventory}\n\
         \n\
         ## Tool Usage Strategy\n\
         \n\
         1. **Maximize parallel execution**: When multiple independent operations are needed, \
         execute ALL of them simultaneously.\n\
            - Reading multiple files → parallel\n\
            - Searching different patterns → parallel\n\
            - Only serialize when output of one call is required input for the next\n\
         \n\
         2. **Gather before acting**: Before making changes, first understand the full context. \
         Read relevant files, search for usages, check imports — all in parallel.\n\
         \n\
         3. **Right tool for the job**:\n\
            - Exact text/symbol lookup → grep_search\n\
            - Known file path → read_file directly\n\
            - File name search → search_files\n\
            - Directory listing → list_directory\n\
            - Shell commands → bash\n\
         \n\
         4. **Error recovery**: If a tool call fails, analyze why and try a different approach. \
         Do not retry with identical arguments. After 3 failures, switch strategy entirely.\n\
         \n\
         5. **Efficiency**: If you can answer from context already gathered, do so without \
         additional tool calls. Minimize unnecessary operations.\n\
         </tools>"
    )
}

/// Build the safety constraints section.
///
/// Provides guardrails that apply to all subagents regardless of their
/// specific role. These mirror Claude Code's SubAgent constraints.
fn build_constraints_section(has_tools: bool) -> String {
    let mut constraints = Vec::new();

    constraints.push(
        "You are operating in an isolated subagent context with an independent conversation window."
    );
    constraints.push(
        "You do NOT have access to the parent agent's conversation history or memory."
    );
    constraints.push(
        "Your response will be returned to the parent agent as a summary — be thorough but concise."
    );

    if has_tools {
        constraints.push(
            "Present tool results naturally as part of your analysis. \
             Never mention internal tool names or protocols to the user."
        );
        constraints.push(
            "Do not fabricate tool arguments. If you lack required information, \
             search for it using available tools."
        );
    }

    constraints.push(
        "Respond in the same language as the task description. \
         If the task is in Chinese, all output (including intermediate thoughts \
         and the final report) must be in Chinese. Never switch languages mid-response."
    );

    let items: Vec<String> = constraints
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c))
        .collect();

    format!(
        "<constraints>\n\
         {}\n\
         </constraints>",
        items.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_effective_prompt_with_tools() {
        let base = "You are a code reviewer.";
        let tools = vec![
            ToolInfo {
                name: "read_file".to_string(),
                description: "Read file contents".to_string(),
                source: "built-in".to_string(),
            },
            ToolInfo {
                name: "grep_search".to_string(),
                description: "Search with regex".to_string(),
                source: "built-in".to_string(),
            },
        ];

        let prompt = build_effective_prompt(base, &tools, true);

        // Should contain the base prompt
        assert!(prompt.starts_with("You are a code reviewer."));
        // Should contain environment section
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("Operating system:"));
        assert!(prompt.contains("Current directory:"));
        // Should contain tool guidance
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("grep_search"));
        assert!(prompt.contains("Maximize parallel execution"));
        // Should contain constraints
        assert!(prompt.contains("<constraints>"));
        assert!(prompt.contains("isolated subagent context"));
    }

    #[test]
    fn test_build_effective_prompt_without_tools() {
        let base = "You are a planning agent.";
        let prompt = build_effective_prompt(base, &[], false);

        // Should contain the base prompt
        assert!(prompt.starts_with("You are a planning agent."));
        // Should contain environment section
        assert!(prompt.contains("<environment>"));
        // Should NOT contain tool guidance
        assert!(!prompt.contains("<tools>"));
        // Should contain constraints (without tool-specific ones)
        assert!(prompt.contains("<constraints>"));
        assert!(prompt.contains("isolated subagent context"));
        assert!(!prompt.contains("tool names or protocols"));
    }

    #[test]
    fn test_environment_section_has_required_fields() {
        let section = build_environment_section();
        assert!(section.contains("Operating system:"));
        assert!(section.contains("Default shell:"));
        assert!(section.contains("Current directory:"));
        assert!(section.contains("Today's date:"));
    }

    #[test]
    fn test_tool_guidance_lists_all_tools() {
        let tools = vec![
            ToolInfo {
                name: "bash".to_string(),
                description: "Execute shell commands".to_string(),
                source: "built-in".to_string(),
            },
            ToolInfo {
                name: "list_directory".to_string(),
                description: "List directory contents".to_string(),
                source: "built-in".to_string(),
            },
        ];

        let section = build_tool_guidance_section(&tools);
        assert!(section.contains("bash"));
        assert!(section.contains("list_directory"));
        assert!(section.contains("Execute shell commands"));
    }

    #[test]
    fn test_constraints_with_tools() {
        let section = build_constraints_section(true);
        assert!(section.contains("tool names or protocols"));
        assert!(section.contains("Do not fabricate"));
    }

    #[test]
    fn test_constraints_without_tools() {
        let section = build_constraints_section(false);
        assert!(!section.contains("tool names or protocols"));
        assert!(!section.contains("Do not fabricate"));
    }
}
