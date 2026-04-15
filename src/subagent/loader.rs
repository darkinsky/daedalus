use std::path::Path;

use anyhow::{Context, Result};

use super::{IsolationMode, PermissionMode, SubagentDefinition, SubagentSource};

/// Loads subagent definitions from the filesystem.
///
/// Subagent definitions are Markdown files with YAML frontmatter,
/// placed directly in the `agents/` directory (no subdirectories needed).
///
/// ## Directory structure
///
/// ```text
/// agents/
/// ├── code-reviewer.md
/// ├── explore.md
/// └── safe-researcher.md
/// ```
///
/// ## File format
///
/// ```markdown
/// ---
/// name: code-reviewer
/// description: Reviews code for quality and best practices.
/// tools: read_file, list_directory, search_files
/// model: sonnet
/// permissionMode: default
/// maxTurns: 10
/// ---
///
/// You are a senior code reviewer.
/// Analyze code and provide actionable feedback.
/// ```
pub struct SubagentLoader;

/// Intermediate struct for YAML frontmatter parsing.
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
    tools: Option<Vec<String>>,
    disallowed_tools: Option<Vec<String>>,
    permission_mode: Option<String>,
    max_turns: Option<usize>,
    isolation: Option<String>,
    on_start: Option<String>,
    on_complete: Option<String>,
}

