use anyhow::Result;

use crate::agent::Agent;

/// Run an interactive REPL loop for the agent.
pub async fn run_interactive(agent: &mut Agent) -> Result<()> {
    use std::io::{self, BufRead, Write};

    println!("╔══════════════════════════════════════════╗");
    println!("║       🏛️  Daedalus Agent v0.1.0          ║");
    println!("║  Type your message or 'quit' to exit     ║");
    println!("╚══════════════════════════════════════════╝");
    println!();
    println!(
        "  Provider: {}  |  Model: {}",
        agent.provider_name(),
        agent.model_name()
    );
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("You > ");
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
