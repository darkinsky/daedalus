use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::embedding::Embedding;
use crate::llm::LlmConfig;
use crate::prompt::PromptStyle;
use crate::workspace::{self, Workspace};

// ── Shared constants ──

/// The built-in default system prompt.
///
/// This constant is the single source of truth for the default prompt,
/// used to detect custom overrides.
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are Daedalus, a helpful AI assistant. \
     Be concise and accurate in your responses.";

// ── Memory strategy selection ──

/// Available memory strategies (mutually exclusive).
///
/// Users select one strategy via `memory.strategy` in the YAML config.
/// Each strategy has its own strengths:
///
/// - **SlidingWindow** (default): Dual-layer memory with hot/cold data,
///   consolidation, and optional cheatsheet. Best for general use.
/// - **DynamicCheatsheet**: Lightweight adaptive memory that accumulates
///   problem-solving insights via LLM reflection. Best for repetitive
///   task patterns.
/// - **Agentic**: Knowledge graph memory (A-MEM) with embedding-based
///   retrieval and memory evolution. Best for long-term knowledge
///   accumulation across sessions.
/// - **Wiki**: LLM Wiki memory (Karpathy pattern) with structured
///   Markdown pages, wikilinks, and periodic lint. Best for deep
///   knowledge compilation into an Obsidian-compatible wiki.
/// - **Ace**: ACE (Agentic Context Engineering) with evolving playbook,
///   structured sections/bullets, and deterministic curation. Best for
///   strategy accumulation and self-improving context.
/// - **MemPalace**: Memory Palace (Method of Loci) with spatial organization
///   into Wings/Rooms/Halls, ChromaDB vector storage, knowledge graph,
///   and cross-wing tunnels. Requires embedding config and ChromaDB.
///   Best for cross-project/cross-person long-term memory navigation.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStrategy {
    /// Sliding window with dual-layer consolidation (default).
    #[default]
    SlidingWindow,
    /// Dynamic Cheatsheet — adaptive insight accumulation.
    DynamicCheatsheet,
    /// Agentic Memory (A-MEM) — knowledge graph with embedding retrieval.
    Agentic,
    /// LLM Wiki — structured knowledge compilation with Markdown persistence.
    Wiki,
    /// ACE (Agentic Context Engineering) — evolving playbook with deterministic curation.
    Ace,
    /// Memory Palace — spatial memory with ChromaDB, knowledge graph, and tunnels.
    MemPalace,
}

impl std::fmt::Display for MemoryStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlidingWindow => write!(f, "sliding_window"),
            Self::DynamicCheatsheet => write!(f, "dynamic_cheatsheet"),
            Self::Agentic => write!(f, "agentic"),
            Self::Wiki => write!(f, "wiki"),
            Self::Ace => write!(f, "ace"),
            Self::MemPalace => write!(f, "mempalace"),
        }
    }
}

// ── Embedding provider configuration ──

/// Embedding provider configuration for strategies that need vector search.
///
/// This is a **top-level** config section (`embedding:` in YAML), separate
/// from memory config, because embedding providers may be shared across
/// multiple features in the future.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// API key for the embedding provider.
    /// Falls back to `OPENAI_API_KEY` env var if not set.
    pub api_key: Option<String>,
    /// Base URL for the embedding API.
    /// Falls back to `OPENAI_BASE_URL` env var, then "https://api.openai.com/v1".
    pub api_base: Option<String>,
    /// Embedding model name (e.g., "text-embedding-3-small").
    /// Falls back to `DAEDALUS_EMBEDDING_MODEL` env var, then "text-embedding-3-small".
    pub model: Option<String>,
    /// Embedding vector dimensions.
    /// Falls back to `DAEDALUS_EMBEDDING_DIMENSIONS` env var, then 1536.
    pub dimensions: Option<usize>,
}

impl EmbeddingConfig {
    /// Create an embedding provider from this configuration.
    ///
    /// Resolves each field with fallback to environment variables:
    /// - `api_key` → `OPENAI_API_KEY`
    /// - `api_base` → `OPENAI_BASE_URL` → `"https://api.openai.com/v1"`
    /// - `model` → `DAEDALUS_EMBEDDING_MODEL` → `"text-embedding-3-small"`
    /// - `dimensions` → `DAEDALUS_EMBEDDING_DIMENSIONS` → `1536`
    pub fn create_provider(&self) -> Result<Arc<dyn Embedding>> {
        use crate::embedding::OpenAiEmbedding;

        let api_key = self.api_key.clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| anyhow::anyhow!(
                "Embedding provider requires an API key. \
                 Set `embedding.api_key` in config or OPENAI_API_KEY env var."
            ))?;

        let api_base = self.api_base.clone()
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let model = self.model.clone()
            .or_else(|| std::env::var("DAEDALUS_EMBEDDING_MODEL").ok())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

