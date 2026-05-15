pub mod coding;
pub mod inputs;
pub mod sections;
pub mod shared_reminders;

use crate::tools::ToolInfo;

use sections::context::build_context_section;
use sections::reminders::build_reminders_section;
use sections::response_style::build_response_style_section;
use sections::role::build_role_section;
use sections::thinking::build_thinking_section;
use sections::tool_guidance::build_tool_guidance_section;

// Re-export the coding prompt's environment context for external use.
#[allow(unused_imports)]
pub use coding::EnvironmentContext;

// ── Unified prompt construction ──

/// Build a system prompt using the specified style and parameters.
///
/// This is the single entry point for all prompt construction, centralizing
/// the style dispatch logic that was previously embedded in `ChatAgent`.
///
/// If `prompt_override` is `Some`, it is returned directly, bypassing all
/// style-specific builders. Otherwise, the appropriate builder is selected
/// based on `style`.
///
/// # Arguments
/// * `prompt_override` — Custom system prompt (from CLI `--system-prompt`).
/// * `agent_name` — Optional custom agent name.
/// * `soul` — Optional personality content (from SOUL.md).
/// * `project_rules` — Optional project rules content (from DAEDALUS.md files).
/// * `tools` — Available tool descriptions for prompt injection.
/// * `style` — Which prompt architecture to use (Default vs Coding).
/// * `cwd` — Current working directory (used by Coding style for environment detection).
pub fn build_system_prompt(
    prompt_override: Option<&str>,
    agent_name: Option<&str>,
    soul: Option<&str>,
    project_rules: Option<&str>,
    tools: &[ToolInfo],
    style: &PromptStyle,
    cwd: Option<&str>,
) -> String {
    // If custom prompt is set, use it directly regardless of style
    if let Some(custom) = prompt_override {
        return custom.to_string();
    }

    match style {
        PromptStyle::Default => {
            let mut builder = PromptBuilder::new().tools(tools);
            if let Some(name) = agent_name {
                builder = builder.agent_name(name);
            }
            if let Some(soul_content) = soul {
                builder = builder.soul(soul_content);
            }
            if let Some(rules) = project_rules {
                builder = builder.project_rules(rules);
            }
            builder.build()
        }
        PromptStyle::Coding => {
            use coding::{CodingPromptBuilder, EnvironmentContext};

            let resolved_cwd = cwd
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string())
                });

            let env = EnvironmentContext::detect(&resolved_cwd);

            let mut builder = CodingPromptBuilder::new()
                .tools(tools)
                .environment(env);

            if let Some(name) = agent_name {
                builder = builder.agent_name(name);
            }
            if let Some(soul_content) = soul {
                builder = builder.soul(soul_content);
            }
            if let Some(rules) = project_rules {
                builder = builder.project_rules(rules);
            }

            builder.build()
        }
    }
}

/// Prompt assembly style selection.
///
/// Controls which prompt architecture is used to build the system prompt.
/// Can be set via YAML config (`agent.prompt_style`) or CLI (`--prompt-style`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptStyle {
    /// Original Daedalus prompt — generic AI assistant with XML sections.
    #[default]
    Default,
    /// Coding-focused prompt — autonomous coding agent with cache boundary,
    /// environment awareness, and agentic coding focus.
    Coding,
}

impl std::fmt::Display for PromptStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Coding => write!(f, "coding"),
        }
    }
}

/// Configuration for building a system prompt.
///
/// Uses `PromptInputs` for shared fields, eliminating duplication
/// with `CodingPromptBuilder`.
///
/// # Example
///
/// ```rust
/// use daedalus::prompt::PromptBuilder;
///
/// let prompt = PromptBuilder::new()
///     .agent_name("Atlas")
///     .tools(&tool_list)
///     .memory_context("User prefers Rust.")
///     .build();
/// ```
pub struct PromptBuilder<'a> {
    /// Shared input fields (agent_name, tools, soul, project_rules, memory_context).
    inputs: inputs::PromptInputs<'a>,
}

impl<'a> PromptBuilder<'a> {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            inputs: inputs::PromptInputs::new(),
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

    /// Set a custom soul/personality preamble (e.g., from SOUL.md).
    pub fn soul(mut self, soul: &'a str) -> Self {
        self.inputs.soul = Some(soul);
        self
    }

    /// Set project-level rules (loaded from DAEDALUS.md files).
    pub fn project_rules(mut self, rules: &'a str) -> Self {
        self.inputs.project_rules = Some(rules);
        self
    }

    /// Set language preference (e.g., "Chinese", "English").
    #[allow(dead_code)]
    pub fn language_preference(mut self, lang: &'a str) -> Self {
        self.inputs.language_preference = Some(lang);
        self
    }

    /// Add an extra dynamic section to be appended after all standard sections.
    #[allow(dead_code)]
    pub fn extra_section(mut self, section: String) -> Self {
        self.inputs.extra_sections.push(section);
        self
    }

