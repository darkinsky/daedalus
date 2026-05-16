//! Interactive REPL (Read-Eval-Print Loop).
//!
//! ## Module structure
//!
//! - `chat`         — Chat turn execution (text + multimodal)
//! - `confirmation` — Interactive tool confirmation UI
//! - `streaming`    — Streaming state machine and tool event callback
//! - `image`        — Image file loading and encoding

mod chat;
mod confirmation;
mod image;
mod streaming;

use std::sync::{Arc, Mutex};

use anyhow::Result;
use crossterm::style::{Attribute, Color, Stylize};
use rustyline::error::ReadlineError;
use rustyline::{CompletionType, Config, EditMode, Editor};

use crate::agent::AgentMode;
use crate::middleware::builtin::confirmation::{self as mw_confirmation, ConfirmationReceiver};
use crate::middleware::builtin::cost::{SessionCost, SharedSessionCost};
use crate::middleware::builtin::permission_rules::PermissionRuleSet;

use super::commands::{self, Command};
use super::completer::SlashCommandHelper;
use super::render;

use chat::{handle_chat, handle_chat_with_message};
use image::load_image_as_base64;

/// Handle a parsed slash command. Returns `true` if the REPL should exit.
async fn handle_command(
    cmd: Command<'_>,
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) -> Result<bool> {
    match cmd {
        Command::Exit => {
            render::goodbye();
            return Ok(true);
        }
        Command::Help => render::help(),
        Command::NewSession => {
            agent.new_session();
            if let Ok(mut c) = cost.lock() {
                c.reset();
            }
            render::new_session(agent);
        }
        Command::Compact { instruction, range } => {
            handle_compact(agent, instruction, range).await;
        }
        Command::Clear => {
            print!("\x1B[2J\x1B[1;1H");
            std::io::Write::flush(&mut std::io::stdout())?;
            render::screen_cleared(agent);
        }
        Command::Cost => {
            if let Ok(c) = cost.lock() {
                render::cost(&c);
            }
        }
        Command::Model => render::model_info(agent),
        Command::Tools => render::tools_list(agent),
        Command::Skills => render::skills_list(agent),
        Command::Agents => render::agents_list(agent),
        Command::Permissions => handle_permissions(agent),
        Command::Undo => handle_undo().await,
        Command::Plan => handle_plan(agent),
        Command::Skip => handle_skip(agent),
        Command::Image { path } => {
            // Image handling is done in the REPL loop, not here.
            // This branch should not be reached — the REPL loop intercepts it.
            println!(
                "  {} Image queued: {}",
                "📎".with(Color::Cyan),
                path.as_str().with(Color::Grey),
            );
            println!();
        }
        Command::Resume => {
            handle_resume(agent, cost, confirm_rx).await;
        }
        Command::Context => {
            let messages = agent.context_messages();
            let analysis = super::context_analysis::analyze(
                &messages,
                agent.context_window(),
                agent.tool_count(),
            );
            render::context_usage(&analysis);
        }
        Command::Unknown(raw) => render::unknown_command(raw),
    }
    Ok(false)
}

// ── Slash command handlers ──

/// Handle the `/permissions` command — display active permission rules.
fn handle_permissions(agent: &dyn AgentMode) {
    let mode = agent.permission_mode_name();

    if let Some(rules_arc) = agent.permission_rules() {
        let rt = tokio::runtime::Handle::current();
        let all_rules: Vec<_> = rt.block_on(async {
            let rules = rules_arc.lock().await;
            rules.all_rules()
                .into_iter()
                .map(|(r, s)| (r.clone(), s))
                .collect()
        });
        render::permissions_list(&all_rules, mode);
    } else {
        let workspace_root = agent.workspace_root();
        let rule_set = PermissionRuleSet::load(workspace_root.as_deref());
        let all_rules: Vec<_> = rule_set.all_rules()
            .into_iter()
            .map(|(r, s)| (r.clone(), s))
            .collect();
        render::permissions_list(&all_rules, mode);
    }
}

/// Handle the `/compact` command — compress conversation history.
async fn handle_compact(agent: &mut dyn AgentMode, instruction: Option<&str>, range: Option<(usize, usize)>) {
    let spinner = Arc::new(render::spinner());
    let msg = if range.is_some() {
        "Compressing partial context\u{2026}"
    } else {
        "Compressing context\u{2026}"
    };
    spinner.set_message(msg);

    let result = if let Some(r) = range {
        agent.compact_range(instruction, r).await
    } else {
        agent.compact(instruction).await
    };

    match result {
        Ok(message) => {
            spinner.finish_and_clear();
            println!(
                "  {} {}",
                "✓".with(Color::Green).attribute(Attribute::Bold),
                message.with(Color::Grey),
            );
            println!();
        }
        Err(e) => {
            spinner.finish_and_clear();
            println!(
                "  {} Compact failed: {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e,
            );
            println!();
        }
    }
}