impl SubagentLoader {
    /// Load all subagent definitions from a directory.
    ///
    /// Scans for `.md` files directly in the directory.
    /// Files that fail to parse are logged as warnings and skipped.
    pub fn load_from_dir(dir: &Path, source: SubagentSource) -> Result<Vec<SubagentDefinition>> {
        if !dir.exists() {
            tracing::debug!(
                path = %dir.display(),
                "Subagent directory does not exist, skipping"
            );
            return Ok(Vec::new());
        }

        if !dir.is_dir() {
            tracing::warn!(
                path = %dir.display(),
                "Subagent path is not a directory, skipping"
            );
            return Ok(Vec::new());
        }

        let mut agents = Vec::new();
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read subagent directory: {}", dir.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .map(|ext| ext == "md")
                        .unwrap_or(false)
            })
            .collect();

        // Sort for deterministic loading order
        entries.sort();

        for path in entries {
            match Self::load_agent(&path, source.clone()) {
                Ok(agent) => {
                    tracing::info!(
                        agent = %agent.name,
                        description_len = agent.description.len(),
                        prompt_len = agent.system_prompt.len(),
                        source = %agent.source,
                        "Loaded subagent definition"
                    );
                    agents.push(agent);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to load subagent definition, skipping"
                    );
                }
            }
        }

        Ok(agents)
    }

    /// Load a single subagent definition from a `.md` file.
    fn load_agent(path: &Path, source: SubagentSource) -> Result<SubagentDefinition> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read subagent file: {}", path.display()))?;

        let trimmed = content.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Subagent file is empty: {}", path.display());
        }

        // Derive fallback name from filename (without extension)
        let fallback_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse YAML frontmatter
        if trimmed.starts_with("---") {
            if let Some((front_matter, body)) = Self::parse_front_matter(trimmed) {
                let name = front_matter.name.unwrap_or(fallback_name);
                let description = front_matter.description.unwrap_or_else(|| {
                    // Fallback: first non-empty line of body
                    body.lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("(no description)")
                        .trim()
                        .to_string()
                });

                let permission_mode = front_matter
                    .permission_mode
                    .as_deref()
                    .map(|s| s.parse::<PermissionMode>())
                    .transpose()
                    .map_err(|e| anyhow::anyhow!(e))?
                    .unwrap_or_default();

                return Ok(SubagentDefinition {
                    name,
                    description,
                    system_prompt: body,
                    model: front_matter.model,
                    tools: front_matter.tools,
                    disallowed_tools: front_matter.disallowed_tools,
                    permission_mode,
                    max_turns: front_matter.max_turns,
                    source,
                    isolation: front_matter
                        .isolation
                        .as_deref()
                        .map(|s| s.parse::<IsolationMode>())
                        .transpose()
                        .map_err(|e| anyhow::anyhow!(e))?
                        .unwrap_or_default(),
                    on_start: front_matter.on_start,
                    on_complete: front_matter.on_complete,
                });
            }
        }

        // No frontmatter — use entire content as system prompt
        let (description, system_prompt) = Self::parse_simple(trimmed);

        Ok(SubagentDefinition {
            name: fallback_name,
            description,
            system_prompt,
            model: None,
            tools: None,
            disallowed_tools: None,
            permission_mode: PermissionMode::Default,
            max_turns: None,
            source,
            isolation: IsolationMode::default(),
            on_start: None,
            on_complete: None,
        })
    }

    /// Parse YAML frontmatter from a Markdown file.
    ///
    /// Expected format:
    /// ```text
    /// ---
    /// name: code-reviewer
    /// description: Reviews code for quality
    /// tools: read_file, list_directory
    /// model: sonnet
    /// ---
    /// System prompt body here...
    /// ```
    fn parse_front_matter(content: &str) -> Option<(FrontMatter, String)> {
        // Find the closing `---`
        let after_first = &content[3..]; // skip opening `---`
        let closing_pos = after_first.find("\n---")?;

        let yaml_block = &after_first[..closing_pos];
        let body = after_first[closing_pos + 4..].trim().to_string();

        let mut fm = FrontMatter {
            name: None,
            description: None,
            model: None,
            tools: None,
            disallowed_tools: None,
            permission_mode: None,
            max_turns: None,
            isolation: None,
            on_start: None,
            on_complete: None,
        };

        // Simple line-by-line YAML parsing (avoids pulling in a full YAML parser
        // dependency just for frontmatter — same approach as SkillLoader).
        //
        // State machine:
        //   current_key=None                → looking for a new "key: value" line
        //   current_key=Some, value="|"|">" → accumulating multiline continuation lines
        //   current_key=Some, value=other   → single-line value, will flush on next key
        let mut current_key: Option<String> = None;
        let mut multiline_value = String::new();

        for line in yaml_block.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Check if this is a continuation of a multiline value.
            // NOTE: Must check the *original* line (not `trimmed`) because
            // `trim()` strips the leading whitespace that signals continuation.
            if line.starts_with("  ") || line.starts_with('\t') {
                if current_key.is_some() {
                    if !multiline_value.is_empty() {
                        multiline_value.push(' ');
                    }
                    multiline_value.push_str(trimmed);
                    continue;
                }
            }

            // Flush previous multiline value
            if let Some(ref key) = current_key {
                Self::apply_frontmatter_field(&mut fm, key, &multiline_value);
            }

            // Parse key: value
            if let Some(colon_pos) = trimmed.find(':') {
                let key = trimmed[..colon_pos].trim().to_string();
                let value = trimmed[colon_pos + 1..].trim().to_string();

                // Check for multiline indicator (| or >)
                if value == "|" || value == ">" {
                    current_key = Some(key);
                    multiline_value = String::new();
                } else {
                    current_key = Some(key.clone());
                    multiline_value = value;
                }
            }
        }

        // Flush the last field
        if let Some(ref key) = current_key {
            Self::apply_frontmatter_field(&mut fm, key, &multiline_value);
        }

        // Must have at least a description
        if fm.description.is_none() && fm.name.is_none() {
            return None;
        }

        Some((fm, body))
    }

    /// Apply a parsed YAML frontmatter key-value pair to the `FrontMatter` struct.
    ///
    /// Supports both camelCase (`maxTurns`) and snake_case (`max_turns`) variants
    /// for each field name. Unknown keys are logged and ignored.
    fn apply_frontmatter_field(fm: &mut FrontMatter, key: &str, value: &str) {
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            return;
        }

        match key {
            "name" => fm.name = Some(value.to_string()),
            "description" => fm.description = Some(value.to_string()),
            "model" => fm.model = Some(value.to_string()),
            "tools" => {
                fm.tools = Some(
                    value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            "disallowedTools" | "disallowed_tools" => {
                fm.disallowed_tools = Some(
                    value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            "permissionMode" | "permission_mode" => {
                fm.permission_mode = Some(value.to_string());
            }
            "maxTurns" | "max_turns" => {
                if let Ok(n) = value.parse::<usize>() {
                    fm.max_turns = Some(n);
                }
            }
            "isolation" => {
                fm.isolation = Some(value.to_string());
            }
            "onStart" | "on_start" => {
                fm.on_start = Some(value.to_string());
            }
            "onComplete" | "on_complete" => {
                fm.on_complete = Some(value.to_string());
            }
            _ => {
                tracing::debug!(key = key, value = value, "Unknown frontmatter field, ignoring");
            }
        }
    }

    /// Simple parsing fallback when no YAML frontmatter is present.
    ///
    /// Returns `(description, system_prompt)` where:
    /// - `description`: the first non-empty line (with leading `# ` stripped if present)
    /// - `system_prompt`: the entire file content (used as-is)
    fn parse_simple(content: &str) -> (String, String) {
        let mut lines = content.lines();

        let description = loop {
            match lines.next() {
                Some(line) if !line.trim().is_empty() => {
                    let trimmed = line.trim();
                    let desc = trimmed.strip_prefix("# ").unwrap_or(trimmed);
                    break desc.to_string();
                }
                Some(_) => continue,
                None => break String::from("(no description)"),
            }
        };

        (description, content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_front_matter_basic() {
        let content = "---\nname: code-reviewer\ndescription: Reviews code quality\n---\n\nYou are a code reviewer.";
        let (fm, body) = SubagentLoader::parse_front_matter(content).unwrap();
        assert_eq!(fm.name.unwrap(), "code-reviewer");
        assert_eq!(fm.description.unwrap(), "Reviews code quality");
        assert_eq!(body, "You are a code reviewer.");
    }

    #[test]
    fn test_parse_front_matter_with_tools() {
        let content = "---\nname: safe-reader\ndescription: Read-only agent\ntools: read_file, list_directory, search_files\nmodel: haiku\nmaxTurns: 5\n---\n\nOnly read files.";
        let (fm, body) = SubagentLoader::parse_front_matter(content).unwrap();
        assert_eq!(fm.name.unwrap(), "safe-reader");
        assert_eq!(fm.tools.unwrap(), vec!["read_file", "list_directory", "search_files"]);
        assert_eq!(fm.model.unwrap(), "haiku");
        assert_eq!(fm.max_turns.unwrap(), 5);
        assert_eq!(body, "Only read files.");
    }

    #[test]
    fn test_parse_front_matter_with_disallowed_tools() {
        let content = "---\nname: no-writes\ndescription: No write access\ndisallowedTools: write_file\npermissionMode: dontAsk\n---\n\nDo not write.";
        let (fm, _body) = SubagentLoader::parse_front_matter(content).unwrap();
        assert_eq!(fm.disallowed_tools.unwrap(), vec!["write_file"]);
        assert_eq!(fm.permission_mode.unwrap(), "dontAsk");
    }

    #[test]
    fn test_parse_simple_with_heading() {
        let content = "# Code Review Expert\n\nReview code carefully.";
        let (desc, instructions) = SubagentLoader::parse_simple(content);
        assert_eq!(desc, "Code Review Expert");
        assert_eq!(instructions, content);
    }

    #[test]
    fn test_load_from_nonexistent_dir() {
        let result = SubagentLoader::load_from_dir(
            std::path::Path::new("/nonexistent/path"),
            SubagentSource::Project,
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_load_from_dir_with_md_files() {
        let dir = std::env::temp_dir().join("daedalus_subagent_loader_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create a subagent with frontmatter
        fs::write(
            dir.join("code-reviewer.md"),
            "---\nname: code-reviewer\ndescription: Reviews code quality\ntools: read_file, list_directory\nmodel: sonnet\n---\n\nYou are a code reviewer.",
        ).unwrap();

        // Create a subagent without frontmatter
        fs::write(
            dir.join("simple-agent.md"),
            "# Simple Helper\n\nHelp with simple tasks.",
        ).unwrap();

        // Create a non-md file (should be ignored)
        fs::write(dir.join("notes.txt"), "This should be ignored").unwrap();

        let agents = SubagentLoader::load_from_dir(&dir, SubagentSource::Project).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "code-reviewer");
        assert_eq!(agents[0].description, "Reviews code quality");
        assert_eq!(agents[0].tools, Some(vec!["read_file".to_string(), "list_directory".to_string()]));
        assert_eq!(agents[0].model, Some("sonnet".to_string()));
        assert_eq!(agents[1].name, "simple-agent");
        assert_eq!(agents[1].description, "Simple Helper");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_permission_mode_parsing() {
        assert_eq!("default".parse::<PermissionMode>().unwrap(), PermissionMode::Default);
        assert_eq!("acceptEdits".parse::<PermissionMode>().unwrap(), PermissionMode::AcceptEdits);
        assert_eq!("dontAsk".parse::<PermissionMode>().unwrap(), PermissionMode::DontAsk);
        assert_eq!("bypassPermissions".parse::<PermissionMode>().unwrap(), PermissionMode::BypassPermissions);
        assert_eq!("plan".parse::<PermissionMode>().unwrap(), PermissionMode::Plan);
        assert!("invalid".parse::<PermissionMode>().is_err());
    }
}
