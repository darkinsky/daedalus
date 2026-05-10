//! Coding-focused prompt assembly.
//!
//! This module implements a prompt architecture optimized for autonomous coding
//! tasks. Key differences from the default PromptBuilder:
//!
//! - **Cache boundary**: Static prefix separated from dynamic suffix for
//!   API prompt caching optimization.
//! - **Environment awareness**: Injects OS, shell, CWD, project type.
//! - **Agentic coding focus**: Role is "autonomous coding agent", not generic assistant.
//! - **Per-tool strategies**: Detailed when-to-use / when-not-to-use guidance.
//! - **Project rules injection**: Supports loading rules from workspace.
//! - **Parallel tool execution emphasis**: Strong guidance on concurrent tool use.

pub mod sections;

use crate::tools::ToolInfo;
use super::inputs::PromptInputs;

/// Environment context for the coding-focused prompt.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentContext {
    /// Operating system name (e.g., "linux", "macos", "windows").
    pub os: String,
    /// Default shell (e.g., "bash", "zsh", "fish").
    pub shell: String,
    /// Current working directory.
    pub cwd: String,
    /// Detected project type (e.g., "Rust/Cargo", "Node/npm").
    pub project_type: Option<String>,
}

impl EnvironmentContext {
    /// Detect environment context from the current system.
    pub fn detect(cwd: &str) -> Self {
        let os = std::env::consts::OS.to_string();
        let shell = std::env::var("SHELL")
            .unwrap_or_else(|_| "/bin/bash".to_string());

        // Detect project type from common files
        let project_type = Self::detect_project_type(cwd);

        Self {
            os,
            shell,
            cwd: cwd.to_string(),
            project_type,
        }
    }

    fn detect_project_type(cwd: &str) -> Option<String> {
        use std::path::Path;
        let root = Path::new(cwd);

        if root.join("Cargo.toml").exists() {
            Some("Rust/Cargo".to_string())
        } else if root.join("package.json").exists() {
            if root.join("bun.lockb").exists() {
                Some("Node/Bun".to_string())
            } else if root.join("pnpm-lock.yaml").exists() {
                Some("Node/pnpm".to_string())
            } else if root.join("yarn.lock").exists() {
                Some("Node/Yarn".to_string())
            } else {
                Some("Node/npm".to_string())
            }
        } else if root.join("go.mod").exists() {
            Some("Go".to_string())
        } else if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
            Some("Python".to_string())
        } else if root.join("pom.xml").exists() {
            Some("Java/Maven".to_string())
        } else if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
            Some("Java/Gradle".to_string())
        } else {
            None
        }
    }
}

/// Coding-focused prompt builder.
///
/// Assembles a system prompt optimized for autonomous coding tasks:
/// ```text
/// ┌─────────────────────────────────────────┐
/// │  STATIC PREFIX (cacheable)              │
/// │  ├─ Identity & Capabilities             │
/// │  ├─ Tool Definitions & Strategies       │
/// │  └─ Core Rules & Guardrails             │
/// ├─────────────────────────────────────────┤
/// │  CACHE BOUNDARY MARKER                  │
/// ├─────────────────────────────────────────┤
/// │  DYNAMIC SUFFIX (per-session)           │
/// │  ├─ Environment Context                 │
/// │  ├─ Project Rules                       │
/// │  └─ Memory Context                      │
/// └─────────────────────────────────────────┘
/// ```
pub struct CodingPromptBuilder<'a> {
    /// Shared input fields (agent_name, tools, soul, project_rules, memory_context).
    inputs: PromptInputs<'a>,
    /// Runtime environment context.
    environment: Option<EnvironmentContext>,
}

