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
    /// Optional language preference (e.g., "Chinese", "English", "Japanese").
    /// When set, overrides the generic "respond in the user's language" rule
    /// with a specific language directive.
    pub language_preference: Option<&'a str>,
    /// Extra dynamic sections injected at runtime.
    ///
    /// This enables P2-8 (dynamic section registration) without a full trait system.
    /// Each entry is a pre-formatted section string (with XML tags if desired).
    /// Sections are appended to the dynamic suffix in order.
    pub extra_sections: Vec<String>,
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
            language_preference: None,
            extra_sections: Vec::new(),
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

    /// Build the language preference section (shared between both styles).
    ///
    /// Returns `None` if no language preference is configured.
    /// When present, this provides a specific language directive that is more
    /// precise than the generic "respond in the user's language" rule in reminders.
    pub fn language_section(&self) -> Option<String> {
        self.language_preference
            .filter(|l| !l.trim().is_empty())
            .map(|lang| format!(
                "<language>\n\
                 Always respond in {lang}. All explanations, comments, and communication \
                 with the user should be in {lang}. Technical terms and code identifiers \
                 should remain in their original language.\n\
                 </language>",
                lang = lang.trim()
            ))
    }
}

impl<'a> Default for PromptInputs<'a> {
    fn default() -> Self {
        Self::new()
    }
}
