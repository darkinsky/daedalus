/// Persistent long-term memory — key facts extracted across conversations.
///
/// This is the "hot" layer that gets automatically injected into the system
/// prompt on every LLM call. It contains structured knowledge organized
/// into named sections.
///
/// Sections are stored in insertion order and rendered as Markdown headings.
/// Default sections are created on construction, but new sections can be
/// added dynamically (e.g., "code_patterns", "error_history").
#[derive(Debug, Clone)]
pub struct LongTermMemory {
    /// Ordered list of (section_name, items) pairs.
    /// Preserves insertion order for deterministic Markdown rendering.
    sections: Vec<(String, Vec<String>)>,
}

/// Default section names created on construction.
const DEFAULT_SECTIONS: &[&str] = &[
    "User Preferences",
    "Project Context",
    "Important Decisions",
    "Important Notes",
];

impl Default for LongTermMemory {
    fn default() -> Self {
        Self {
            sections: DEFAULT_SECTIONS
                .iter()
                .map(|name| (name.to_string(), Vec::new()))
                .collect(),
        }
    }
}

impl LongTermMemory {
    // ── Dynamic section access ──

    /// Get items in a named section, or empty slice if the section doesn't exist.
    pub fn section(&self, name: &str) -> &[String] {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, items)| items.as_slice())
            .unwrap_or(&[])
    }

    /// Get a mutable reference to a section's items, creating it if needed.
    pub fn section_mut(&mut self, name: &str) -> &mut Vec<String> {
        if let Some(pos) = self.sections.iter().position(|(n, _)| n == name) {
            &mut self.sections[pos].1
        } else {
            self.sections.push((name.to_string(), Vec::new()));
            &mut self.sections.last_mut().expect("just pushed").1
        }
    }

    /// Add an item to a named section, creating the section if needed.
    #[allow(dead_code)]
    pub fn add_to_section(&mut self, section: &str, item: String) {
        self.section_mut(section).push(item);
    }

    /// Return an iterator over all (section_name, items) pairs.
    #[allow(dead_code)]
    pub fn iter_sections(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.sections.iter().map(|(n, items)| (n.as_str(), items.as_slice()))
    }

    // ── Backward-compatible accessors for default sections ──

    /// Get user preferences (shorthand for `section("User Preferences")`).
    #[allow(dead_code)]
    pub fn user_preferences(&self) -> &[String] {
        self.section("User Preferences")
    }

    /// Get mutable user preferences.
    #[allow(dead_code)]
    pub fn user_preferences_mut(&mut self) -> &mut Vec<String> {
        self.section_mut("User Preferences")
    }

    /// Get project context.
    #[allow(dead_code)]
    pub fn project_context(&self) -> &[String] {
        self.section("Project Context")
    }

    /// Get mutable project context.
    #[allow(dead_code)]
    pub fn project_context_mut(&mut self) -> &mut Vec<String> {
        self.section_mut("Project Context")
    }

    /// Get important decisions.
    #[allow(dead_code)]
    pub fn important_decisions(&self) -> &[String] {
        self.section("Important Decisions")
    }

    /// Get important notes.
    #[allow(dead_code)]
    pub fn important_notes(&self) -> &[String] {
        self.section("Important Notes")
    }

    /// Get mutable important notes.
    #[allow(dead_code)]
    pub fn important_notes_mut(&mut self) -> &mut Vec<String> {
        self.section_mut("Important Notes")
    }

    // ── Rendering ──

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
        let sections: Vec<String> = self
            .sections
            .iter()
            .filter_map(|(name, items)| Self::render_section(name, items))
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
        self.sections.iter().all(|(_, items)| items.is_empty())
    }

    /// Replace the entire long-term memory content (used during consolidation).
    #[allow(dead_code)]
    pub fn replace_with(&mut self, other: LongTermMemory) {
        *self = other;
    }
}
