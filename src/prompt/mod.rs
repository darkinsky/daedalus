pub mod sections;

use crate::llm::ToolInfo;

use sections::context::build_context_section;
use sections::reminders::build_reminders_section;
use sections::response_style::build_response_style_section;
use sections::role::build_role_section;
use sections::thinking::build_thinking_section;
use sections::tool_guidance::build_tool_guidance_section;

/// Configuration for building a system prompt.
///
/// Collects all the dynamic inputs needed to assemble a production-grade
/// system prompt. Use `PromptBuilder::default()` and chain setter methods
/// for a fluent API.
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
    /// Custom agent name (defaults to "Daedalus").
    agent_name: Option<&'a str>,
    /// Available MCP tool descriptions.
    tools: &'a [ToolInfo],
    /// Optional long-term memory context to inject.
    memory_context: Option<&'a str>,
    /// Optional custom preamble to prepend (e.g., loaded from SOUL.md).
    /// This is injected right after the role section.
    soul: Option<&'a str>,
}

impl<'a> PromptBuilder<'a> {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            agent_name: None,
            tools: &[],
            memory_context: None,
            soul: None,
        }
    }

    /// Set a custom agent name.
    pub fn agent_name(mut self, name: &'a str) -> Self {
        self.agent_name = Some(name);
        self
    }

    /// Set the available MCP tools.
    pub fn tools(mut self, tools: &'a [ToolInfo]) -> Self {
        self.tools = tools;
        self
    }

    /// Set the long-term memory context to inject.
    ///
    /// Reserved for future use when long-term memory is implemented.
    #[allow(dead_code)]
    pub fn memory_context(mut self, ctx: &'a str) -> Self {
        self.memory_context = Some(ctx);
        self
    }

    /// Set a custom soul/personality preamble (e.g., from SOUL.md).
    pub fn soul(mut self, soul: &'a str) -> Self {
        self.soul = Some(soul);
        self
    }

    /// Assemble the final system prompt from all configured sections.
    ///
    /// The prompt is assembled in a deliberate order:
    /// 1. **Role** — Who am I? What can I do?
    /// 2. **Soul** — Personality and behavioral guardrails (optional)
    /// 3. **Thinking Style** — How should I reason?
    /// 4. **Tool Guidance** — How do I use tools? (only if tools available)
    /// 5. **Response Style** — How should I format output?
    /// 6. **Context** — Dynamic runtime info (date, memory)
    /// 7. **Critical Reminders** — Hard rules that must not be violated
    pub fn build(&self) -> String {
        let has_tools = !self.tools.is_empty();

        let mut sections: Vec<String> = Vec::with_capacity(8);

        // 1. Role definition
        sections.push(build_role_section(self.agent_name, self.tools));

        // 2. Soul / personality (optional)
        if let Some(soul) = self.soul {
            if !soul.trim().is_empty() {
                sections.push(format!("<soul>\n{}\n</soul>", soul.trim()));
            }
        }

        // 3. Thinking style
        sections.push(build_thinking_section(has_tools));

        // 4. Tool guidance (only if tools are available)
        let tool_section = build_tool_guidance_section(self.tools);
        if !tool_section.is_empty() {
            sections.push(tool_section);
        }

        // 5. Response style
        sections.push(build_response_style_section());

        // 6. Dynamic context (date, memory)
        sections.push(build_context_section(self.memory_context));

        // 7. Critical reminders (always last — highest salience)
        sections.push(build_reminders_section(has_tools));

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
                server: "brave".to_string(),
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
        let context_pos = prompt.find("<context>").unwrap();
        let reminders_pos = prompt.find("<critical_reminders>").unwrap();

        assert!(role_pos < soul_pos);
        assert!(soul_pos < thinking_pos);
        assert!(thinking_pos < response_pos);
        assert!(response_pos < context_pos);
        assert!(context_pos < reminders_pos);
    }

    #[test]
    fn test_empty_soul_is_skipped() {
        let prompt = PromptBuilder::new()
            .soul("   ")
            .build();
        assert!(!prompt.contains("<soul>"));
    }
}