        let dimensions = self.dimensions
            .or_else(|| {
                std::env::var("DAEDALUS_EMBEDDING_DIMENSIONS")
                    .ok()
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(1536);

        let embedder = OpenAiEmbedding::new(&api_key, &api_base, &model, dimensions);
        Ok(Arc::new(embedder))
    }
}

// ── YAML section structures ──

/// Memory section in the YAML config file.
///
/// Includes strategy selection and per-strategy configuration sub-sections.
/// Each sub-section uses `#[serde(default)]` so unconfigured strategies
/// fall back to their built-in defaults.
///
/// ```yaml
/// memory:
///   strategy: mempalace
///   mempalace:
///     chroma_url: "http://chroma.prod:8000"
///   sliding_window:
///     consolidation_threshold: 200
/// ```
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct MemorySection {
    /// Which memory strategy to use.
    pub strategy: MemoryStrategy,
    /// Sliding window specific configuration.
    pub sliding_window: crate::memory::sliding_window::config::SlidingWindowConfig,
    /// ACE (Agentic Context Engineering) specific configuration.
    pub ace: crate::memory::ace::config::AceConfig,
    /// Dynamic Cheatsheet specific configuration.
    pub dynamic_cheatsheet: crate::memory::dynamic_cheatsheet::config::CheatsheetConfig,
    /// Memory Palace specific configuration.
    pub mempalace: crate::memory::mempalace::config::MemPalaceConfig,
}

/// Agent section in the YAML config file.
///
/// This type is `pub(super)` so the unified loader can deserialize it
/// and pass it to `AgentConfig::build()`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub(super) struct AgentSection {
    /// Custom agent name (defaults to "Daedalus").
    name: Option<String>,
    /// Custom system prompt (overrides PromptBuilder when set).
    system_prompt: Option<String>,
    /// Path to SOUL.md personality file.
    soul_file: Option<String>,
    /// Prompt assembly style: "default" or "coding".
    prompt_style: PromptStyle,
}

// ── Agent configuration ──

/// Agent configuration loaded from a YAML config file.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// LLM provider configuration (api_key, model, api_base, adapter_kind).
    pub llm: LlmConfig,
    /// System prompt for the agent (legacy, used as fallback when prompt builder is bypassed).
    pub system_prompt: String,
    /// Whether the user explicitly set a custom system prompt.
    ///
    /// When `true`, the custom prompt takes priority over the PromptBuilder.
    pub is_custom_prompt: bool,
    /// Custom agent name (defaults to "Daedalus").
    pub agent_name: Option<String>,
    /// Loaded soul content (read from SOUL.md file at startup).
    pub soul: Option<String>,
    /// Loaded project rules content (read from DAEDALUS.md files at startup).
    /// Merged from multiple locations: CWD > workspace > global.
    pub project_rules: Option<String>,
    /// Prompt assembly style (default vs coding).
    pub prompt_style: PromptStyle,
    /// Selected memory strategy.
    pub memory_strategy: MemoryStrategy,
    /// Full memory configuration (strategy + per-strategy sub-configs).
    /// The sub-configs are available for factory initialization.
    #[allow(dead_code)]
    pub memory_config: MemorySection,
    /// Embedding provider configuration (used by agentic memory).
    pub embedding: EmbeddingConfig,
}

impl AgentConfig {
    /// Build AgentConfig from pre-parsed YAML sections.
    ///
    /// Called by the unified loader (`config::loader`) after parsing the
    /// YAML file once. This method handles soul file resolution and
    /// custom prompt detection.
    pub(super) fn build(
        llm: LlmConfig,
        agent: AgentSection,
        memory: MemorySection,
        embedding: EmbeddingConfig,
        workspace: Option<&Workspace>,
    ) -> Self {
        // Detect whether the user explicitly set a custom system prompt
        let (system_prompt, is_custom_prompt) = match &agent.system_prompt {
            Some(custom) if !custom.is_empty() && custom != DEFAULT_SYSTEM_PROMPT => {
                (custom.clone(), true)
            }
            _ => (DEFAULT_SYSTEM_PROMPT.to_string(), false),
        };

        let agent_name = agent.name;

        // Load soul content
        let soul = Self::load_soul(agent.soul_file.as_deref(), workspace);

        // Load project rules from DAEDALUS.md files (multi-level merge)
        let project_rules = Self::load_project_rules(workspace);

        Self {
            llm,
            system_prompt,
            is_custom_prompt,
            agent_name,
            soul,
            project_rules,
            prompt_style: agent.prompt_style,
            memory_strategy: memory.strategy.clone(),
            memory_config: memory,
            embedding,
        }
    }

