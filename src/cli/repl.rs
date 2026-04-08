use std::io::{self, BufRead, Write};
use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};

use crate::agent::AgentMode;
use super::commands::{self, Command};
use super::cost::SessionCost;
use super::render;

/// Run an interactive REPL loop in Claude Code style.
pub async fn run(agent: &mut dyn AgentMode) -> Result<()> {
    let mut cost = SessionCost::new();

    render::banner(agent);

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // Prompt: simple ">" like Claude Code
        print!("{} ", ">".with(Color::Cyan).attribute(Attribute::Bold));
        stdout.flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // ── Handle slash commands ──
        if let Some(cmd) = commands::parse(input) {
            match cmd {
                Command::Exit => {
                    render::goodbye();
                    break;
                }
                Command::Help => render::help(),
                Command::NewSession => {
                    agent.new_session();
                    cost.reset();
                    render::new_session(agent);
                }
                Command::Clear => {
                    print!("\x1B[2J\x1B[1;1H");
                    stdout.flush()?;
                    render::screen_cleared(agent);
                }
                Command::Cost => render::cost(&cost),
                Command::Model => render::model_info(agent),
                Command::Tools => render::tools_list(agent),
                Command::Unknown(raw) => render::unknown_command(raw),
            }
            continue;
        }

        // ── Handle quit/exit without slash ──
        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            render::goodbye();
            break;
        }

        tracing::debug!("User input: {}", input);

        // Show spinner while waiting for LLM response
        let spinner = render::spinner();
        let start = Instant::now();

        match agent.chat(input).await {
            Ok(result) => {
                let elapsed = start.elapsed().as_secs_f64();
                spinner.finish_and_clear();

                render::response(&result.content);

                // Extract token usage from the ChatResponse
                let prompt_tokens = result.usage.as_ref().and_then(|u| u.prompt_tokens);
                let completion_tokens = result.usage.as_ref().and_then(|u| u.completion_tokens);

                cost.add(
                    prompt_tokens.unwrap_or(0),
                    completion_tokens.unwrap_or(0),
                );

                render::response_footer(prompt_tokens, completion_tokens, elapsed);
                println!();
            }
            Err(e) => {
                spinner.finish_and_clear();
                tracing::error!("Agent error: {}", e);
                render::error(&e);
            }
        }
    }

    Ok(())
}
