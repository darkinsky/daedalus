/// Persistent long-term memory — key facts extracted across conversations.
///
/// This is the "hot" layer that gets automatically injected into the system
/// prompt on every LLM call. It contains structured knowledge organized
/// into categories.
#[derive(Debug, Clone, Default)]
pub struct LongTermMemory {
    /// User preferences and important facts (e.g., "prefers Rust", "timezone: UTC+8").
    pub user_preferences: Vec<String>,
    /// Project context (e.g., "working on Daedalus CLI agent").
    pub project_context: Vec<String>,
    /// Important decisions made during conversations.
    pub important_decisions: Vec<String>,
    /// Other important notes.
    pub important_notes: Vec<String>,
}

impl LongTermMemory {
    /// Render a single section as Markdown, or `None` if items is empty.
    fn render_section(title: &str, items: &[String]) -> Option<String> {
        if items.is_empty() {
            return None;
        }
        let body: Vec<String> = items.iter().map(|s| format!("- {}", s)).collect();
        Some(format!("### {}\n{}", title, body.join("\n")))
    }

    /// Render long-term memory as a Markdown string for injection into
    /// the system prompt.
    ///
    /// Returns `None` if all sections are empty.
    pub fn to_markdown(&self) -> Option<String> {
        let sections: Vec<String> = [
            Self::render_section("User Preferences", &self.user_preferences),
            Self::render_section("Project Context", &self.project_context),
            Self::render_section("Important Decisions", &self.important_decisions),
            Self::render_section("Important Notes", &self.important_notes),
        ]
        .into_iter()
        .flatten()
        .collect();

        if sections.is_empty() {
            None
        } else {
            Some(format!("## Long-Term Memory\n\n{}", sections.join("\n\n")))
        }
    }

    /// Check if all sections are empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.user_preferences.is_empty()
            && self.project_context.is_empty()
            && self.important_decisions.is_empty()
            && self.important_notes.is_empty()
    }

    /// Replace the entire long-term memory content (used during consolidation).
    #[allow(dead_code)]
    pub fn replace_with(&mut self, other: LongTermMemory) {
        *self = other;
    }
}
