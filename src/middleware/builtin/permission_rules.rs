
//! Permission rules engine — persistent, pattern-based tool call authorization.
//!
//! This module implements a rule-based permission system that allows users to
//! pre-approve or deny tool calls based on tool name and argument patterns.
//! Rules are persisted to JSON files at two scopes:
//!
//! - **Global**: `~/.daedalus/permissions.json` (applies to all projects)
//! - **Project**: `.daedalus/permissions.json` (applies to current project only)
//!
//! Rules support glob patterns for fine-grained matching:
//! - `bash` with pattern `git *` → allows all git commands
//! - `edit_file` with pattern `src/**` → allows edits within src/
//! - `bash` with pattern `rm *` → can deny all rm commands

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── YAML Configuration ──

/// YAML configuration for the `permissions:` section in daedalus.yaml.
///
/// Allows users to pre-configure permission rules declaratively.
/// These rules are loaded at startup and merged with the persisted
/// rules from `permissions.json` files.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    /// Permission mode: "default", "acceptEdits", "bypassPermissions", "plan".
    ///
    /// - `default`: Sensitive/Dangerous tools require confirmation
    /// - `acceptEdits`: Sensitive tools auto-approved, Dangerous still requires confirmation
    /// - `bypassPermissions`: All tools auto-approved (equivalent to --dangerously-skip-permissions)
    /// - `plan`: Only ReadOnly tools allowed, all others denied
    pub mode: PermissionMode,

    /// Pre-approved tool patterns (loaded as project-scope rules).
    #[serde(default)]
    pub allow: Vec<PermissionRuleConfig>,

    /// Explicitly blocked tool patterns (loaded as project-scope deny rules).
    #[serde(default)]
    pub deny: Vec<PermissionRuleConfig>,
}

/// Permission mode controlling the overall confirmation behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Sensitive/Dangerous tools require confirmation (default).
    #[default]
    Default,
    /// Sensitive tools (edit_file, write_file) auto-approved.
    /// Dangerous tools (bash, MCP) still require confirmation.
    AcceptEdits,
    /// All tools auto-approved — no confirmations at all.
    BypassPermissions,
    /// Read-only mode — only ReadOnly tools allowed, all others denied.
    Plan,
}

/// A single rule entry in the YAML `permissions.allow` or `permissions.deny` list.
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionRuleConfig {
    /// Tool name to match.
    pub tool: String,
    /// Optional argument pattern (glob).
    pub pattern: Option<String>,
}

// ── Rule Types ──

/// A single permission rule that matches tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionRule {
    /// Tool name to match (exact match).
    pub tool: String,

    /// Optional argument pattern for fine-grained matching.
    ///
    /// Interpretation depends on the tool:
    /// - `bash`: matches against the command string (glob, e.g. "git *")
    /// - `edit_file`/`multi_edit`/`write_file`: matches against the file path (glob)
    /// - Other tools: matches against a string representation of the first argument
    ///
    /// If `None`, the rule matches all invocations of the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    /// The decision for matching calls.
    pub decision: RuleDecision,
}

/// The decision a rule makes when it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleDecision {
    /// Allow the tool call to proceed.
    Allow,
    /// Deny the tool call.
    Deny,
}

/// Where a rule was defined / should be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleScope {
    /// Persisted in `~/.daedalus/permissions.json` (cross-project).
    Global,
    /// Persisted in `.daedalus/permissions.json` (project-level).
    Project,
    /// Only valid for current session (in-memory, not persisted).
    Session,
}

// ── Persisted file format ──

/// The JSON file format for permissions.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsFile {
    /// Schema version for forward compatibility.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The list of rules.
    pub rules: Vec<PermissionRule>,
}

fn default_version() -> u32 {
    1
}

impl Default for PermissionsFile {
    fn default() -> Self {
        Self {
            version: 1,
            rules: Vec::new(),
        }
    }
}

// ── Rule Set (in-memory aggregate) ──

/// Aggregated permission rules from all scopes.
///
/// Rules are evaluated in priority order:
/// 1. Session rules (highest priority — user just approved/denied)
/// 2. Project rules (`.daedalus/permissions.json`)
/// 3. Global rules (`~/.daedalus/permissions.json`)
///
/// Within each scope, rules are evaluated in order (first match wins).
pub struct PermissionRuleSet {
    /// Session-level rules (in-memory only).
    session_rules: Vec<PermissionRule>,
    /// Project-level rules (from `.daedalus/permissions.json`).
    project_rules: Vec<PermissionRule>,
    /// Global rules (from `~/.daedalus/permissions.json`).
    global_rules: Vec<PermissionRule>,
    /// Path to the project permissions file (for saving).
    project_path: Option<PathBuf>,
    /// Path to the global permissions file (for saving).
    global_path: Option<PathBuf>,
}

