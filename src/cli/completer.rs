use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::validate::Validator;
use rustyline::{Context, Helper};

use super::commands::SLASH_COMMANDS;

/// A rustyline helper that provides tab-completion and inline hints
/// for Daedalus slash commands.
#[derive(Default)]
pub struct SlashCommandHelper;

impl SlashCommandHelper {
    pub fn new() -> Self {
        Self
    }

    /// Find all slash commands that start with the given prefix.
    fn matching_commands(prefix: &str) -> Vec<(&'static str, &'static str)> {
        SLASH_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix))
            .copied()
            .collect()
    }
}

// ── Completer ──

impl Completer for SlashCommandHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Only complete if the line starts with '/'
        if !line.starts_with('/') {
            return Ok((0, Vec::new()));
        }

        let prefix = &line[..pos];
        let matches = Self::matching_commands(prefix);

        let pairs: Vec<Pair> = matches
            .into_iter()
            .map(|(cmd, desc)| Pair {
                display: format!("{:<12} {}", cmd, desc),
                replacement: cmd.to_string(),
            })
            .collect();

        // Replace from position 0 (the entire input so far)
        Ok((0, pairs))
    }
}

// ── Hinter (inline ghost-text hint) ──

/// A simple hint that shows the rest of a matching command in dim text.
pub struct CommandHint {
    /// The text to display (the suffix after what the user has typed).
    display: String,
}

impl Hint for CommandHint {
    fn display(&self) -> &str {
        &self.display
    }

    fn completion(&self) -> Option<&str> {
        Some(&self.display)
    }
}

impl Hinter for SlashCommandHelper {
    type Hint = CommandHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<CommandHint> {
        if !line.starts_with('/') || pos == 0 {
            return None;
        }

        let prefix = &line[..pos];
        let matches = Self::matching_commands(prefix);

        // Only show a hint if there's exactly one match and it's not already complete
        if matches.len() == 1 {
            let (cmd, _) = matches[0];
            if cmd != prefix {
                return Some(CommandHint {
                    display: cmd[pos..].to_string(),
                });
            }
        }

        None
    }
}

// ── Highlighter ──

impl Highlighter for SlashCommandHelper {
    /// Render the hint suffix in dim style so it stands out from user input.
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        // ANSI: \x1b[2m = dim, \x1b[0m = reset
        std::borrow::Cow::Owned(format!("\x1b[2m{}\x1b[0m", hint))
    }
}

// ── Validator ──

impl Validator for SlashCommandHelper {}
impl Helper for SlashCommandHelper {}
