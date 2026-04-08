/// All supported slash commands with their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show this help message"),
    ("/new", "Start a new conversation session"),
    ("/clear", "Clear the screen (keep conversation history)"),
    ("/compact", "Start a new session (clear history)"),
    ("/cost", "Show token usage for the current session"),
    ("/model", "Show current model information"),
    ("/exit", "Exit the application"),
];

/// Parsed result of a slash command.
pub enum Command<'a> {
    Exit,
    Help,
    NewSession,
    Clear,
    Cost,
    Model,
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
    Some(match lower.as_str() {
        "/exit" | "/quit" => Command::Exit,
        "/help" | "/h" | "/?" => Command::Help,
        "/new" | "/compact" => Command::NewSession,
        "/clear" => Command::Clear,
        "/cost" => Command::Cost,
        "/model" => Command::Model,
        _ => Command::Unknown(input),
    })
}
