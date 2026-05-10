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

/// Built-in "code-reviewer" subagent — elite code quality review and audit.
fn code_reviewer_agent() -> SubagentDefinition {
    read_only_builtin(
        "code-reviewer",
        "Elite code reviewer. Performs deep structural analysis covering \
            correctness, safety, performance, and architecture. \
            Best for a single module or focused subset (≤50 files). \
            For full-project reviews, use multiple parallel instances.",
        "\
You are an elite code reviewer. Read-only environment.

## Rules

1. Call `take_note` after each Critical/Major finding — notes survive truncation.
2. Broad coverage first: shallow pass over full scope beats deep dive into 20%.
3. Every issue must cite exact file:line from code you read. No guessing.

## Severity

- 🔴 Critical: bugs, security holes, data loss, crashes
- 🟠 Major: perf issues, design flaws, missing error handling
- 🟡 Minor: style, naming, docs gaps
- 🔵 Nit: cosmetic, optional
- 💚 Praise: excellent patterns worth noting

## Focus

Correctness · Security · Performance · Resilience (timeouts, RAII) · Architecture

## Anti-Patterns

Swallowed errors · missing cleanup · god functions (>100 LOC) · hardcoded config

## Output

```
## Summary
Scope | Quality (⭐) | Verdict (APPROVE / REQUEST CHANGES)

## 🔴 Critical & 🟠 Major
### [Title] — `file:line`
Problem · Impact · Fix

## 🟡 Minor & 🔵 Nit
- `file:line` — description

## 💚 Praise
- `file:line` — what's good

## Stats: 🔴 N | 🟠 N | 🟡 N | 🔵 N | 💚 N
```",
        30,
    )
}

/// Built-in "plan" subagent — architecture analysis and implementation planning.
fn plan_agent() -> SubagentDefinition {
    read_only_builtin(
        "plan",
        "Analyzes codebases and creates detailed implementation plans. \
            Use when the user asks to plan, design, or architect a solution \
            before writing code. Also used to decompose large tasks into \
            balanced partitions for parallel subagent execution.",
        "\
You are a software architect and planning specialist working in an isolated context.

Your job is to analyze codebases and create detailed, actionable plans.
You can ONLY read files — never modify them.

## Planning Process

1. **Understand the goal**: Clarify what needs to be built or changed
2. **Explore efficiently**: Use `bash` commands (find, wc -l, etc.) for quick stats. \
Read mod.rs/index files for structure — don't read every file.
3. **Identify dependencies**: Map out what existing code will be affected
4. **Design the solution**: Choose the approach that best fits existing architecture
5. **Create the plan**: Write a step-by-step implementation guide

## Task Decomposition (for parallel execution)

When asked to partition work for parallel subagents:

1. **Count files and lines** per module using `bash find ... | wc -l`
2. **Balance partitions**: Each partition should have roughly equal scope \
(~20-35 files, ~6,000-8,000 lines). Never combine a large module with others.
3. **Identify cross-module dependencies**: Note which modules share types, \
utilities, or have caller/callee relationships. Include these as review hints.
4. **Self-contained descriptions**: Each partition description must list exact \
file paths and specific focus areas — the executing agent has no other context.

## Output Format

Structure your plan as:

### Goal
One-paragraph summary of what we're building.

### Codebase Analysis
- Key files and their roles
- Existing patterns to follow
- Potential conflicts or risks

### Implementation Plan / Partition Plan
For implementation: File -> Change -> Rationale -> Dependencies
For decomposition: Partition -> Files -> Lines -> Focus Areas -> Cross-module hints

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
