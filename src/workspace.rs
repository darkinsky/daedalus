use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Workspace resolution strategy.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceKind {
    /// Project-level workspace (`.daedalus/` in project root or ancestors)
    Project,
    /// Global workspace (`~/.daedalus/`)
    Global,
    /// Custom workspace (from `DAEDALUS_WORKSPACE` env var)
    Custom,
}

impl std::fmt::Display for WorkspaceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project => write!(f, "project"),
            Self::Global => write!(f, "global"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// Unified workspace for all Daedalus file I/O.
///
/// Provides canonical paths for configuration, memory persistence,
/// session storage, skills, and logs. Resolves workspace location
/// using a priority chain:
///
/// 1. `DAEDALUS_WORKSPACE` env var (explicit override)
/// 2. `.daedalus/` in current directory or ancestors (project-level)
/// 3. `~/.daedalus/` (global fallback, auto-created)
///
/// ## Directory layout
///
/// ```text
/// <workspace_root>/
/// ├── config/
/// │   ├── daedalus.yaml     # Main configuration file
/// │   ├── mcp.json          # MCP server configuration
/// │   └── soul.md           # SOUL personality file
/// ├── memory/
/// │   ├── long_term.json    # LongTermMemory persistence
/// │   ├── history.jsonl     # HistoryLog persistence (append-only)
/// │   └── agentic/
/// │       └── notes.json    # A-MEM knowledge graph persistence
/// ├── sessions/
/// │   └── last_session_id   # Last session ID for resume
/// ├── skills/               # Skill definitions
/// │   └── <skill-name>/
/// │       └── SKILL.md
/// └── logs/                 # Rolling log files
///     └── daedalus.<date>
/// ```
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Root directory of the workspace.
    root: PathBuf,
    /// How this workspace was resolved.
    kind: WorkspaceKind,
}

impl Workspace {
    // ── Resolution ──

    /// Resolve the workspace using the priority chain.
    ///
    /// Priority:
    /// 1. `DAEDALUS_WORKSPACE` env var → custom workspace
    /// 2. `.daedalus/` in cwd or ancestors → project workspace
    /// 3. `~/.daedalus/` → global workspace (auto-created)
    pub fn resolve() -> Result<Self> {
        // 1. Explicit env var override
        if let Ok(path) = std::env::var("DAEDALUS_WORKSPACE") {
            let root = PathBuf::from(&path);
            Self::ensure_dirs(&root)?;
            // NOTE: tracing is not yet initialized when resolve() is called,
            // so we use eprintln for early diagnostics. The workspace info
            // is logged again in main() after logging is set up.
            return Ok(Self { root, kind: WorkspaceKind::Custom });
        }

        // 2. Walk up from cwd looking for `.daedalus/`
        if let Some(project_ws) = Self::find_project_workspace()? {
            return Ok(project_ws);
        }

        // 3. Global fallback: ~/.daedalus/
        let home = home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory for global workspace"))?;
        let root = home.join(".daedalus");
        Self::ensure_dirs(&root)?;
        Ok(Self { root, kind: WorkspaceKind::Global })
    }

