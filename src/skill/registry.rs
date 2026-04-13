use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::loader::SkillLoader;
use super::{SkillDefinition, SkillInfo};
use crate::tools::BuiltinTool;

/// Manages all available skills and provides LLM-routable skill access.
///
/// The registry loads skills from the filesystem and exposes them as a
/// built-in tool (`use_skill`) that the LLM can call to activate a skill.
/// When the LLM calls `use_skill(name)`, the skill's instructions are
/// returned as the tool result, effectively injecting domain-specific
/// knowledge into the conversation context.
///
/// This is the "LLM routing" approach: instead of injecting all skills
/// into the system prompt (which wastes tokens), the LLM decides which
/// skill to use based on the user's request and the skill descriptions
/// provided in the tool definition.
pub struct SkillRegistry {
    /// All loaded skill definitions, keyed by name.
    skills: HashMap<String, SkillDefinition>,
}

impl SkillRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Load skills from a directory and add them to the registry.
    ///
    /// Can be called multiple times to load from multiple directories.
    /// If a skill name conflicts, the later one wins (with a warning).
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<usize> {
        let definitions = SkillLoader::load_from_dir(dir)?;
        let count = definitions.len();

        for def in definitions {
            if self.skills.contains_key(&def.name) {
                tracing::warn!(
                    skill = %def.name,
                    "Skill name conflict — overwriting with new definition"
                );
            }
            self.skills.insert(def.name.clone(), def);
        }

        Ok(count)
    }

    /// Return the number of available skills.
    pub fn skill_count(&self) -> usize {
        self.skills.len()
    }

    /// Return true if any skills are loaded.
    #[allow(dead_code)]
    pub fn has_skills(&self) -> bool {
        !self.skills.is_empty()
    }

    /// Get a skill definition by name.
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&SkillDefinition> {
        self.skills.get(name)
    }

    /// Return metadata for all available skills (for display and tool definitions).
    pub fn skill_infos(&self) -> Vec<SkillInfo> {
        let mut infos: Vec<SkillInfo> = self
            .skills
            .values()
            .map(|def| SkillInfo {
                name: def.name.clone(),
                description: def.description.clone(),
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }

    /// Execute the `use_skill` tool call — return the skill's instructions.
    ///
    /// This is called by the ToolRouter when the LLM invokes `use_skill`.
    /// The skill instructions are returned as the tool result, which the
    /// LLM then uses to guide its response.
    pub fn execute_skill(&self, name: &str) -> Result<String> {
        let skill = self.skills.get(name).ok_or_else(|| {
            let available: Vec<&str> = self.skills.keys().map(|s| s.as_str()).collect();
            anyhow::anyhow!(
                "Skill '{}' not found. Available skills: {}",
                name,
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )
        })?;

        Ok(skill.instructions.clone())
    }

    /// Build a `BuiltinTool` implementation for the `use_skill` tool.
    ///
    /// Returns `None` if no skills are loaded.
    /// The returned tool wraps this registry (via `Arc`) and delegates
    /// execution to `execute_skill`. This allows the `ToolRouter` to
    /// treat skills as regular built-in tools without special-casing.
    pub fn build_skill_tool(self: &Arc<Self>) -> Option<Box<dyn BuiltinTool>> {
        if self.skills.is_empty() {
            return None;
        }
        Some(Box::new(SkillTool {
            registry: Arc::clone(self),
        }))
    }

    /// Build the OpenAI function-calling JSON definition for the `use_skill` tool.
    ///
    /// The tool description includes a list of all available skills and their
    /// descriptions, so the LLM can make an informed routing decision.
    ///
    /// Returns `None` if no skills are loaded (the tool should not be registered).
    pub fn build_tool_definition(&self) -> Option<serde_json::Value> {
        if self.skills.is_empty() {
            return None;
        }

        // Build the skill catalog for the tool description
        let skill_list: Vec<String> = self
            .skill_infos()
            .iter()
            .map(|info| format!("  - **{}**: {}", info.name, info.description))
            .collect();

        let description = format!(
            "Execute a skill within the current conversation. \
             Skills provide specialized capabilities and domain knowledge.\n\n\
             Available skills:\n{}\n\n\
             When the user's request matches a skill's domain, invoke this tool \
             with the skill name. The skill's detailed instructions will be \
             returned, which you should then follow to complete the task.\n\n\
             Important:\n\
             - Only use skills listed above\n\
             - Do not invoke a skill that is already in context\n\
             - The skill's prompt will expand and provide detailed instructions",
            skill_list.join("\n")
        );

        // Build the enum of valid skill names
        let skill_names: Vec<serde_json::Value> = self
            .skills
            .keys()
            .map(|name| serde_json::Value::String(name.clone()))
            .collect();

        Some(serde_json::json!({
            "type": "function",
            "function": {
                "name": "use_skill",
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "The skill name to execute. Must be one of the available skills listed in the tool description.",
                            "enum": skill_names,
                        }
                    },
                    "required": ["name"],
                }
            }
        }))
    }
}