/// Handle the `/undo` command — restore the last file modification.
async fn handle_undo() {
    use crate::tools::checkpoint;

    match checkpoint::undo().await {
        Ok(message) => {
            println!(
                "  {} {}",
                "↩".with(Color::Green).attribute(Attribute::Bold),
                message.with(Color::Grey),
            );
            println!();
        }
        Err(e) => {
            println!(
                "  {} {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e.to_string().with(Color::Grey),
            );
            println!();
        }
    }
}

/// Handle the `/plan` command — display the current task plan status.
fn handle_plan(agent: &dyn AgentMode) {
    let Some(plan_arc) = agent.shared_plan() else {
        println!(
            "  {} Plan tracking is not available in this mode.",
            "ℹ".with(Color::Blue).attribute(Attribute::Bold),
        );
        println!();
        return;
    };

    match plan_arc.lock() {
        Ok(mgr) => {
            if let Some(plan) = mgr.active_plan() {
                println!();
                for line in plan.format_for_display() {
                    if line.is_empty() {
                        println!();
                    } else {
                        println!("{}", line);
                    }
                }
                println!();
            } else {
                println!(
                    "  {} No active plan. The agent will create one when needed.",
                    "ℹ".with(Color::Blue).attribute(Attribute::Bold),
                );
                println!();
            }
        }
        Err(_) => {
            println!(
                "  {} Failed to read plan state.",
                "✗".with(Color::Red).attribute(Attribute::Bold),
            );
            println!();
        }
    }
}

/// Handle the `/skip` command — skip the current plan step.
fn handle_skip(agent: &dyn AgentMode) {
    let Some(plan_arc) = agent.shared_plan() else {
        println!(
            "  {} Plan tracking is not available in this mode.",
            "ℹ".with(Color::Blue).attribute(Attribute::Bold),
        );
        println!();
        return;
    };

    let mut mgr = match plan_arc.lock() {
        Ok(m) => m,
        Err(_) => {
            println!(
                "  {} Failed to access plan state.",
                "✗".with(Color::Red).attribute(Attribute::Bold),
            );
            println!();
            return;
        }
    };

    match mgr.skip_current() {
        Ok(msg) => {
            println!(
                "  {} {}",
                "⏭️".with(Color::Cyan).attribute(Attribute::Bold),
                msg.with(Color::Grey),
            );
            println!();
        }
        Err(e) => {
            println!(
                "  {} {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e.with(Color::Grey),
            );
            println!();
        }
    }
}

/// Handle the /resume command — check for a saved checkpoint and resume execution.
async fn handle_resume(
    agent: &mut dyn AgentMode,
    cost: &SharedSessionCost,
    confirm_rx: &Arc<tokio::sync::Mutex<ConfirmationReceiver>>,
) {
    use crate::agent::tool_loop::checkpoint::ToolLoopCheckpoint;

    let checkpoint_path = agent.checkpoint_path();
    let checkpoint_path = match checkpoint_path {
        Some(p) => p,
        None => {
            println!(
                "  {} No workspace configured — cannot resume.",
                "✗".with(Color::Red).attribute(Attribute::Bold),
            );
            println!();
            return;
        }
    };

    match ToolLoopCheckpoint::load(&checkpoint_path) {
        Ok(Some(cp)) => {
            println!(
                "  {} Found checkpoint: {}",
                "▶".with(Color::Green).attribute(Attribute::Bold),
                cp.summary().with(Color::Grey),
            );
            println!(
                "  {} Resuming with the original task...",
                "↻".with(Color::Cyan).attribute(Attribute::Bold),
            );
            println!();

            let tool_summary: Vec<String> = cp.restore_tool_history().iter().enumerate().map(|(i, round)| {
                let calls: Vec<String> = round.calls.iter().map(|c| {
                    format!("{}({})", c.function_name, crate::tools::truncate_chars(&c.arguments.to_string(), 100))
                }).collect();
                format!("  Round {}: {}", i + 1, calls.join(", "))
            }).collect();

            let files_read = cp.restore_files_read();
            let files_summary = if files_read.is_empty() {
                String::new()
            } else {
                let mut sorted: Vec<&String> = files_read.iter().collect();
                sorted.sort();
                format!("\nFiles read: {}", sorted.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect::<Vec<_>>().join(", "))
            };

            let resume_prompt = format!(
                "[RESUMING INTERRUPTED TASK]\n\
                 Original task: {}\n\
                 Progress: completed {} rounds with {} tool calls.{}\n\
                 Tool call history:\n{}\n\n\
                 Continue from where you left off. Do NOT repeat work that was already done. \
                 Review the files that were already read and pick up the task from the point of interruption.",
                cp.user_input,
                cp.last_round,
                cp.total_tool_calls,
                files_summary,
                tool_summary.join("\n"),
            );

            ToolLoopCheckpoint::clear(&checkpoint_path);
            handle_chat(&resume_prompt, agent, cost, confirm_rx).await;
        }
        Ok(None) => {
            println!(
                "  {} No checkpoint found — nothing to resume.",
                "ℹ".with(Color::Blue).attribute(Attribute::Bold),
            );
            println!();
        }
        Err(e) => {
            println!(
                "  {} Failed to load checkpoint: {}",
                "✗".with(Color::Red).attribute(Attribute::Bold),
                e.to_string().with(Color::Grey),
            );
            println!();
        }
    }
}

