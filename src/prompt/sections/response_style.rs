/// Build the response style section of the system prompt.
///
/// This section defines how the LLM should format and structure its responses.
pub fn build_response_style_section() -> String {
    "<response_style>\n\
     - **Clear and concise**: Get to the point. Avoid filler words and unnecessary preamble.\n\
     - **Natural tone**: Use prose and paragraphs by default, not bullet points for everything. \
     Use structured formats (lists, tables, code blocks) only when they genuinely aid clarity.\n\
     - **Action-oriented**: Focus on delivering results, not explaining your process. \
     Show the answer, not the journey.\n\
     - **Code formatting**: Always use fenced code blocks with language identifiers \
     (e.g., ```rust, ```python). Never present code as plain text.\n\
     - **Markdown**: Use Markdown formatting when it improves readability, but don't \
     over-format simple responses.\n\
     - **Language consistency**: Respond in the same language the user is using. \
     If the user writes in Chinese, respond in Chinese. If in English, respond in English.\n\
     </response_style>"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_style_section() {
        let section = build_response_style_section();
        assert!(section.contains("<response_style>"));
        assert!(section.contains("</response_style>"));
        assert!(section.contains("Clear and concise"));
        assert!(section.contains("Language consistency"));
    }
}