// ── SkillTool: BuiltinTool adapter for SkillRegistry ──

/// The tool name used for LLM-routed skill invocation.
const SKILL_TOOL_NAME: &str = "use_skill";

/// A `BuiltinTool` implementation that wraps the `SkillRegistry`.
///
/// This allows the `ToolRouter` to treat skill invocation as a regular
/// built-in tool call, eliminating the need for special-case routing logic.
/// The LLM sees `use_skill` as just another tool in the tool list.
struct SkillTool {
    registry: Arc<SkillRegistry>,
}

#[async_trait]
impl BuiltinTool for SkillTool {
    fn name(&self) -> &str {
        SKILL_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Execute a skill within the current conversation for specialized tasks"
    }

    fn input_schema(&self) -> serde_json::Value {
        // Build the enum of valid skill names
        let skill_names: Vec<serde_json::Value> = self
            .registry
            .skills
            .keys()
            .map(|name| serde_json::Value::String(name.clone()))
            .collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name to execute. Must be one of the available skills listed in the tool description.",
                    "enum": skill_names,
                }
            },
            "required": ["name"],
        })
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let skill_name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        tracing::info!(
            skill = %skill_name,
            "LLM invoked use_skill — routing to skill registry"
        );

        self.registry.execute_skill(skill_name)
    }

    /// Override the default `to_openai_json` to use the rich description
    /// from `SkillRegistry::build_tool_definition`.
    fn to_openai_json(&self) -> serde_json::Value {
        // Delegate to the registry's existing rich tool definition builder
        self.registry
            .build_tool_definition()
            .unwrap_or_else(|| {
                // Fallback to default format (should not happen since we
                // only create SkillTool when skills are loaded)
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": self.name(),
                        "description": self.description(),
                        "parameters": self.input_schema(),
                    }
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str, desc: &str, instructions: &str) -> SkillDefinition {
        SkillDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            instructions: instructions.to_string(),
        }
    }

    #[test]
    fn test_empty_registry() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.skill_count(), 0);
        assert!(!registry.has_skills());
        assert!(registry.build_tool_definition().is_none());
    }

    #[test]
    fn test_execute_skill() {
        let mut registry = SkillRegistry::new();
        registry.skills.insert(
            "code-review".to_string(),
            make_skill("code-review", "Code reviewer", "Review code carefully."),
        );

        let result = registry.execute_skill("code-review").unwrap();
        assert_eq!(result, "Review code carefully.");
    }

    #[test]
    fn test_execute_unknown_skill() {
        let registry = SkillRegistry::new();
        let result = registry.execute_skill("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_build_tool_definition() {
        let mut registry = SkillRegistry::new();
        registry.skills.insert(
            "code-review".to_string(),
            make_skill("code-review", "Expert code reviewer", "Review code."),
        );

        let tool_def = registry.build_tool_definition().unwrap();
        let func = &tool_def["function"];
        assert_eq!(func["name"], "use_skill");
        assert!(func["description"].as_str().unwrap().contains("code-review"));
        assert!(func["description"].as_str().unwrap().contains("Expert code reviewer"));

        // Check enum constraint
        let enum_values = &func["parameters"]["properties"]["name"]["enum"];
        assert!(enum_values.as_array().unwrap().contains(&serde_json::json!("code-review")));
    }

    #[test]
    fn test_skill_infos_sorted() {
        let mut registry = SkillRegistry::new();
        registry.skills.insert(
            "zebra".to_string(),
            make_skill("zebra", "Z skill", "Z instructions"),
        );
        registry.skills.insert(
            "alpha".to_string(),
            make_skill("alpha", "A skill", "A instructions"),
        );

        let infos = registry.skill_infos();
        assert_eq!(infos[0].name, "alpha");
        assert_eq!(infos[1].name, "zebra");
    }
}
