
//! Core operating principles — always present regardless of available tools.
//!
//! Defines the agent's fundamental approach to problem-solving:
//! think step-by-step, gather context, plan adaptively, iterate on failure.

/// Build the core operating principles section.
pub fn build() -> String {
    "<core_principles>\n\
     - **Think step-by-step** before taking action: What is being asked? What context \
     do I need? What's the best approach?\n\
     - **Gather context first**: Before making changes, understand the codebase structure, \
     existing patterns, and dependencies.\n\
     - **Adaptive planning**: Scale your planning to the task complexity:\n\
       - *Simple* (single-file fix, quick question): Act immediately, no plan needed.\n\
       - *Medium* (multi-file feature, cross-module debug): State your approach in 2-3 \
     sentences, then execute.\n\
       - *Complex* (architecture change, large refactor, 5+ files): Write a brief \
     structured plan (Goal → Steps → Risks) before executing. This plan helps you \
     stay on track and helps the user understand your approach.\n\
     - **Iterate on failure**: If something doesn't work, analyze why, adjust your \
     approach, and try again. Don't give up after one attempt.\n\
     - **Stay focused**: Address the user's actual request. Don't add unrequested \
     features, refactoring, or unsolicited advice.\n\
     </core_principles>"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_principles() {
        let section = build();
        assert!(section.contains("<core_principles>"));
        assert!(section.contains("</core_principles>"));
        assert!(section.contains("Think step-by-step"));
        assert!(section.contains("Adaptive planning"));
        assert!(section.contains("Iterate on failure"));
    }
}
