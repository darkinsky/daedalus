
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
     - *Complex* (architecture change, large refactor, 5+ files): Call `create_plan` to \
     create a tracked execution plan before executing. This externalizes your plan into \
     a persistent state that prevents goal drift across rounds.\n\
     - **Iterate on failure**: If something doesn't work, diagnose the root cause before \
     switching tactics. Don't retry with identical arguments — change your approach. \
     Escalate to the user only when genuinely stuck after 2-3 attempts.\n\
     - **Stay focused**: Address the user's actual request.\n\
       - DO: Fix the reported bug, implement the requested feature, answer the question.\n\
       - DON'T: Also reorganize imports, rename variables, add comments to unchanged code, \
     or suggest \"while we're at it\" improvements.\n\
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