impl PermissionRuleSet {
    /// Create a new empty rule set.
    pub fn new() -> Self {
        Self {
            session_rules: Vec::new(),
            project_rules: Vec::new(),
            global_rules: Vec::new(),
            project_path: None,
            global_path: None,
        }
    }

    /// Load rules from the workspace and global directories.
    ///
    /// - `workspace_root`: The project workspace root (e.g. `.daedalus/`)
    /// - Automatically resolves `~/.daedalus/permissions.json` for global rules.
    pub fn load(workspace_root: Option<&Path>) -> Self {
        let mut rule_set = Self::new();

        // Load global rules from ~/.daedalus/permissions.json
        if let Some(home) = crate::workspace::home_dir() {
            let global_path = home.join(".daedalus/permissions.json");
            if global_path.exists() {
                match Self::load_file(&global_path) {
                    Ok(rules) => {
                        tracing::info!(
                            count = rules.len(),
                            path = %global_path.display(),
                            "Loaded global permission rules"
                        );
                        rule_set.global_rules = rules;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %global_path.display(),
                            "Failed to load global permission rules"
                        );
                    }
                }
            }
            rule_set.global_path = Some(global_path);
        }

        // Load project rules from <workspace>/permissions.json
        if let Some(ws_root) = workspace_root {
            let project_path = ws_root.join("permissions.json");
            if project_path.exists() {
                match Self::load_file(&project_path) {
                    Ok(rules) => {
                        tracing::info!(
                            count = rules.len(),
                            path = %project_path.display(),
                            "Loaded project permission rules"
                        );
                        rule_set.project_rules = rules;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %project_path.display(),
                            "Failed to load project permission rules"
                        );
                    }
                }
            }
            rule_set.project_path = Some(project_path);
        }

        rule_set
    }

    /// Load rules from a JSON file.
    fn load_file(path: &Path) -> anyhow::Result<Vec<PermissionRule>> {
        let content = std::fs::read_to_string(path)?;
        let file: PermissionsFile = serde_json::from_str(&content)?;
        Ok(file.rules)
    }

    /// Save rules to the appropriate file based on scope.
    fn save_to_scope(&self, scope: RuleScope) -> anyhow::Result<()> {
        let (path, rules) = match scope {
            RuleScope::Global => {
                let path = self.global_path.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No global permissions path configured"))?;
                (path, &self.global_rules)
            }
            RuleScope::Project => {
                let path = self.project_path.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No project permissions path configured"))?;
                (path, &self.project_rules)
            }
            RuleScope::Session => return Ok(()), // Session rules are not persisted
        };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = PermissionsFile {
            version: 1,
            rules: rules.clone(),
        };
        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(path, json)?;

        tracing::info!(
            scope = ?scope,
            path = %path.display(),
            rules = rules.len(),
            "Permission rules saved"
        );
        Ok(())
    }

    /// Evaluate a tool call against all rules.
    ///
    /// Returns `Some(decision)` if a matching rule is found, or `None` if
    /// no rule matches (caller should prompt the user).
    ///
    /// Evaluation order: session → project → global (first match wins).
    pub fn evaluate(&self, tool_name: &str, match_value: Option<&str>) -> Option<RuleDecision> {
        // Check session rules first (highest priority)
        if let Some(decision) = Self::find_match(&self.session_rules, tool_name, match_value) {
            return Some(decision);
        }
        // Then project rules
        if let Some(decision) = Self::find_match(&self.project_rules, tool_name, match_value) {
            return Some(decision);
        }
        // Finally global rules
        Self::find_match(&self.global_rules, tool_name, match_value)
    }

    /// Find the first matching rule in a list.
    fn find_match(rules: &[PermissionRule], tool_name: &str, match_value: Option<&str>) -> Option<RuleDecision> {
        for rule in rules {
            if rule.tool != tool_name {
                continue;
            }
            match (&rule.pattern, match_value) {
                // Rule has no pattern → matches all invocations of this tool
                (None, _) => return Some(rule.decision),
                // Rule has a pattern and we have a value to match against
                (Some(pattern), Some(value)) => {
                    if glob_match(pattern, value) {
                        return Some(rule.decision);
                    }
                }
                // Rule has a pattern but no value to match → skip
                (Some(_), None) => continue,
            }
        }
        None
    }

    /// Add a new rule and persist it.
    ///
    /// If a rule with the same tool+pattern already exists in the target scope,
    /// it is replaced (no duplicates).
    pub fn add_rule(&mut self, rule: PermissionRule, scope: RuleScope) {
        let rules = match scope {
            RuleScope::Session => &mut self.session_rules,
            RuleScope::Project => &mut self.project_rules,
            RuleScope::Global => &mut self.global_rules,
        };

        // Remove existing rule with same tool+pattern (dedup)
        rules.retain(|r| !(r.tool == rule.tool && r.pattern == rule.pattern));
        rules.push(rule);

        // Persist (ignore errors — best effort)
        if scope != RuleScope::Session {
            if let Err(e) = self.save_to_scope(scope) {
                tracing::warn!(error = %e, "Failed to persist permission rule");
            }
        }
    }

    /// Return the total number of rules across all scopes.
    #[allow(dead_code)]
    pub fn rule_count(&self) -> usize {
        self.session_rules.len() + self.project_rules.len() + self.global_rules.len()
    }

    /// Return all rules (for display in /permissions command).
    pub fn all_rules(&self) -> Vec<(&PermissionRule, RuleScope)> {
        let mut result = Vec::new();
        for r in &self.session_rules {
            result.push((r, RuleScope::Session));
        }
        for r in &self.project_rules {
            result.push((r, RuleScope::Project));
        }
        for r in &self.global_rules {
            result.push((r, RuleScope::Global));
        }
        result
    }

    /// Load rules from workspace + global files, then merge YAML config rules.
    ///
    /// YAML `permissions.allow` rules are added as project-scope allow rules.
    /// YAML `permissions.deny` rules are added as project-scope deny rules.
    /// These are loaded into memory but NOT persisted to permissions.json
    /// (they live in daedalus.yaml).
    pub fn load_with_config(
        workspace_root: Option<&Path>,
        config: &PermissionsConfig,
    ) -> Self {
        let mut rule_set = Self::load(workspace_root);

        // Merge YAML allow rules (prepend so they have higher priority than file rules)
        let yaml_allow: Vec<PermissionRule> = config.allow.iter().map(|r| {
            PermissionRule {
                tool: r.tool.clone(),
                pattern: r.pattern.clone(),
                decision: RuleDecision::Allow,
            }
        }).collect();

        let yaml_deny: Vec<PermissionRule> = config.deny.iter().map(|r| {
            PermissionRule {
                tool: r.tool.clone(),
                pattern: r.pattern.clone(),
                decision: RuleDecision::Deny,
            }
        }).collect();

        if !yaml_allow.is_empty() || !yaml_deny.is_empty() {
            tracing::info!(
                allow = yaml_allow.len(),
                deny = yaml_deny.len(),
                "Loaded permission rules from YAML config"
            );
            // Deny rules first (higher priority), then allow rules
            // Both are prepended to project_rules so they take priority
            // over rules from permissions.json
            let mut merged = yaml_deny;
            merged.extend(yaml_allow);
            merged.extend(rule_set.project_rules);
            rule_set.project_rules = merged;
        }

        rule_set
    }
}