    /// Load configuration from a specific YAML file path (standalone, for testing).
    #[allow(dead_code)]
    pub fn from_file(path: &str) -> Result<Self> {
        /// Standalone YAML structure for `from_file()`.
        #[derive(Debug, Clone, Default, serde::Deserialize)]
        #[serde(default)]
        struct StandaloneConfig {
            llm: LlmConfig,
            agent: AgentSection,
            memory: MemorySection,
            embedding: EmbeddingConfig,
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path))?;
        let file_config: StandaloneConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path))?;
        Ok(Self::build(file_config.llm, file_config.agent, file_config.memory, file_config.embedding, None))
    }

    /// Load soul content from the configured path or workspace fallback.
    ///
    /// Priority:
    /// 1. Explicit `soul_file` path from config
    /// 2. Workspace `config/soul.md` (if workspace is provided)
    fn load_soul(
        soul_file: Option<&str>,
        workspace: Option<&Workspace>,
    ) -> Option<String> {
        // 1. Try explicit soul_file path
        if let Some(path) = soul_file {
            if let Some(content) = Self::read_trimmed_file(path) {
                tracing::info!(path = %path, "Loaded SOUL personality file");
                return Some(content);
            }
            if std::path::Path::new(path).exists() {
                tracing::warn!(path = %path, "SOUL file is empty, skipping");
            } else {
                tracing::warn!(path = %path, "Failed to load SOUL file, skipping");
            }
        }

        // 2. Try workspace soul file
        if let Some(ws) = workspace {
            if ws.has_soul_file() {
                let path = ws.soul_file_path();
                if let Some(content) = Self::read_trimmed_file(&path.to_string_lossy()) {
                    tracing::info!(path = %path.display(), "Loaded SOUL file from workspace");
                    return Some(content);
                }
            }
        }

        None
    }

    /// Load project rules from DAEDALUS.md files.
    ///
    /// Searches multiple locations and merges all found content (higher
    /// priority first). This mirrors Claude Code's CLAUDE.md convention:
    ///
    /// 1. `CWD/DAEDALUS.md` — project root (highest priority)
    /// 2. `<workspace>/DAEDALUS.md` — workspace level
    /// 3. `~/.daedalus/DAEDALUS.md` — global level (lowest priority)
    ///
    /// All found files are concatenated with section headers. If no files
    /// are found, returns `None`.
    fn load_project_rules(workspace: Option<&Workspace>) -> Option<String> {
        let mut sections: Vec<String> = Vec::new();

        // 1. CWD/DAEDALUS.md (project root)
        if let Ok(cwd) = std::env::current_dir() {
            let cwd_path = cwd.join("DAEDALUS.md");
            if let Some(content) = Self::read_trimmed_file(&cwd_path.to_string_lossy()) {
                tracing::info!(path = %cwd_path.display(), "Loaded project-level DAEDALUS.md");
                sections.push(content);
            }
        }

        // 2. <workspace>/DAEDALUS.md
        if let Some(ws) = workspace {
            let ws_path = ws.root().join("DAEDALUS.md");
            // Avoid loading the same file twice if workspace root == cwd/.daedalus
            let already_loaded = std::env::current_dir().ok().map_or(false, |cwd| {
                let cwd_path = cwd.join("DAEDALUS.md");
                same_file(&cwd_path, &ws_path)
            });
            if !already_loaded {
                if let Some(content) = Self::read_trimmed_file(&ws_path.to_string_lossy()) {
                    tracing::info!(path = %ws_path.display(), "Loaded workspace-level DAEDALUS.md");
                    sections.push(content);
                }
            }
        }

        // 3. ~/.daedalus/DAEDALUS.md (global)
        if let Some(home) = workspace::home_dir() {
            let global_path = home.join(".daedalus/DAEDALUS.md");
            // Avoid loading the same file twice if workspace is the global dir
            let already_loaded = workspace.map_or(false, |ws| {
                same_file(&ws.root().join("DAEDALUS.md"), &global_path)
            });
            if !already_loaded {
                if let Some(content) = Self::read_trimmed_file(&global_path.to_string_lossy()) {
                    tracing::info!(path = %global_path.display(), "Loaded global DAEDALUS.md");
                    sections.push(content);
                }
            }
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

    /// Read a file and return its trimmed content, or `None` if the file
    /// doesn't exist, can't be read, or is empty after trimming.
    fn read_trimmed_file(path: &str) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let trimmed = content.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    }

    /// Convenience accessor for the model name.
    pub fn model(&self) -> &str {
        &self.llm.model
    }

    /// Convenience accessor for the API base URL.
    pub fn api_base(&self) -> Option<&str> {
        self.llm.api_base.as_deref()
    }
}

// ── Module-level helpers ──

/// Check if two paths refer to the same file (by canonical path comparison).
///
/// Returns `false` if either path doesn't exist or can't be canonicalized.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}
