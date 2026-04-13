pub(crate) mod loader;
pub mod registry;

pub use registry::SkillRegistry;

/// Metadata about a skill for display and LLM routing.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    /// Unique skill name (derived from subdirectory name).
    pub name: String,
    /// Human-readable description (extracted from SKILL.md front-matter
    /// or the first non-empty line of the file).
    pub description: String,
}

/// A fully loaded skill definition.
#[derive(Debug, Clone)]
pub struct SkillDefinition {
    /// Unique skill name.
    pub name: String,
    /// Human-readable description for LLM routing.
    pub description: String,
    /// The full prompt/instructions content of the skill.
    /// This gets injected into the conversation when the skill is activated.
    pub instructions: String,
}