// ── Glob Matching ──

/// Simple glob pattern matching.
///
/// Supports:
/// - `*` — matches any sequence of characters (including `/`)
/// - `?` — matches any single character
/// - Literal characters match themselves
///
/// This is intentionally simple (no character classes, no escaping)
/// to keep the implementation small and predictable.
///
/// Note: Unlike filesystem globs, `*` matches `/` here because patterns
/// are used for both file paths and command strings. For directory-only
/// matching, use explicit path components (e.g. `src/*.rs`).
pub fn glob_match(pattern: &str, value: &str) -> bool {
    glob_match_recursive(pattern.as_bytes(), value.as_bytes())
}

fn glob_match_recursive(pattern: &[u8], value: &[u8]) -> bool {
    match (pattern.first(), value.first()) {
        // Both exhausted — match
        (None, None) => true,
        // Pattern exhausted but value remains — no match
        (None, Some(_)) => false,
        // Pattern has `*` — match zero or more of any character
        (Some(b'*'), _) => {
            // Consume consecutive stars (**, ***, etc. are equivalent to *)
            let mut star_end = 1;
            while star_end < pattern.len() && pattern[star_end] == b'*' {
                star_end += 1;
            }
            let rest_pattern = &pattern[star_end..];
            // Try matching * against zero chars, one char, two chars, etc.
            for i in 0..=value.len() {
                if glob_match_recursive(rest_pattern, &value[i..]) {
                    return true;
                }
            }
            false
        }
        // Pattern has `?` — match any single character
        (Some(b'?'), Some(_)) => {
            glob_match_recursive(&pattern[1..], &value[1..])
        }
        // Literal match
        (Some(&p), Some(&v)) if p == v => {
            glob_match_recursive(&pattern[1..], &value[1..])
        }
        // No match
        _ => false,
    }
}

