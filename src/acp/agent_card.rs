//! Agent Card — capability descriptors for agent discovery and routing.
//!
//! An `AgentCard` is the ACP equivalent of a service descriptor or API manifest.
//! It declares what an agent can do, what inputs it accepts, and how to reach it.
//!
//! ## Usage
//!
//! - **Discovery**: Clients query agent cards to find agents that match a task.
//! - **Routing**: The ACP client uses cards to select the best agent for a request.
//! - **Documentation**: Cards serve as machine-readable agent documentation.
//!
//! ## Relationship to SubagentDefinition
//!
//! `AgentCard` is the ACP-level abstraction; `SubagentDefinition` is the
//! Daedalus-internal representation. A `SubagentDefinition` can be converted
//! to an `AgentCard` via the `From` impl, bridging the existing subagent
//! system with the new protocol layer.

use serde::{Deserialize, Serialize};

/// An agent's capability card — the public-facing description of an agent.
///
/// This is the ACP equivalent of Google A2A's "Agent Card". It contains
/// everything a client needs to know to decide whether and how to interact
/// with this agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// Unique agent name (lowercase + hyphens, e.g., "code-reviewer").
    pub name: String,
    /// Human-readable description of the agent's purpose and capabilities.
    pub description: String,
    /// Version of the agent (semver or free-form).
    #[serde(default = "default_version")]
    pub version: String,
    /// The skills/capabilities this agent offers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<AgentSkill>,
    /// Agent capabilities and constraints.
    #[serde(default)]
    pub capabilities: AgentCapabilities,
    /// Optional URL for remote agents (Phase 2: HTTP transport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional provider/author of this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional tags for categorization and search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

impl AgentCard {
    /// Create a minimal agent card with just a name and description.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: default_version(),
            skills: vec![],
            capabilities: AgentCapabilities::default(),
            url: None,
            provider: None,
            tags: vec![],
        }
    }

    /// Add a skill to this agent card.
    pub fn with_skill(mut self, skill: AgentSkill) -> Self {
        self.skills.push(skill);
        self
    }

    /// Set the capabilities for this agent card.
    pub fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set the URL for remote access (Phase 2).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the provider/author.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Add tags for categorization.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Check if this agent has a specific skill by name.
    pub fn has_skill(&self, skill_name: &str) -> bool {
        self.skills.iter().any(|s| s.name == skill_name)
    }

    /// Check if this agent is a remote agent (has a URL).
    pub fn is_remote(&self) -> bool {
        self.url.is_some()
    }
}

/// A specific skill/capability offered by an agent.
///
/// Skills are the atomic units of agent capability. Each skill describes
/// one thing the agent can do, with optional input/output schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    /// Unique skill name within this agent.
    pub name: String,
    /// Human-readable description of what this skill does.
    pub description: String,
    /// Optional JSON Schema for the skill's input parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Optional list of output MIME types this skill can produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_types: Vec<String>,
    /// Optional examples of how to invoke this skill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

impl AgentSkill {
    /// Create a new skill with a name and description.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: None,
            output_types: vec![],
            examples: vec![],
        }
    }

    /// Attach an input schema to this skill.
    pub fn with_input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// Specify output MIME types.
    pub fn with_output_types(mut self, types: Vec<String>) -> Self {
        self.output_types = types;
        self
    }
}

/// Agent capabilities and constraints.
///
/// Declares what the agent can and cannot do, helping clients make
/// informed routing decisions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Whether this agent supports streaming events during task processing.
    #[serde(default)]
    pub streaming: bool,
    /// Whether this agent can use tools (file I/O, bash, etc.).
    #[serde(default)]
    pub tool_use: bool,
    /// Whether this agent supports task cancellation.
    #[serde(default)]
    pub cancellation: bool,
    /// Maximum concurrent tasks this agent can handle (0 = unlimited).
    #[serde(default)]
    pub max_concurrency: usize,
    /// Optional list of supported input MIME types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_types: Vec<String>,
    /// Optional list of supported output MIME types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_types: Vec<String>,
}

