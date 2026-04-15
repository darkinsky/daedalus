use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use super::loader::SubagentLoader;
use super::{SubagentDefinition, SubagentInfo, SubagentSource};

/// Manages all available subagent definitions and provides LLM-routable access.
///
/// The registry loads subagent definitions from the filesystem (project-level
/// and global directories) and exposes them as a built-in tool
/// (`spawn_subagent`) that the LLM can call to delegate tasks.
///
/// When names conflict between sources, higher-priority sources win:
/// Project (.daedalus/agents/) > Global (~/.daedalus/agents/).
pub struct SubagentRegistry {
    /// All loaded subagent definitions, keyed by name.
    agents: HashMap<String, SubagentDefinition>,
}

impl SubagentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Register built-in subagent definitions.
    ///
    /// Built-in agents have the lowest priority (`SubagentSource::Builtin`).
    /// They will be overridden by any user-defined agent with the same name
    /// loaded from project or global directories.
    ///
    /// This should be called **before** `load_from_dir()` so that
    /// user definitions take precedence.
    pub fn register_builtins(&mut self) -> usize {
        let builtins = super::builtins::builtin_agents();
        let count = builtins.len();

        for def in builtins {
            self.agents.insert(def.name.clone(), def);
        }

        tracing::info!(
            builtins = count,
            "Built-in subagent definitions registered"
        );

        count
    }

    /// Load subagent definitions from a directory and add them to the registry.
    ///
    /// Can be called multiple times to load from multiple directories.
    /// If a name conflicts, the later one wins (with a warning).
    pub fn load_from_dir(&mut self, dir: &Path, source: SubagentSource) -> Result<usize> {
        let definitions = SubagentLoader::load_from_dir(dir, source)?;
        let count = definitions.len();

        for def in definitions {
            if let Some(existing) = self.agents.get(&def.name) {
                tracing::warn!(
                    agent = %def.name,
                    old_source = %existing.source,
                    new_source = %def.source,
                    "Subagent name conflict — overwriting with new definition"
                );
            }
            self.agents.insert(def.name.clone(), def);
        }

        Ok(count)
    }

    /// Return the number of available subagents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Return true if any subagents are loaded.
    #[allow(dead_code)]
    pub fn has_agents(&self) -> bool {
        !self.agents.is_empty()
    }

    /// Get a subagent definition by name.
    pub fn get(&self, name: &str) -> Option<&SubagentDefinition> {
        self.agents.get(name)
    }

    /// Return metadata for all available subagents (sorted by name).
    pub fn agent_infos(&self) -> Vec<SubagentInfo> {
        let mut infos: Vec<SubagentInfo> = self
            .agents
            .values()
            .map(|def| SubagentInfo {
                name: def.name.clone(),
                description: def.description.clone(),
                source: def.source.clone(),
            })
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::PermissionMode;

    fn make_agent(name: &str, desc: &str, prompt: &str, source: SubagentSource) -> SubagentDefinition {
        SubagentDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            system_prompt: prompt.to_string(),
            model: None,
            tools: None,
            disallowed_tools: None,
            permission_mode: PermissionMode::Default,
            max_turns: None,
            source,
            isolation: crate::subagent::IsolationMode::default(),
            on_start: None,
            on_complete: None,
        }
    }

    #[test]
    fn test_empty_registry() {
        let registry = SubagentRegistry::new();
        assert_eq!(registry.agent_count(), 0);
        assert!(!registry.has_agents());
        // build_tool_definition is now in crate::subagent::tool
        assert!(crate::subagent::tool::build_tool_definition(&registry).is_none());
    }

    #[test]
    fn test_get_agent() {
        let mut registry = SubagentRegistry::new();
        registry.agents.insert(
            "code-reviewer".to_string(),
            make_agent("code-reviewer", "Reviews code", "Review carefully.", SubagentSource::Project),
        );

        assert!(registry.get("code-reviewer").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_agent_infos_sorted() {
        let mut registry = SubagentRegistry::new();
        registry.agents.insert(
            "zebra".to_string(),
            make_agent("zebra", "Z agent", "Z prompt", SubagentSource::Global),
        );
        registry.agents.insert(
            "alpha".to_string(),
            make_agent("alpha", "A agent", "A prompt", SubagentSource::Project),
        );

        let infos = registry.agent_infos();
        assert_eq!(infos[0].name, "alpha");
        assert_eq!(infos[1].name, "zebra");
    }

    #[test]
    fn test_name_conflict_overwrites() {
        let mut registry = SubagentRegistry::new();
        registry.agents.insert(
            "test".to_string(),
            make_agent("test", "Old description", "Old prompt", SubagentSource::Global),
        );
        registry.agents.insert(
            "test".to_string(),
            make_agent("test", "New description", "New prompt", SubagentSource::Project),
        );

        let agent = registry.get("test").unwrap();
        assert_eq!(agent.description, "New description");
        assert_eq!(agent.source, SubagentSource::Project);
    }
}
