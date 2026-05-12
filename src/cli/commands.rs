/// All supported slash commands with their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show this help message (aliases: /h, /?)"),
    ("/new", "Start a new conversation session"),
    ("/compact", "Compress conversation history (usage: /compact [instruction] | /compact --before N | /compact --after N)"),
    ("/clear", "Clear the screen (keep conversation history)"),
    ("/cost", "Show token usage for the current session"),
    ("/model", "Show current model information"),
    ("/tools", "List available MCP tools"),
    ("/skills", "List available skills"),
    ("/agents", "List available subagents"),
    ("/permissions", "Show permission rules (aliases: /perms)"),
    ("/exit", "Exit the application (alias: /quit)"),
];

/// Parsed result of a slash command.
pub enum Command<'a> {
    Exit,
    Help,
    NewSession,
    /// Context compression with an optional focus instruction and optional range.
    /// The range is `(start, end)` — inclusive start, exclusive end.
    Compact {
        instruction: Option<&'a str>,
        range: Option<(usize, usize)>,
    },
    Clear,
    Cost,
    Model,
    Tools,
    Skills,
    Agents,
    Permissions,
    Unknown(&'a str),
}

/// Try to parse a slash command from user input.
///
/// Returns `Some(Command)` if the input starts with `/`, otherwise `None`.
pub fn parse(input: &str) -> Option<Command<'_>> {
    if !input.starts_with('/') {
        return None;
    }

    let lower = input.to_lowercase();

    // Handle /compact with optional trailing instruction or range flags
    if lower.starts_with("/compact") {
        let rest = input["/compact".len()..].trim();

        // Parse --before N: compress messages 0..N
        if let Some(n_str) = rest.strip_prefix("--before").map(|s| s.trim()) {
            if let Ok(n) = n_str.split_whitespace().next().unwrap_or("").parse::<usize>() {
                return Some(Command::Compact { instruction: None, range: Some((0, n)) });
            }
        }

        // Parse --after N: compress messages N..end (end determined by compact)
        if let Some(n_str) = rest.strip_prefix("--after").map(|s| s.trim()) {
            if let Ok(n) = n_str.split_whitespace().next().unwrap_or("").parse::<usize>() {
                // Use usize::MAX as sentinel — compact will clamp to actual length
                return Some(Command::Compact { instruction: None, range: Some((n, usize::MAX)) });
            }
        }

        // Parse --range M-N: compress messages M..N
        if let Some(range_str) = rest.strip_prefix("--range").map(|s| s.trim()) {
            let parts: Vec<&str> = range_str.split('-').collect();
            if parts.len() == 2 {
                if let (Ok(start), Ok(end)) = (parts[0].trim().parse::<usize>(), parts[1].trim().parse::<usize>()) {
                    return Some(Command::Compact { instruction: None, range: Some((start, end)) });
                }
            }
        }

        let instruction = if rest.is_empty() { None } else { Some(rest) };
        return Some(Command::Compact { instruction, range: None });
    }

    Some(match lower.as_str() {
        "/exit" | "/quit" => Command::Exit,
        "/help" | "/h" | "/?" => Command::Help,
        "/new" => Command::NewSession,
        "/clear" => Command::Clear,
        "/cost" => Command::Cost,
        "/model" => Command::Model,
        "/tools" => Command::Tools,
        "/skills" => Command::Skills,
        "/agents" => Command::Agents,
        "/permissions" | "/perms" => Command::Permissions,
        _ => Command::Unknown(input),
    })
}
