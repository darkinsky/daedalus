use anyhow::Result;

use crate::agent::Mode;

/// Print session info banner.
fn print_session_info(agent: &dyn Mode) {
    let session = agent.session();
    println!("  📋 Session: {} ({})", session.title, &session.id[..8]);
}

/// Run an interactive REPL loop for the given mode.
pub async fn run_interactive(agent: &mut dyn Mode) -> Result<()> {
    use std::io::{self, BufRead, Write};

    println!("╔══════════════════════════════════════════╗");
    println!("║       🏛️  Daedalus Agent v0.1.0          ║");
    println!("║  Type your message or 'quit' to exit     ║");
    println!("║  Type '/new' to start a new session      ║");
    println!("╚══════════════════════════════════════════╝");
    println!();
    println!(
        "  Provider: {}  |  Model: {}  |  Mode: {}",
        agent.provider_name(),
        agent.model_name(),
        agent.mode_name()
    );
    print_session_info(agent);
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let request_id = agent.session().request_id + 1;
        print!("[{}] You > ", request_id);
        stdout.flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            println!("Goodbye! 👋");
            break;
        }

        // Handle slash commands
        if input.eq_ignore_ascii_case("/new") {
            agent.new_session();
            println!();
            println!("✨ New session started!");
            print_session_info(agent);
            println!();
            continue;
        }

        tracing::debug!("User input: {}", input);

        match agent.chat(input).await {
            Ok(answer) => {
                println!();
                println!("Daedalus > {}", answer);
                println!();
            }
            Err(e) => {
                tracing::error!("Agent error: {}", e);
                println!();
                println!("⚠️  Error: {}", e);
                println!();
            }
        }
    }

    Ok(())
}
