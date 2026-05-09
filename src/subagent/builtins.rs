//! Built-in subagent definitions hardcoded in the binary.
//!
//! These provide a baseline set of subagents that are always available,
//! even without any `.md` files on disk. Users can override any built-in
//! by placing a file with the same name in their project or global
//! agents directory.

use super::{IsolationMode, PermissionMode, SubagentDefinition, SubagentSource};

/// Return all built-in subagent definitions.
///
/// Built-in subagents have the lowest priority (`SubagentSource::Builtin`)
/// and will be overridden by project-level or global definitions with
/// the same name.
pub fn builtin_agents() -> Vec<SubagentDefinition> {
    vec![
        explore_agent(),
        code_reviewer_agent(),
        plan_agent(),
    ]
}

/// Standard read-only tool whitelist shared by all built-in subagents.
/// Includes `bash` (for `wc -l`, `find`, etc.) and `take_note` (for
/// persisting findings across context truncation).
const READ_ONLY_TOOLS: &[&str] = &["read_file", "list_directory", "search_files", "grep_search", "get_file_info", "bash", "take_note"];

/// Create a read-only built-in subagent with standard defaults.
///
/// All built-in agents share the same tool whitelist, permission mode (`Plan`),
/// source (`Builtin`), and isolation (`None`). Only the name, description,
/// system prompt, and max turns differ.
fn read_only_builtin(
    name: &str,
    description: &str,
    system_prompt: &str,
    max_turns: usize,
) -> SubagentDefinition {
    SubagentDefinition {
        name: name.to_string(),
        description: description.to_string(),
        system_prompt: system_prompt.to_string(),
        model: None,
        tools: Some(READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect()),
        disallowed_tools: None,
        permission_mode: PermissionMode::Plan,
        max_turns: Some(max_turns),
        source: SubagentSource::Builtin,
        isolation: IsolationMode::None,
        on_start: None,
        on_complete: None,
    }
}

/// Built-in "explore" subagent — read-only code exploration and analysis.
fn explore_agent() -> SubagentDefinition {
    read_only_builtin(
        "explore",
        "Read-only code exploration and analysis agent. \
            Use when the user asks to search, analyze, or understand code \
            without making any changes. Ideal for large codebase exploration \
            where intermediate output would clutter the main conversation.",
        "\
You are a code exploration specialist working in an isolated context.

Your job is to search and analyze codebases efficiently. You can ONLY
read files — never modify them.

## Guidelines

1. Start with a broad search to understand project structure
2. Drill down into specific files based on the user's question
3. Cross-reference imports and dependencies to trace code flow
4. Provide clear, structured summaries of what you find

## Output Format

Organize your findings as:
- **Summary**: One-paragraph answer to the user's question
- **Key Files**: List of relevant files with brief descriptions
- **Details**: Deeper analysis with code snippets if needed
- **Suggestions**: Next steps or related areas to explore",
        30,
    )
}

/// Built-in "code-reviewer" subagent — code quality review and audit.
fn code_reviewer_agent() -> SubagentDefinition {
    read_only_builtin(
        "code-reviewer",
        "Reviews code for quality, best practices, and potential issues. \
            Best for reviewing a single module or focused subset (≤50 files). \
            For full-project reviews, decompose into multiple parallel \
            code-reviewer invocations scoped by module boundary.",
        "\
You are a senior code reviewer working in an isolated context.

Analyze code and provide actionable feedback organized by severity:
- **Critical**: Bugs, security vulnerabilities, data loss risks
- **Major**: Performance issues, design problems, missing error handling
- **Minor**: Style inconsistencies, naming improvements, documentation gaps

## Review Process

1. Read the target files thoroughly
2. Check for common issues: error handling, edge cases, resource leaks
3. Verify naming conventions and code style consistency
4. Look for potential performance bottlenecks
5. Check test coverage implications

## Output Format

For each issue found:
```
[SEVERITY] file:line — Brief description
  Context: What the code does
  Problem: What's wrong
  Fix: Suggested improvement
```

End with a summary: total issues by severity and overall code quality assessment.",
        30,
    )
}

/// Built-in "plan" subagent — architecture analysis and implementation planning.
fn plan_agent() -> SubagentDefinition {
    read_only_builtin(
        "plan",
        "Analyzes codebases and creates detailed implementation plans. \
            Use when the user asks to plan, design, or architect a solution \
            before writing code. Gathers context from the codebase and \
            produces a structured plan with file changes and dependencies.",
        "\
You are a software architect and planning specialist working in an isolated context.

Your job is to analyze codebases and create detailed, actionable implementation
plans. You can ONLY read files — never modify them.

## Planning Process

1. **Understand the goal**: Clarify what needs to be built or changed
2. **Explore the codebase**: Search for relevant files, patterns, and conventions
3. **Identify dependencies**: Map out what existing code will be affected
4. **Design the solution**: Choose the approach that best fits the existing architecture
5. **Create the plan**: Write a step-by-step implementation guide

## Output Format

Structure your plan as:

### Goal
One-paragraph summary of what we're building.

### Codebase Analysis
- Key files and their roles
- Existing patterns to follow
- Potential conflicts or risks

### Implementation Plan
For each step:
1. **File**: Which file to create/modify
2. **Change**: What to add/change
3. **Rationale**: Why this approach
4. **Dependencies**: What must be done first

### Testing Strategy
- What tests to add
- Edge cases to consider

### Estimated Complexity
Low / Medium / High — with justification.",
        30,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_agents_count() {
        let agents = builtin_agents();
        assert_eq!(agents.len(), 3);
    }

    #[test]
    fn test_builtin_agents_names() {
        let agents = builtin_agents();
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"explore"));
        assert!(names.contains(&"code-reviewer"));
        assert!(names.contains(&"plan"));
    }

    #[test]
    fn test_builtin_agents_source() {
        let agents = builtin_agents();
        for agent in &agents {
            assert_eq!(agent.source, SubagentSource::Builtin);
        }
    }

    #[test]
    fn test_builtin_agents_have_read_only_tools() {
        let agents = builtin_agents();
        for agent in &agents {
            let tools = agent.tools.as_ref().expect("Built-in agents should have tool whitelist");
            assert!(tools.contains(&"read_file".to_string()));
            assert!(!tools.contains(&"write_file".to_string()));
        }
    }

    #[test]
    fn test_builtin_agents_have_system_prompts() {
        let agents = builtin_agents();
        for agent in &agents {
            assert!(!agent.system_prompt.is_empty(), "Agent '{}' has empty system prompt", agent.name);
            assert!(agent.system_prompt.len() > 50, "Agent '{}' has suspiciously short system prompt", agent.name);
        }
    }

    #[test]
    fn test_builtin_agents_have_descriptions() {
        let agents = builtin_agents();
        for agent in &agents {
            assert!(!agent.description.is_empty(), "Agent '{}' has empty description", agent.name);
        }
    }
}
