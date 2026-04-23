/// All supported slash commands with their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show this help message (aliases: /h, /?)"),
    ("/new", "Start a new conversation session"),
    ("/compact", "Compress conversation history to save context (usage: /compact [focus instruction])"),
    ("/clear", "Clear the screen (keep conversation history)"),
    ("/cost", "Show token usage for the current session"),
    ("/model", "Show current model information"),
    ("/tools", "List available MCP tools"),
    ("/skills", "List available skills"),
    ("/agents", "List available subagents"),
    ("/exit", "Exit the application (alias: /quit)"),
];

/// Parsed result of a slash command.
pub enum Command<'a> {
    Exit,
    Help,
    NewSession,
    /// Context compression with an optional focus instruction.
    Compact(Option<&'a str>),
    Clear,
    Cost,
    Model,
    Tools,
    Skills,
    Agents,
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

    // Handle /compact with optional trailing instruction
    if lower.starts_with("/compact") {
        let rest = input["/compact".len()..].trim();
        let instruction = if rest.is_empty() { None } else { Some(rest) };
        return Some(Command::Compact(instruction));
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
        _ => Command::Unknown(input),
    })
}