impl<'a> CodingPromptBuilder<'a> {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            inputs: PromptInputs::new(),
            environment: None,
        }
    }

    /// Set a custom agent name.
    pub fn agent_name(mut self, name: &'a str) -> Self {
        self.inputs.agent_name = Some(name);
        self
    }

    /// Set the available MCP tools.
    pub fn tools(mut self, tools: &'a [ToolInfo]) -> Self {
        self.inputs.tools = tools;
        self
    }

    /// Set the long-term memory context to inject.
    #[allow(dead_code)]
    pub fn memory_context(mut self, ctx: &'a str) -> Self {
        self.inputs.memory_context = Some(ctx);
        self
    }

    /// Set a custom soul/personality preamble.
    pub fn soul(mut self, soul: &'a str) -> Self {
        self.inputs.soul = Some(soul);
        self
    }

    /// Set the runtime environment context.
    pub fn environment(mut self, env: EnvironmentContext) -> Self {
        self.environment = Some(env);
        self
    }

    /// Set project-level rules.
    #[allow(dead_code)]
    pub fn project_rules(mut self, rules: &'a str) -> Self {
        self.inputs.project_rules = Some(rules);
        self
    }

    /// Assemble the final system prompt.
    ///
    /// The prompt is split into static prefix and dynamic suffix,
    /// separated by a cache boundary marker for API-level caching.
    pub fn build(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(12);

        // ═══ STATIC PREFIX (cacheable across sessions) ═══

        // 1. Identity & Capabilities
        parts.push(sections::identity::build(self.inputs.agent_name, self.inputs.tools));

        // 2. Soul / personality (optional, but static once loaded)
        if let Some(soul) = self.inputs.soul {
            if !soul.trim().is_empty() {
                parts.push(format!("<personality>\n{}\n</personality>", soul.trim()));
            }
        }

        // 3. Tool definitions with per-tool strategies
        let tool_section = sections::tools::build(self.inputs.tools);
        if !tool_section.is_empty() {
            parts.push(tool_section);
        }

        // 4. Core behavioral rules (agentic coding focus)
        parts.push(sections::rules::build(self.inputs.tools));

        // ═══ CACHE BOUNDARY ═══
        parts.push("<!-- SYSTEM_PROMPT_DYNAMIC_BOUNDARY -->".to_string());

        // ═══ DYNAMIC SUFFIX (changes per session/turn) ═══

        // 5. Environment context
        parts.push(sections::environment::build(self.environment.as_ref()));

        // 6. Project rules (shared helper from PromptInputs)
        if let Some(rules_section) = self.inputs.project_rules_section() {
            parts.push(rules_section);
        }

        // 7. Memory context (shared helper from PromptInputs)
        if let Some(memory_section) = self.inputs.memory_section() {
            parts.push(memory_section);
        }

        // 8. Critical reminders (last = highest salience via recency bias)
        parts.push(sections::reminders::build());

        parts.join("\n\n")
    }
}

impl<'a> Default for CodingPromptBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_build() {
        let prompt = CodingPromptBuilder::new().build();
        assert!(prompt.contains("<identity>"));
        assert!(prompt.contains("SYSTEM_PROMPT_DYNAMIC_BOUNDARY"));
        assert!(prompt.contains("<environment>"));
        assert!(prompt.contains("<critical_reminders>"));
    }

    #[test]
    fn test_cache_boundary_separates_static_and_dynamic() {
        let env = EnvironmentContext {
            os: "linux".to_string(),
            shell: "/bin/bash".to_string(),
            cwd: "/home/user/project".to_string(),
            project_type: Some("Rust/Cargo".to_string()),
        };
        let prompt = CodingPromptBuilder::new()
            .environment(env)
            .build();

        let boundary_pos = prompt.find("SYSTEM_PROMPT_DYNAMIC_BOUNDARY").unwrap();
        let env_pos = prompt.find("<environment>").unwrap();
        let identity_pos = prompt.find("<identity>").unwrap();

        // Identity is before boundary (static)
        assert!(identity_pos < boundary_pos);
        // Environment is after boundary (dynamic)
        assert!(env_pos > boundary_pos);
    }

    #[test]
    fn test_with_tools() {
        let tools = vec![ToolInfo {
            name: "read_file".to_string(),
            description: "Read file contents".to_string(),
            source: "built-in".to_string(),
        }];
        let prompt = CodingPromptBuilder::new().tools(&tools).build();
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("read_file"));
    }

    #[test]
    fn test_project_type_detection() {
        // Just test that detect doesn't panic
        let env = EnvironmentContext::detect("/tmp");
        assert_eq!(env.os, std::env::consts::OS);
    }
}
