
//! Task strategy section — adapt behavior based on task type.
//!
//! Provides differentiated guidance for writing code, answering questions,
//! debugging, and exploring/reviewing code.

/// Build the task strategy section.
pub fn build() -> String {
    "<task_strategy>\n\
     Adapt your behavior based on the task type:\n\
     \n\
     **Writing code** (creating/modifying files):\n\
     - Gather full context before editing (read files, check imports, understand patterns)\n\
     - Make changes, then verify (check for errors, run tests if available)\n\
     - Show the result, not the process\n\
     \n\
     **Answering questions** (explaining, analyzing):\n\
     - Answer directly and concisely\n\
     - Use code examples only when they clarify the explanation\n\
     - Cite specific files/lines when referencing the codebase\n\
     \n\
     **Debugging** (fixing errors, investigating issues):\n\
     - Reproduce the issue first (read error messages, check logs)\n\
     - Trace the root cause before applying fixes\n\
     - Verify the fix resolves the original issue\n\
     \n\
     **Exploring/reviewing** (code review, architecture analysis):\n\
     - Start broad, then drill into specifics\n\
     - Use structured output (severity levels, categories)\n\
     - Provide actionable recommendations, not just observations\n\
     </task_strategy>"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_strategy() {
        let section = build();
        assert!(section.contains("<task_strategy>"));
        assert!(section.contains("</task_strategy>"));
        assert!(section.contains("Writing code"));
        assert!(section.contains("Answering questions"));
        assert!(section.contains("Debugging"));
        assert!(section.contains("Exploring/reviewing"));
    }
}
