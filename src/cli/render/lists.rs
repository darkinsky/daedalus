//! `/tools` / `/skills` / `/agents` slash-command renderers.
//!
//! All three share the same structural pattern (dim header, colored name,
//! dim description footer) but their data sources are independent. Extracted
//! from the main `cli::render` file so each new list view does not bloat
//! the top-level module.

use crossterm::style::{Attribute, Color, Stylize};

use crate::agent::AgentMetadata;

use super::print_dim;

/// Print the `/tools` output — list all available MCP tools.
pub(in crate::cli) fn tools_list(agent: &dyn AgentMetadata) {
    println!();
    if !agent.has_tools() {
        print_dim("  No MCP tools available.");
        print_dim("  Configure MCP servers in mcp.json to enable tool calling.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available MCP tools ({}):", agent.tool_count())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for tool in agent.tool_infos() {
        println!(
            "    {}  {}",
            tool.name.with(Color::Cyan),
            format!("({})", tool.source).with(Color::DarkGrey),
        );
        if !tool.description.is_empty() {
            print_dim(&format!("      {}", tool.description));
        }
    }

    println!();
    print_dim("    The LLM will automatically use these tools when needed.");
    println!();
}

/// Print the `/skills` output — list all available skills.
pub(in crate::cli) fn skills_list(agent: &dyn AgentMetadata) {
    println!();
    let infos = agent.skill_infos();
    if infos.is_empty() {
        print_dim("  No skills available.");
        print_dim("  Place .md files in the ./skills/ directory to add skills.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available skills ({}):", infos.len())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for skill in &infos {
        println!("    {}", skill.name.as_str().with(Color::Cyan));
        if !skill.description.is_empty() {
            print_dim(&format!("      {}", skill.description));
        }
    }

    println!();
    print_dim("    The LLM will automatically invoke skills via the use_skill tool when needed.");
    println!();
}

/// Print the `/agents` output — list all available subagents.
pub(in crate::cli) fn agents_list(agent: &dyn AgentMetadata) {
    println!();
    let infos = agent.subagent_infos();
    if infos.is_empty() {
        print_dim("  No subagents available.");
        print_dim("  Place .md files in the .daedalus/agents/ directory to add subagents.");
        println!();
        return;
    }

    println!(
        "{}",
        format!("  Available subagents ({}):", infos.len())
            .with(Color::White)
            .attribute(Attribute::Bold)
    );
    println!();

    for info in &infos {
        println!(
            "    {}  {}",
            info.name.as_str().with(Color::Cyan),
            format!("({})", info.source).with(Color::DarkGrey),
        );
        if !info.description.is_empty() {
            let first_line = info.description.lines().next().unwrap_or("").trim();
            print_dim(&format!("      {}", first_line));
        }
    }

    println!();
    print_dim(
        "    The LLM will automatically spawn subagents via the spawn_subagent tool when needed.",
    );
    println!();
}
