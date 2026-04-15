use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};
use rustyline::error::ReadlineError;
use rustyline::{CompletionType, Config, EditMode, Editor};

use crate::agent::{AgentMode, ToolEventCallback};
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
        Command::Skills => render::skills_list(agent),
        Command::Agents => render::agents_list(agent),
        Command::Unknown(raw) => render::unknown_command(raw),
    }
    Ok(false)
}

/// Build the tool event callback that renders tool progress to the terminal.
///
/// The callback clears the spinner before printing tool events, then
/// restarts it for the next LLM thinking phase.
fn build_tool_event_callback(spinner: &Arc<indicatif::ProgressBar>) -> ToolEventCallback {
    let spinner = Arc::clone(spinner);
    Arc::new(move |event| {
        // Pause the spinner so tool output is not interleaved
        spinner.finish_and_clear();
        render::tool_event(&event);
        // Restart spinner for the next LLM round
        spinner.set_message("Thinking…");
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    })
}

/// Send user input to the agent and render the response.
async fn handle_chat(input: &str, agent: &mut dyn AgentMode, cost: &mut SessionCost) {
    tracing::debug!("User input: {}", input);

    // Show spinner while waiting for LLM response
    let spinner = Arc::new(render::spinner());
    let start = Instant::now();

    // Build tool event callback for real-time tool progress display
    let tool_callback = build_tool_event_callback(&spinner);

    // Set the subagent event callback so subagent tool events are also rendered
    agent.set_subagent_event_callback(Some(Arc::clone(&tool_callback)));

    match agent.chat(input, Some(&tool_callback)).await {
        Ok(result) => {
            let elapsed = start.elapsed().as_secs_f64();
            spinner.finish_and_clear();

            // Clear the subagent event callback
            agent.set_subagent_event_callback(None);

            // Show reasoning/thinking process if present
            if let Some(ref reasoning) = result.reasoning_content
                && !reasoning.is_empty()
            {
                render::reasoning_content(reasoning);
            }

            render::response(&result.content);

            // Track token usage for the session
            cost.add_usage(result.usage.as_ref());

            render::response_footer(result.usage.as_ref(), elapsed);
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();

            // Clear the subagent event callback
            agent.set_subagent_event_callback(None);

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

    // Persist memory and perform cleanup before exiting
    if let Err(e) = agent.shutdown().await {
        tracing::error!(error = %e, "Failed to shutdown agent cleanly");
    }

    Ok(())
}
