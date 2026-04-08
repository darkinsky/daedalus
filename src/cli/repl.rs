use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};
use rustyline::error::ReadlineError;
use rustyline::{CompletionType, Config, EditMode, Editor};

use crate::agent::AgentMode;
use super::commands::{self, Command};
use super::completer::SlashCommandHelper;
use super::cost::SessionCost;
use super::render;

/// Handle a parsed slash command. Returns `true` if the REPL should exit.
fn handle_command(cmd: Command<'_>, agent: &mut dyn AgentMode, cost: &mut SessionCost) -> Result<bool> {
    match cmd {
        Command::Exit => {
            render::goodbye();
            return Ok(true);
        }
        Command::Help => render::help(),
        Command::NewSession => {
            agent.new_session();
            cost.reset();
            render::new_session(agent);
        }
        Command::Clear => {
            print!("\x1B[2J\x1B[1;1H");
            std::io::Write::flush(&mut std::io::stdout())?;
            render::screen_cleared(agent);
        }
        Command::Cost => render::cost(cost),
        Command::Model => render::model_info(agent),
        Command::Tools => render::tools_list(agent),
        Command::Unknown(raw) => render::unknown_command(raw),
    }
    Ok(false)
}

/// Send user input to the agent and render the response.
async fn handle_chat(input: &str, agent: &mut dyn AgentMode, cost: &mut SessionCost) {
    tracing::debug!("User input: {}", input);

    // Show spinner while waiting for LLM response
    let spinner = render::spinner();
    let start = Instant::now();

    match agent.chat(input).await {
        Ok(result) => {
            let elapsed = start.elapsed().as_secs_f64();
            spinner.finish_and_clear();

            render::response(&result.content);

            // Track token usage for the session
            let prompt_tokens = result.usage.as_ref().and_then(|u| u.prompt_tokens);
            let completion_tokens = result.usage.as_ref().and_then(|u| u.completion_tokens);

            cost.add(
                prompt_tokens.unwrap_or(0),
                completion_tokens.unwrap_or(0),
            );

            render::response_footer(result.usage.as_ref(), elapsed);
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();
            tracing::error!("Agent error: {}", e);
            render::error(&e);
        }
    }
}

/// Run an interactive REPL loop in Claude Code style.
pub async fn run(agent: &mut dyn AgentMode) -> Result<()> {
    let mut cost = SessionCost::new();

    render::banner(agent);

    // Configure rustyline with tab-completion support
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .auto_add_history(true)
        .build();

    let helper = SlashCommandHelper::new();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let prompt = format!("{} ", ">".with(Color::Cyan).attribute(Attribute::Bold));

    loop {
        let readline = rl.readline(&prompt);

        match readline {
            Ok(line) => {
                let input = line.trim();

                if input.is_empty() {
                    continue;
                }

                // ── Handle slash commands ──
                if let Some(cmd) = commands::parse(input) {
                    if handle_command(cmd, agent, &mut cost)? {
                        break;
                    }
                    continue;
                }

                // ── Handle quit/exit without slash ──
                if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
                    render::goodbye();
                    break;
                }

                // ── Chat with the agent ──
                handle_chat(input, agent, &mut cost).await;
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: just print a new line and continue
                println!();
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D: exit gracefully
                render::goodbye();
                break;
            }
            Err(err) => {
                tracing::error!("Readline error: {}", err);
                render::error(&anyhow::anyhow!("Input error: {}", err));
                break;
            }
        }
    }

    Ok(())
}