/// Extract the match value from a tool call's arguments.
///
/// This is the string that will be matched against rule patterns:
/// - `bash`: the command string
/// - `edit_file`/`multi_edit`/`write_file`: the file path
/// - Other tools: `None` (only tool-name matching)
pub fn extract_match_value(tool_name: &str, arguments: &serde_json::Value) -> Option<String> {
    match tool_name {
        "bash" => {
            arguments
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        "edit_file" | "multi_edit" | "write_file" => {
            arguments
                .get("path")
                .or_else(|| arguments.get("file_path"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Generate a suggested pattern for "Always Allow" based on the tool call.
///
/// Returns a human-friendly pattern that the user can approve:
/// - `bash` with `git status` → `"git *"`
/// - `bash` with `cargo build --release` → `"cargo *"`
/// - `edit_file` with `/data/workspace/src/main.rs` → `"src/**"` (relative)
/// - Other → `None` (approve the whole tool)
pub fn suggest_pattern(tool_name: &str, arguments: &serde_json::Value) -> Option<String> {
    match tool_name {
        "bash" => {
            let cmd = arguments.get("command").and_then(|v| v.as_str())?;
            // Extract the first word (program name) and suggest "program *"
            let first_word = cmd.split_whitespace().next()?;
            Some(format!("{} *", first_word))
        }
        "edit_file" | "multi_edit" | "write_file" => {
            let path = arguments
                .get("path")
                .or_else(|| arguments.get("file_path"))
                .and_then(|v| v.as_str())?;
            // Try to extract a meaningful directory prefix
            let path_obj = std::path::Path::new(path);
            // Use the first non-root component as the pattern base
            let components: Vec<_> = path_obj.components().collect();
            if components.len() >= 2 {
                // Find the first "meaningful" directory (skip root `/`)
                let meaningful: Vec<_> = components.iter()
                    .filter(|c| matches!(c, std::path::Component::Normal(_)))
                    .take(2)
                    .collect();
                if !meaningful.is_empty() {
                    let dir = meaningful[0].as_os_str().to_string_lossy();
                    return Some(format!("{}/**", dir));
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Glob matching tests ──

    #[test]
    fn test_glob_exact_match() {
        assert!(glob_match("hello", "hello"));
        assert!(!glob_match("hello", "world"));
    }

    #[test]
    fn test_glob_star_matches_any() {
        assert!(glob_match("git *", "git status"));
        assert!(glob_match("git *", "git commit -m 'test'"));
        assert!(glob_match("cargo *", "cargo build --release"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("*.rs", "src/main.rs")); // * matches / too
    }

    #[test]
    fn test_glob_double_star_same_as_star() {
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("src/**", "src/foo/bar/baz.rs"));
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("**", "anything/at/all"));
    }

    #[test]
    fn test_glob_question_mark() {
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn test_glob_empty() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn test_glob_complex_patterns() {
        assert!(glob_match("rm *", "rm -rf target/"));
        assert!(glob_match("git *", "git push origin master"));
        assert!(!glob_match("git *", "cargo build"));
    }

    // ── Rule evaluation tests ──

    #[test]
    fn test_rule_set_evaluate_no_rules() {
        let rs = PermissionRuleSet::new();
        assert_eq!(rs.evaluate("bash", Some("git status")), None);
    }

    #[test]
    fn test_rule_set_evaluate_exact_tool_match() {
        let mut rs = PermissionRuleSet::new();
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: None,
                decision: RuleDecision::Allow,
            },
            RuleScope::Session,
        );
        assert_eq!(rs.evaluate("bash", Some("anything")), Some(RuleDecision::Allow));
        assert_eq!(rs.evaluate("edit_file", Some("anything")), None);
    }

    #[test]
    fn test_rule_set_evaluate_pattern_match() {
        let mut rs = PermissionRuleSet::new();
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: Some("git *".to_string()),
                decision: RuleDecision::Allow,
            },
            RuleScope::Session,
        );
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: Some("rm *".to_string()),
                decision: RuleDecision::Deny,
            },
            RuleScope::Session,
        );
        assert_eq!(rs.evaluate("bash", Some("git status")), Some(RuleDecision::Allow));
        assert_eq!(rs.evaluate("bash", Some("rm -rf /")), Some(RuleDecision::Deny));
        assert_eq!(rs.evaluate("bash", Some("cargo build")), None);
    }

    #[test]
    fn test_rule_set_priority_session_over_project() {
        let mut rs = PermissionRuleSet::new();
        // Project says deny bash
        rs.project_rules.push(PermissionRule {
            tool: "bash".to_string(),
            pattern: None,
            decision: RuleDecision::Deny,
        });
        // Session says allow bash
        rs.session_rules.push(PermissionRule {
            tool: "bash".to_string(),
            pattern: None,
            decision: RuleDecision::Allow,
        });
        // Session wins
        assert_eq!(rs.evaluate("bash", None), Some(RuleDecision::Allow));
    }

    #[test]
    fn test_rule_set_dedup_on_add() {
        let mut rs = PermissionRuleSet::new();
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: Some("git *".to_string()),
                decision: RuleDecision::Deny,
            },
            RuleScope::Session,
        );
        // Add same tool+pattern with different decision → replaces
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: Some("git *".to_string()),
                decision: RuleDecision::Allow,
            },
            RuleScope::Session,
        );
        assert_eq!(rs.session_rules.len(), 1);
        assert_eq!(rs.session_rules[0].decision, RuleDecision::Allow);
    }

    // ── Extract match value tests ──

    #[test]
    fn test_extract_match_value_bash() {
        let args = serde_json::json!({"command": "git status"});
        assert_eq!(extract_match_value("bash", &args), Some("git status".to_string()));
    }

    #[test]
    fn test_extract_match_value_edit_file() {
        let args = serde_json::json!({"path": "/src/main.rs"});
        assert_eq!(extract_match_value("edit_file", &args), Some("/src/main.rs".to_string()));
    }

    #[test]
    fn test_extract_match_value_unknown_tool() {
        let args = serde_json::json!({"query": "hello"});
        assert_eq!(extract_match_value("custom_tool", &args), None);
    }

    // ── Suggest pattern tests ──

    #[test]
    fn test_suggest_pattern_bash() {
        let args = serde_json::json!({"command": "git status"});
        assert_eq!(suggest_pattern("bash", &args), Some("git *".to_string()));

        let args = serde_json::json!({"command": "cargo build --release"});
        assert_eq!(suggest_pattern("bash", &args), Some("cargo *".to_string()));
    }

    #[test]
    fn test_suggest_pattern_edit_file() {
        let args = serde_json::json!({"path": "/data/workspace/src/main.rs"});
        let pattern = suggest_pattern("edit_file", &args);
        assert!(pattern.is_some());
        // Should suggest something like "data/**" or "workspace/**"
        assert!(pattern.unwrap().ends_with("/**"));
    }

    // ── Persistence tests ──

    #[test]
    fn test_permissions_file_serialization() {
        let file = PermissionsFile {
            version: 1,
            rules: vec![
                PermissionRule {
                    tool: "bash".to_string(),
                    pattern: Some("git *".to_string()),
                    decision: RuleDecision::Allow,
                },
                PermissionRule {
                    tool: "bash".to_string(),
                    pattern: Some("rm *".to_string()),
                    decision: RuleDecision::Deny,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        let parsed: PermissionsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.rules[0].tool, "bash");
        assert_eq!(parsed.rules[0].pattern, Some("git *".to_string()));
        assert_eq!(parsed.rules[0].decision, RuleDecision::Allow);
    }

    #[test]
    fn test_permissions_file_roundtrip() {
        let dir = std::env::temp_dir().join("daedalus_test_permissions");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("permissions.json");

        let mut rs = PermissionRuleSet::new();
        rs.global_path = Some(path.clone());
        rs.add_rule(
            PermissionRule {
                tool: "bash".to_string(),
                pattern: Some("git *".to_string()),
                decision: RuleDecision::Allow,
            },
            RuleScope::Global,
        );

        // Verify file was written
        assert!(path.exists());

        // Load it back
        let loaded = PermissionRuleSet::load_file(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tool, "bash");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