    /// Assemble the final system prompt from all configured sections.
    ///
    /// The prompt is assembled in a deliberate order optimized for KV cache:
    ///
    /// **Static prefix** (cacheable across requests):
    /// 1. **Role** — Who am I? What can I do?
    /// 2. **Soul** — Personality and behavioral guardrails (optional)
    /// 3. **Thinking Style** — How should I reason?
    /// 4. **Tool Guidance** — How do I use tools? (only if tools available)
    /// 5. **Response Style** — How should I format output?
    /// 6. **Critical Reminders** — Hard rules that must not be violated
    ///
    /// **Dynamic suffix** (changes per session/turn):
    /// 7. **Project Rules** — Workspace-specific rules (semi-static)
    /// 8. **Context** — Dynamic runtime info (date, memory)
    ///
    /// This ordering ensures the static prefix is identical across requests,
    /// maximizing KV cache hit rate for both implicit (OpenAI) and explicit
    /// (Anthropic) prompt caching.
    pub fn build(&self) -> String {
        let has_tools = self.inputs.has_tools();

        let mut sections: Vec<String> = Vec::with_capacity(8);

        // ═══ STATIC PREFIX (cacheable across requests) ═══

        // 1. Role definition
        sections.push(build_role_section(self.inputs.agent_name, self.inputs.tools));

        // 2. Soul / personality (optional)
        if let Some(soul) = self.inputs.soul {
            if !soul.trim().is_empty() {
                sections.push(format!("<soul>\n{}\n</soul>", soul.trim()));
            }
        }

        // 3. Thinking style
        sections.push(build_thinking_section(has_tools));

        // 4. Tool guidance (only if tools are available)
        let tool_section = build_tool_guidance_section(self.inputs.tools);
        if !tool_section.is_empty() {
            sections.push(tool_section);
        }

        // 5. Response style
        sections.push(build_response_style_section());

        // 6. Critical reminders (static guardrails)
        sections.push(build_reminders_section(has_tools));

        // ═══ CACHE BOUNDARY ═══
        sections.push("<!-- CACHE_BOUNDARY -->".to_string());

        // ═══ DYNAMIC SUFFIX (changes per session/turn) ═══

        // 7. Project rules from DAEDALUS.md (optional, semi-static)
        if let Some(rules_section) = self.inputs.project_rules_section() {
            sections.push(rules_section);
        }

        // 8. Language preference (optional)
        if let Some(lang_section) = self.inputs.language_section() {
            sections.push(lang_section);
        }

        // 9. Dynamic context (date, memory) — last for maximum cache prefix
        sections.push(build_context_section(self.inputs.memory_context));

        // 10. Extra dynamic sections (runtime-registered)
        for section in &self.inputs.extra_sections {
            if !section.trim().is_empty() {
                sections.push(section.clone());
            }
        }

        sections.join("\n\n")
    }
}

impl<'a> Default for PromptBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_build() {
        let prompt = PromptBuilder::new().build();
        // Should contain all mandatory sections
        assert!(prompt.contains("<role>"));
        assert!(prompt.contains("<thinking_style>"));
        assert!(prompt.contains("<response_style>"));
        assert!(prompt.contains("<context>"));
        assert!(prompt.contains("<critical_reminders>"));
        // Should NOT contain tool section
        assert!(!prompt.contains("<tool_system>"));
    }

    #[test]
    fn test_full_build() {
        let tools = vec![
            ToolInfo {
                name: "search".to_string(),
                description: "Web search".to_string(),
                source: "brave".to_string(),
                usage_hint: None,
            },
        ];
        let prompt = PromptBuilder::new()
            .agent_name("TestBot")
            .tools(&tools)
            .memory_context("User likes Rust")
            .soul("Be friendly and enthusiastic.")
            .build();

        assert!(prompt.contains("TestBot"));
        assert!(prompt.contains("<tool_system>"));
        assert!(prompt.contains("search"));
        assert!(prompt.contains("<memory>"));
        assert!(prompt.contains("User likes Rust"));
        assert!(prompt.contains("<soul>"));
        assert!(prompt.contains("Be friendly"));
    }

    #[test]
    fn test_section_order() {
        let prompt = PromptBuilder::new()
            .soul("Be kind.")
            .build();

        let role_pos = prompt.find("<role>").unwrap();
        let soul_pos = prompt.find("<soul>").unwrap();
        let thinking_pos = prompt.find("<thinking_style>").unwrap();
        let response_pos = prompt.find("<response_style>").unwrap();
        let reminders_pos = prompt.find("<critical_reminders>").unwrap();
        let context_pos = prompt.find("<context>").unwrap();

        // Static prefix: role → soul → thinking → response → reminders
        assert!(role_pos < soul_pos);
        assert!(soul_pos < thinking_pos);
        assert!(thinking_pos < response_pos);
        assert!(response_pos < reminders_pos);
        // Dynamic suffix: context comes after reminders (for cache optimization)
        assert!(reminders_pos < context_pos);
    }

    #[test]
    fn test_empty_soul_is_skipped() {
        let prompt = PromptBuilder::new()
            .soul("   ")
            .build();
        assert!(!prompt.contains("<soul>"));
    }
}
