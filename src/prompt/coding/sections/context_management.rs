
//! Context management awareness section.
//!
//! Informs the model about automatic context management mechanisms
//! (sliding window, tool result truncation, conversation consolidation)
//! so it can cooperate with these systems rather than fight them.

/// Build the context management awareness section.
///
/// This section is placed in the dynamic suffix because it describes
/// runtime behavior that the model needs to be aware of.
pub fn build() -> String {
    "<context_management>\n\
     This conversation uses automatic context management:\n\
     - Old tool results may be summarized or removed to stay within context limits. \
     Do not reference specific tool outputs from many rounds ago — re-read files if needed.\n\
     - Conversation history is automatically consolidated via sliding window. \
     You have effectively unlimited conversation length through automatic summarization.\n\
     - If you notice missing context from earlier in the conversation, use tools to \
     re-gather the information rather than guessing from memory.\n\
     </context_management>"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_management() {
        let section = build();
        assert!(section.contains("<context_management>"));
        assert!(section.contains("</context_management>"));
        assert!(section.contains("sliding window"));
        assert!(section.contains("re-read files"));
    }
}