// ── Conversion from SubagentDefinition ──

impl From<&crate::subagent::SubagentDefinition> for AgentCard {
    /// Convert a Daedalus `SubagentDefinition` into an ACP `AgentCard`.
    ///
    /// This bridges the existing subagent system with the ACP protocol,
    /// allowing subagents to participate in ACP discovery and routing
    /// without any changes to their definition files.
    fn from(def: &crate::subagent::SubagentDefinition) -> Self {
        let has_tools = def.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(true)
            && def.disallowed_tools.as_ref().map(|d| d.len() < 9).unwrap_or(true);

        AgentCard {
            name: def.name.clone(),
            description: def.description.clone(),
            version: "1.0.0".to_string(),
            skills: vec![
                AgentSkill::new(
                    "task_execution",
                    &def.description,
                ),
            ],
            capabilities: AgentCapabilities {
                streaming: false, // Phase 1: no streaming
                tool_use: has_tools,
                cancellation: false, // Phase 1: no cancellation
                max_concurrency: 1,
                input_types: vec!["text/plain".to_string()],
                output_types: vec!["text/plain".to_string()],
            },
            url: None, // Local agent
            provider: Some("daedalus".to_string()),
            tags: vec![format!("source:{}", def.source)],
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_card_creation() {
        let card = AgentCard::new("code-reviewer", "Reviews code for quality and correctness")
            .with_skill(AgentSkill::new("review", "Review a code file"))
            .with_capabilities(AgentCapabilities {
                tool_use: true,
                streaming: false,
                ..Default::default()
            })
            .with_tags(vec!["code".to_string(), "review".to_string()]);

        assert_eq!(card.name, "code-reviewer");
        assert!(card.has_skill("review"));
        assert!(!card.has_skill("deploy"));
        assert!(card.capabilities.tool_use);
        assert!(!card.is_remote());
        assert_eq!(card.tags.len(), 2);
    }

    #[test]
    fn test_agent_card_remote() {
        let card = AgentCard::new("remote-agent", "A remote agent")
            .with_url("https://agent.example.com/acp");
        assert!(card.is_remote());
        assert_eq!(card.url, Some("https://agent.example.com/acp".to_string()));
    }

    #[test]
    fn test_agent_skill_with_schema() {
        let skill = AgentSkill::new("analyze", "Analyze code")
            .with_input_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                }
            }))
            .with_output_types(vec!["text/plain".to_string(), "application/json".to_string()]);

        assert!(skill.input_schema.is_some());
        assert_eq!(skill.output_types.len(), 2);
    }

    #[test]
    fn test_agent_card_serialization() {
        let card = AgentCard::new("test-agent", "A test agent");
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("test-agent"));

        let deserialized: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "test-agent");
    }

    #[test]
    fn test_from_subagent_definition() {
        let def = crate::subagent::SubagentDefinition {
            name: "explore".to_string(),
            description: "Read-only code exploration agent".to_string(),
            system_prompt: "You are an explorer.".to_string(),
            model: None,
            tools: Some(vec!["read_file".to_string(), "grep_search".to_string()]),
            disallowed_tools: None,
            permission_mode: crate::subagent::PermissionMode::Plan,
            max_turns: Some(10),
            source: crate::subagent::SubagentSource::Builtin,
            isolation: crate::subagent::IsolationMode::None,
            on_start: None,
            on_complete: None,
            shared_context: None,
            context_budget_tokens: None,
        };

        let card = AgentCard::from(&def);
        assert_eq!(card.name, "explore");
        assert_eq!(card.description, "Read-only code exploration agent");
        assert!(card.capabilities.tool_use);
        assert!(!card.is_remote());
        assert_eq!(card.provider, Some("daedalus".to_string()));
    }
}