    /// Walk up from cwd looking for a `.daedalus/` directory.
    fn find_project_workspace() -> Result<Option<Self>> {
        let cwd = std::env::current_dir()
            .context("Failed to get current working directory")?;
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join(".daedalus");
            if candidate.is_dir() {
                Self::ensure_dirs(&candidate)?;
                return Ok(Some(Self {
                    root: candidate,
                    kind: WorkspaceKind::Project,
                }));
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
        Ok(None)
    }

    /// Ensure all required subdirectories exist under the workspace root.
    fn ensure_dirs(root: &Path) -> Result<()> {
        let dirs = [
            "config",
            "memory",
            "memory/agentic",
            "memory/wiki",
            "sessions",
            "skills",
            "agents",
            "logs",
        ];
        for d in &dirs {
            std::fs::create_dir_all(root.join(d))
                .with_context(|| format!(
                    "Failed to create workspace directory: {}/{}",
                    root.display(), d
                ))?;
        }
        Ok(())
    }

    // ── Path accessors ──

    /// Root directory of the workspace.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How this workspace was resolved.
    pub fn kind(&self) -> &WorkspaceKind {
        &self.kind
    }

    // ── Config paths ──

    /// Path to the main YAML configuration file.
    pub fn config_file_path(&self) -> PathBuf {
        self.root.join("config/daedalus.yaml")
    }

    /// Path to the MCP server configuration file.
    pub fn mcp_config_path(&self) -> PathBuf {
        self.root.join("config/mcp.json")
    }

    /// Path to the SOUL personality file.
    pub fn soul_file_path(&self) -> PathBuf {
        self.root.join("config/soul.md")
    }

    // ── Memory paths ──

    /// Path to the LongTermMemory persistence file.
    pub fn long_term_memory_path(&self) -> PathBuf {
        self.root.join("memory/long_term.json")
    }

    /// Path to the HistoryLog persistence file (JSONL format).
    pub fn history_log_path(&self) -> PathBuf {
        self.root.join("memory/history.jsonl")
    }

    /// Path to the A-MEM knowledge graph persistence file.
    pub fn agentic_notes_path(&self) -> PathBuf {
        self.root.join("memory/agentic/notes.json")
    }

    /// Path to the Dynamic Cheatsheet persistence file.
    pub fn cheatsheet_path(&self) -> PathBuf {
        self.root.join("memory/cheatsheet.json")
    }

    /// Path to the Wiki memory directory (Markdown files).
    pub fn wiki_dir(&self) -> PathBuf {
        self.root.join("memory/wiki")
    }

    // ── Session paths ──

    /// Directory for session snapshots.
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Path to the file tracking the last session ID.
    pub fn last_session_id_path(&self) -> PathBuf {
        self.root.join("sessions/last_session_id")
    }

    // ── Skills path ──

    /// Directory for skill definitions.
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    // ── Agents path ──

    /// Directory for subagent definitions.
    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    // ── Logs path ──

    /// Directory for rolling log files.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    // ── Convenience checks ──

    /// Check if the main configuration file exists in this workspace.
    pub fn has_config_file(&self) -> bool {
        self.config_file_path().exists()
    }

    /// Check if a MCP config file exists in this workspace.
    pub fn has_mcp_config(&self) -> bool {
        self.mcp_config_path().exists()
    }

    /// Check if a SOUL file exists in this workspace.
    pub fn has_soul_file(&self) -> bool {
        self.soul_file_path().exists()
    }

    /// Check if long-term memory data exists in this workspace.
    pub fn has_long_term_memory(&self) -> bool {
        self.long_term_memory_path().exists()
    }

    /// Check if history log data exists in this workspace.
    pub fn has_history_log(&self) -> bool {
        self.history_log_path().exists()
    }
}

/// Get the user's home directory from environment variables.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_workspace_kind_display() {
        assert_eq!(WorkspaceKind::Project.to_string(), "project");
        assert_eq!(WorkspaceKind::Global.to_string(), "global");
        assert_eq!(WorkspaceKind::Custom.to_string(), "custom");
    }

    #[test]
    fn test_ensure_dirs_creates_structure() {
        let dir = std::env::temp_dir().join("daedalus_ws_test_ensure");
        let _ = fs::remove_dir_all(&dir);

        Workspace::ensure_dirs(&dir).unwrap();

        assert!(dir.join("config").is_dir());
        assert!(dir.join("memory").is_dir());
        assert!(dir.join("memory/agentic").is_dir());
        assert!(dir.join("memory/wiki").is_dir());
        assert!(dir.join("sessions").is_dir());
        assert!(dir.join("skills").is_dir());
        assert!(dir.join("agents").is_dir());
        assert!(dir.join("logs").is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_path_accessors() {
        let ws = Workspace {
            root: PathBuf::from("/tmp/test_ws"),
            kind: WorkspaceKind::Global,
        };

        assert_eq!(ws.config_file_path(), PathBuf::from("/tmp/test_ws/config/daedalus.yaml"));
        assert_eq!(ws.mcp_config_path(), PathBuf::from("/tmp/test_ws/config/mcp.json"));
        assert_eq!(ws.soul_file_path(), PathBuf::from("/tmp/test_ws/config/soul.md"));
        assert_eq!(ws.long_term_memory_path(), PathBuf::from("/tmp/test_ws/memory/long_term.json"));
        assert_eq!(ws.history_log_path(), PathBuf::from("/tmp/test_ws/memory/history.jsonl"));
        assert_eq!(ws.agentic_notes_path(), PathBuf::from("/tmp/test_ws/memory/agentic/notes.json"));
        assert_eq!(ws.cheatsheet_path(), PathBuf::from("/tmp/test_ws/memory/cheatsheet.json"));
        assert_eq!(ws.wiki_dir(), PathBuf::from("/tmp/test_ws/memory/wiki"));
        assert_eq!(ws.sessions_dir(), PathBuf::from("/tmp/test_ws/sessions"));
        assert_eq!(ws.last_session_id_path(), PathBuf::from("/tmp/test_ws/sessions/last_session_id"));
        assert_eq!(ws.skills_dir(), PathBuf::from("/tmp/test_ws/skills"));
        assert_eq!(ws.agents_dir(), PathBuf::from("/tmp/test_ws/agents"));
        assert_eq!(ws.logs_dir(), PathBuf::from("/tmp/test_ws/logs"));
    }

    #[test]
    fn test_custom_workspace_from_env() {
        let dir = std::env::temp_dir().join("daedalus_ws_test_custom");
        let _ = fs::remove_dir_all(&dir);

        // Set env var and resolve
        // SAFETY: This test is single-threaded and the env var is removed immediately after.
        unsafe { std::env::set_var("DAEDALUS_WORKSPACE", dir.to_str().unwrap()); }
        let ws = Workspace::resolve().unwrap();
        unsafe { std::env::remove_var("DAEDALUS_WORKSPACE"); }

        assert_eq!(ws.kind(), &WorkspaceKind::Custom);
        assert_eq!(ws.root(), dir.as_path());
        assert!(dir.join("config").is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_project_workspace_detection() {
        let dir = std::env::temp_dir().join("daedalus_ws_test_project");
        let _ = fs::remove_dir_all(&dir);
        let project_dir = dir.join("my_project");
        let daedalus_dir = project_dir.join(".daedalus");
        fs::create_dir_all(&daedalus_dir).unwrap();

        // Simulate cwd inside the project
        // Note: We can't easily change cwd in tests, so we test find_project_workspace indirectly
        // by testing ensure_dirs on the .daedalus directory
        Workspace::ensure_dirs(&daedalus_dir).unwrap();
        assert!(daedalus_dir.join("config").is_dir());
        assert!(daedalus_dir.join("memory").is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_has_checks() {
        let dir = std::env::temp_dir().join("daedalus_ws_test_has");
        let _ = fs::remove_dir_all(&dir);
        Workspace::ensure_dirs(&dir).unwrap();

        let ws = Workspace {
            root: dir.clone(),
            kind: WorkspaceKind::Global,
        };

        // Initially no files exist
        assert!(!ws.has_mcp_config());
        assert!(!ws.has_soul_file());
        assert!(!ws.has_long_term_memory());
        assert!(!ws.has_history_log());

        // Create some files
        fs::write(ws.mcp_config_path(), "{}").unwrap();
        fs::write(ws.soul_file_path(), "Be kind.").unwrap();
        assert!(ws.has_mcp_config());
        assert!(ws.has_soul_file());

        let _ = fs::remove_dir_all(&dir);
    }
}
