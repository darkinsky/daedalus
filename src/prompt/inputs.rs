//! Shared prompt builder fields and methods.
//!
//! Both `PromptBuilder` (default style) and `CodingPromptBuilder` (coding style)
//! share the same set of input fields (agent_name, tools, soul, project_rules,
//! memory_context). This module provides a shared struct and setter methods
//! to eliminate the duplication.

use crate::tools::ToolInfo;

/// Shared input fields for all prompt builder styles.
///
/// Both `PromptBuilder` and `CodingPromptBuilder` compose this struct
/// instead of duplicating the same 5 fields and their setter methods.
pub struct PromptInputs<'a> {
    /// Custom agent name (defaults to "Daedalus").
    pub agent_name: Option<&'a str>,
    /// Available MCP tool descriptions.
    pub tools: &'a [ToolInfo],
    /// Optional long-term memory context to inject.
    pub memory_context: Option<&'a str>,
    /// Optional custom preamble to prepend (e.g., loaded from SOUL.md).
    pub soul: Option<&'a str>,
    /// Optional project rules (loaded from DAEDALUS.md files).
    pub project_rules: Option<&'a str>,
}

impl<'a> PromptInputs<'a> {
    /// Create new inputs with default settings.
    pub fn new() -> Self {
        Self {
            agent_name: None,
            tools: &[],
            memory_context: None,
            soul: None,
            project_rules: None,
        }
    }

    /// Whether any tools are available.
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    /// Build the project rules section (shared between both styles).
    ///
    /// Returns `None` if no project rules are configured.
    pub fn project_rules_section(&self) -> Option<String> {
        self.project_rules
            .filter(|r| !r.trim().is_empty())
            .map(|rules| format!(
                "<project_rules>\n\
                 The following rules are specific to this project. Follow them strictly.\n\n\
                 {}\n\
                 </project_rules>",
                rules.trim()
            ))
    }

    /// Build the memory context section (shared between both styles).
    ///
    /// Returns `None` if no memory context is configured.
    pub fn memory_section(&self) -> Option<String> {
        self.memory_context
            .filter(|c| !c.trim().is_empty())
            .map(|ctx| format!(
                "<memory>\n\
                 The following is what you remember from previous conversations. \
                 Use this to maintain continuity, but do not mention it unless relevant.\n\n\
                 {}\n\
                 </memory>",
                ctx.trim()
            ))
    }
}

impl<'a> Default for PromptInputs<'a> {
    fn default() -> Self {
        Self::new()
    }
}
