//! Environment context section — runtime system information.
//!
//! Injects OS, shell, CWD, project type, and current date into the prompt.
//! This enables context-aware decisions.

use chrono::Local;

use super::super::EnvironmentContext;

/// Build the environment context section.
///
/// Injects runtime information that helps the agent make context-aware
/// decisions (e.g., which package manager to use, what OS commands are available).
pub fn build(env: Option<&EnvironmentContext>) -> String {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d, %A").to_string();

    let env_details = match env {
        Some(ctx) => {
            let mut lines = vec![
                format!("- Operating System: {}", ctx.os),
                format!("- Default Shell: {}", ctx.shell),
                format!("- Current Working Directory: {}", ctx.cwd),
            ];
            if let Some(ref pt) = ctx.project_type {
                lines.push(format!("- Project Type: {}", pt));
            }
            lines.push(format!("- Current Date: {}", date_str));
            lines.join("\n")
        }
        None => {
            format!("- Current Date: {}", date_str)
        }
    };

    format!(
        "<environment>\n\
         {env_details}\n\
         </environment>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_without_context() {
        let section = build(None);
        assert!(section.contains("<environment>"));
        assert!(section.contains("Current Date"));
        assert!(!section.contains("Operating System"));
    }

    #[test]
    fn test_environment_with_context() {
        let env = EnvironmentContext {
            os: "linux".to_string(),
            shell: "/bin/bash".to_string(),
            cwd: "/home/user/project".to_string(),
            project_type: Some("Rust/Cargo".to_string()),
        };
        let section = build(Some(&env));
        assert!(section.contains("linux"));
        assert!(section.contains("/bin/bash"));
        assert!(section.contains("/home/user/project"));
        assert!(section.contains("Rust/Cargo"));
    }

    #[test]
    fn test_environment_without_project_type() {
        let env = EnvironmentContext {
            os: "macos".to_string(),
            shell: "/bin/zsh".to_string(),
            cwd: "/tmp".to_string(),
            project_type: None,
        };
        let section = build(Some(&env));
        assert!(section.contains("macos"));
        assert!(!section.contains("Project Type"));
    }
}
