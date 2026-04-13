use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::SkillDefinition;

/// The canonical skill definition filename within each skill subdirectory.
const SKILL_FILENAME: &str = "SKILL.md";

/// Loads skill definitions from the filesystem.
///
/// Skills are organized as subdirectories under a root skills directory.
/// Each subdirectory represents one skill and must contain a `SKILL.md` file
/// as the entry point.
///
/// ## Directory structure
///
/// ```text
/// skills/
/// ├── code-review/
/// │   └── SKILL.md
/// ├── sql-expert/
/// │   ├── SKILL.md
/// │   └── examples.sql      # optional resource files
/// └── skill-creator/
///     └── SKILL.md
/// ```
///
/// ## SKILL.md format
///
/// A `SKILL.md` file can optionally start with a YAML front-matter block
/// containing a `description` field:
///
/// ```markdown
/// ---
/// description: Expert code reviewer following best practices
/// ---
///
/// When reviewing code, follow these steps:
/// ...
/// ```
///
/// If no front-matter is present, the first non-empty line is used as
/// the description, and the entire file content is the instructions body.
pub struct SkillLoader;

impl SkillLoader {
    /// Load all skill definitions from a directory.
    ///
    /// Scans for subdirectories containing `SKILL.md` files.
    /// Subdirectories without `SKILL.md` are silently skipped.
    /// Subdirectories that fail to load are logged as warnings and skipped.
    pub fn load_from_dir(dir: &Path) -> Result<Vec<SkillDefinition>> {
        if !dir.exists() {
            tracing::debug!(path = %dir.display(), "Skills directory does not exist, skipping");
            return Ok(Vec::new());
        }

        if !dir.is_dir() {
            tracing::warn!(path = %dir.display(), "Skills path is not a directory, skipping");
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read skills directory: {}", dir.display()))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();

        // Sort for deterministic loading order
        entries.sort();

        for subdir in entries {
            let skill_file = subdir.join(SKILL_FILENAME);
            if !skill_file.exists() {
                tracing::debug!(
                    path = %subdir.display(),
                    "Subdirectory has no {}, skipping",
                    SKILL_FILENAME
                );
                continue;
            }

            match Self::load_skill(&subdir, &skill_file) {
                Ok(skill) => {
                    tracing::info!(
                        skill = %skill.name,
                        description_len = skill.description.len(),
                        instructions_len = skill.instructions.len(),
                        "Loaded skill"
                    );
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %skill_file.display(),
                        error = %e,
                        "Failed to load skill, skipping"
                    );
                }
            }
        }

        Ok(skills)
    }

    /// Load a single skill from a subdirectory's SKILL.md file.
    ///
    /// The skill name is derived from the subdirectory name.
    fn load_skill(subdir: &Path, skill_file: &Path) -> Result<SkillDefinition> {
        let name = subdir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid skill directory name: {}", subdir.display()))?
            .to_string();

        let content = std::fs::read_to_string(skill_file)
            .with_context(|| format!("Failed to read {}", skill_file.display()))?;

        let trimmed = content.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{} is empty: {}", SKILL_FILENAME, skill_file.display());
        }

        // Try to parse YAML front-matter
        if trimmed.starts_with("---") {
            if let Some((description, instructions)) = Self::parse_front_matter(trimmed) {
                return Ok(SkillDefinition {
                    name,
                    description,
                    instructions,
                });
            }
        }

        // Fallback: first non-empty line is description, rest is instructions
        let (description, instructions) = Self::parse_simple(trimmed);