// ── Main REPL loop ──

/// Run an interactive REPL loop in Claude Code style.
pub async fn run(agent: &mut dyn AgentMode) -> Result<()> {
    let cost: SharedSessionCost = agent
        .session_cost()
        .cloned()
        .unwrap_or_else(|| Arc::new(Mutex::new(SessionCost::new())));

    let (confirm_tx, confirm_rx) = mw_confirmation::confirmation_channel();
    agent.set_confirmation_sender(confirm_tx);
    let confirm_rx = Arc::new(tokio::sync::Mutex::new(confirm_rx));

    render::banner(agent);

    // Trigger SessionStart lifecycle hooks
    if let Some(hooks_config) = agent.hooks_config() {
        let session_id = agent.session_id().to_string();
        crate::hooks::run_session_start_hooks(hooks_config, &session_id).await;
    }

    // Configure rustyline with tab-completion support
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .auto_add_history(true)
        .build();

    let helper = SlashCommandHelper::new();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let prompt = format!("{} ", ">".with(Color::Blue).attribute(Attribute::Bold));
    let mut pending_image: Option<String> = None;

    loop {
        let readline = rl.readline(&prompt);

        match readline {
            Ok(line) => {
                let input = line.trim();

                if input.is_empty() {
                    continue;
                }

                // Handle slash commands
                if let Some(cmd) = commands::parse(input) {
                    // Special handling for /image — store the path for the next message
                    if let commands::Command::Image { ref path } = cmd {
                        match load_image_as_base64(path) {
                            Ok((media_type, _data)) => {
                                pending_image = Some(path.clone());
                                println!(
                                    "  {} Image attached: {} ({}). Type your message to send it.",
                                    "📎".with(Color::Cyan).attribute(Attribute::Bold),
                                    path.as_str().with(Color::Grey),
                                    media_type.as_str().with(Color::DarkGrey),
                                );
                                println!();
                            }
                            Err(e) => {
                                println!(
                                    "  {} Failed to load image: {}",
                                    "✗".with(Color::Red).attribute(Attribute::Bold),
                                    e.to_string().with(Color::Grey),
                                );
                                println!();
                            }
                        }
                        continue;
                    }

                    if handle_command(cmd, agent, &cost, &confirm_rx).await? {
                        break;
                    }
                    continue;
                }

                // Handle quit/exit without slash
                if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
                    render::goodbye();
                    break;
                }

                // Chat with the agent (with optional image attachment)
                if let Some(ref image_path) = pending_image.take() {
                    match load_image_as_base64(image_path) {
                        Ok((media_type, data)) => {
                            let image_source = crate::llm::ImageSource::Base64 {
                                media_type,
                                data,
                            };
                            let msg = crate::llm::ChatMessage::user_with_image(input, image_source);
                            handle_chat_with_message(msg, agent, &cost, &confirm_rx).await;
                        }
                        Err(e) => {
                            println!(
                                "  {} Failed to load image: {}. Sending text only.",
                                "⚠".with(Color::Yellow),
                                e.to_string().with(Color::Grey),
                            );
                            handle_chat(input, agent, &cost, &confirm_rx).await;
                        }
                    }
                } else {
                    handle_chat(input, agent, &cost, &confirm_rx).await;
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!();
                continue;
            }
            Err(ReadlineError::Eof) => {
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
