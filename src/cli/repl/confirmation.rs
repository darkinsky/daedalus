//! Interactive tool confirmation UI.
//!
//! Displays tool call details and risk level, reads the user's decision.

use crossterm::style::{Attribute, Color, Stylize};

use crate::middleware::builtin::confirmation::{
    ConfirmationRequest, ToolRiskLevel, UserDecision,
};
use crate::middleware::builtin::permission_rules::RuleScope;

/// Prompt the user for confirmation of a tool call.
///
/// Displays the tool call details and risk level, then reads a single
/// character from stdin to determine the user's decision.
pub(crate) fn prompt_user_confirmation(request: &ConfirmationRequest) -> UserDecision {
    use std::io::Write;

    let risk_label = match request.risk_level {
        ToolRiskLevel::Sensitive => "Sensitive".with(Color::Yellow),
        ToolRiskLevel::Dangerous => "Dangerous".with(Color::Red).attribute(Attribute::Bold),
        ToolRiskLevel::ReadOnly => "ReadOnly".with(Color::Green), // shouldn't happen
    };

    let risk_reason = match request.risk_level {
        ToolRiskLevel::Sensitive => "modifies files",
        ToolRiskLevel::Dangerous => "arbitrary execution",
        ToolRiskLevel::ReadOnly => "read-only",
    };

    // Print the confirmation prompt
    println!();
    println!(
        "  {} {} wants to execute:",
        "⚠️".with(Color::Yellow),
        request.tool_name.clone().with(Color::Cyan).attribute(Attribute::Bold),
    );
    println!(
        "  {}  {}",
        "┃".with(Color::DarkGrey),
        request.description.clone().with(Color::White),
    );
    println!(
        "  {}",
        "┃".with(Color::DarkGrey),
    );
    println!(
        "  {}  Risk: {} ({})",
        "┃".with(Color::DarkGrey),
        risk_label,
        risk_reason,
    );
    println!(
        "  {}",
        "┃".with(Color::DarkGrey),
    );

    // Build the options line based on whether we have a suggested pattern
    let pattern_hint = if let Some(ref pattern) = request.suggested_pattern {
        format!(
            "  [{}] Always allow \"{}({})\"  ",
            "a".with(Color::Magenta).attribute(Attribute::Bold),
            request.tool_name,
            pattern,
        )
    } else {
        format!(
            "  [{}] Always allow {}  ",
            "a".with(Color::Magenta).attribute(Attribute::Bold),
            request.tool_name,
        )
    };

    print!(
        "  {}  [{}] Allow once  [{}] Allow for session {}[{}] Deny  > ",
        "┗━".with(Color::DarkGrey),
        "y".with(Color::Green).attribute(Attribute::Bold),
        "s".with(Color::Blue).attribute(Attribute::Bold),
        pattern_hint,
        "n".with(Color::Red).attribute(Attribute::Bold),
    );
    let _ = std::io::stdout().flush();

    // Read user input (single line)
    let decision = read_confirmation_input(request.suggested_pattern.clone());
    println!();
    decision
}

/// Read a single character of confirmation input from the terminal.
///
/// Falls back to "deny" if input cannot be read.
fn read_confirmation_input(suggested_pattern: Option<String>) -> UserDecision {
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(_) => {
            let trimmed = input.trim().to_lowercase();
            match trimmed.as_str() {
                "y" | "yes" | "" => UserDecision::AllowOnce,
                "s" | "session" => UserDecision::AllowSession,
                "a" | "always" => UserDecision::AlwaysAllow {
                    scope: RuleScope::Project,
                    pattern: suggested_pattern,
                },
                "ag" | "always-global" | "global" => UserDecision::AlwaysAllow {
                    scope: RuleScope::Global,
                    pattern: suggested_pattern,
                },
                "n" | "no" | "d" | "deny" => UserDecision::Deny,
                _ => {
                    // Unknown input — treat as deny for safety
                    println!(
                        "  {} Unknown input '{}', denying.",
                        "⚠".with(Color::Yellow),
                        trimmed,
                    );
                    UserDecision::Deny
                }
            }
        }
        Err(_) => {
            // Can't read input — deny for safety
            UserDecision::Deny
        }
    }
}