        Ok(SkillDefinition {
            name,
            description,
            instructions,
        })
    }

    /// Parse YAML front-matter to extract description.
    ///
    /// Expected format:
    /// ```text
    /// ---
    /// description: Some description here
    /// ---
    /// Rest of the content...
    /// ```
    fn parse_front_matter(content: &str) -> Option<(String, String)> {
        // Find the closing `---`
        let after_first = &content[3..]; // skip opening `---`
        let closing_pos = after_first.find("\n---")?;

        let front_matter = &after_first[..closing_pos];
        let body = after_first[closing_pos + 4..].trim(); // skip `\n---`

        // Simple YAML parsing: look for `description:` line
        let description = front_matter
            .lines()
            .find_map(|line| {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("description:") {
                    let desc = rest.trim().trim_matches('"').trim_matches('\'');
                    if !desc.is_empty() {
                        return Some(desc.to_string());
                    }
                }
                None
            })?;

        // Use the body after front-matter as instructions.
        // If body is empty, use the entire content (including front-matter) as instructions.
        let instructions = if body.is_empty() {
            content.to_string()
        } else {
            body.to_string()
        };

        Some((description, instructions))
    }

    /// Simple parsing: first non-empty line = description, rest = instructions.
    ///
    /// If the first line starts with `# `, the heading prefix is stripped
    /// for a cleaner description.
    fn parse_simple(content: &str) -> (String, String) {
        let mut lines = content.lines();

        // Find first non-empty line for description
        let description = loop {
            match lines.next() {
                Some(line) if !line.trim().is_empty() => {
                    let trimmed = line.trim();
                    // Strip markdown heading prefix
                    let desc = trimmed
                        .strip_prefix("# ")
                        .unwrap_or(trimmed);
                    break desc.to_string();
                }
                Some(_) => continue,
                None => break String::from("(no description)"),
            }
        };

        // The full content is the instructions (including the first line)
        let instructions = content.to_string();

        (description, instructions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_front_matter() {
        let content = "---\ndescription: Expert code reviewer\n---\n\nReview code carefully.";
        let (desc, body) = SkillLoader::parse_front_matter(content).unwrap();
        assert_eq!(desc, "Expert code reviewer");
        assert_eq!(body, "Review code carefully.");
    }

    #[test]
    fn test_parse_front_matter_with_quotes() {
        let content = "---\ndescription: \"Quoted description\"\n---\n\nBody here.";
        let (desc, _) = SkillLoader::parse_front_matter(content).unwrap();
        assert_eq!(desc, "Quoted description");
    }

    #[test]
    fn test_parse_simple_with_heading() {
        let content = "# Code Review Expert\n\nReview code carefully.";
        let (desc, instructions) = SkillLoader::parse_simple(content);
        assert_eq!(desc, "Code Review Expert");
        assert_eq!(instructions, content);
    }

    #[test]
    fn test_parse_simple_without_heading() {
        let content = "This is a skill description\n\nWith instructions.";
        let (desc, _) = SkillLoader::parse_simple(content);
        assert_eq!(desc, "This is a skill description");
    }

    #[test]
    fn test_load_from_nonexistent_dir() {
        let result = SkillLoader::load_from_dir(Path::new("/nonexistent/path"));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_load_from_dir_with_subdirectories() {
        let dir = std::env::temp_dir().join("daedalus_skill_subdir_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create a skill subdirectory with SKILL.md (front-matter format)
        let code_review_dir = dir.join("code-review");
        fs::create_dir_all(&code_review_dir).unwrap();
        fs::write(
            code_review_dir.join("SKILL.md"),
            "---\ndescription: Expert code reviewer\n---\n\nReview code step by step.",
        ).unwrap();

        // Create a skill subdirectory with SKILL.md (simple heading format)
        let sql_dir = dir.join("sql-expert");
        fs::create_dir_all(&sql_dir).unwrap();
        fs::write(
            sql_dir.join("SKILL.md"),
            "# SQL Query Expert\n\nHelp write optimized SQL queries.",
        ).unwrap();

        // Create a subdirectory without SKILL.md (should be skipped)
        let empty_dir = dir.join("incomplete-skill");
        fs::create_dir_all(&empty_dir).unwrap();
        fs::write(empty_dir.join("notes.txt"), "No SKILL.md here").unwrap();

        // Create a regular file at root level (should be ignored)
        fs::write(dir.join("README.md"), "This should be ignored").unwrap();

        let skills = SkillLoader::load_from_dir(&dir).unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "code-review");
        assert_eq!(skills[0].description, "Expert code reviewer");
        assert_eq!(skills[1].name, "sql-expert");
        assert_eq!(skills[1].description, "SQL Query Expert");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_skill_name_from_directory() {
        let dir = std::env::temp_dir().join("daedalus_skill_name_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let skill_dir = dir.join("my-custom-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: A custom skill\n---\n\nDo custom things.",
        ).unwrap();

        let skills = SkillLoader::load_from_dir(&dir).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-custom-skill");

        let _ = fs::remove_dir_all(&dir);
    }
}
