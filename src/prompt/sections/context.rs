use chrono::Local;

/// Build the dynamic context section of the system prompt.
///
/// This section injects runtime context that changes between sessions,
/// such as the current date/time and memory summaries.
///
/// # Arguments
/// * `memory_context` - Optional long-term memory summary to inject.
pub fn build_context_section(memory_context: Option<&str>) -> String {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d, %A").to_string();

    let memory_block = match memory_context {
        Some(ctx) if !ctx.trim().is_empty() => {
            format!(
                "\n<memory>\n\
                 The following is what you remember about the user from previous conversations. \
                 Use this to personalize your responses, but do not mention it unless relevant.\n\
                 \n\
                 {ctx}\n\
                 </memory>\n"
            )
        }
        _ => String::new(),
    };

    format!(
        "<context>\n\
         <current_date>{date_str}</current_date>{memory_block}\n\
         </context>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_without_memory() {
        let section = build_context_section(None);
        assert!(section.contains("<current_date>"));
        assert!(section.contains("</current_date>"));
        assert!(!section.contains("<memory>"));
    }

    #[test]
    fn test_context_with_memory() {
        let section = build_context_section(Some("User prefers Rust and concise answers."));
        assert!(section.contains("<memory>"));
        assert!(section.contains("User prefers Rust"));
    }

    #[test]
    fn test_context_with_empty_memory() {
        let section = build_context_section(Some("   "));
        assert!(!section.contains("<memory>"));
    }
}
