//! Streaming response rendering — real-time LLM output display.
//!
//! Extracted from `render/mod.rs` to reduce file size and improve
//! separation of concerns. These functions handle the incremental
//! display of LLM responses during streaming mode.

use chrono::Local;
use crossterm::style::{Attribute, Color, Stylize};

/// Print the streaming response header (before any chunks arrive).
pub fn stream_response_header() {
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {}",
        "🤖 Response".with(Color::Blue).attribute(Attribute::Bold),
        format!("[{}]", ts).with(Color::DarkGrey),
    );
    // Print the indent prefix for the first line of streaming content
    print!("  ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Print a streaming text chunk (no newline, no markdown processing).
///
/// This is called for each `StreamText` event during streaming mode.
/// The text is printed raw (without markdown rendering) for real-time
/// display. The full response will be re-rendered with markdown after
/// streaming completes.
pub fn stream_text_chunk(text: &str) {
    use std::io::Write;
    // Handle newlines in the chunk: indent continuation lines
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            print!("\n  ");
        }
        print!("{}", line);
        first = false;
    }
    let _ = std::io::stdout().flush();
}

/// Print a streaming reasoning chunk.
pub fn stream_reasoning_chunk(text: &str) {
    use std::io::Write;
    print!("{}", text.with(Color::DarkCyan));
    let _ = std::io::stdout().flush();
}

/// Print the streaming reasoning header.
pub fn stream_reasoning_header() {
    let ts = Local::now().format("%H:%M:%S");
    println!();
    println!(
        "  {} {} {}",
        "💭".to_string(),
        "Reasoning:".with(Color::DarkBlue).attribute(Attribute::Italic),
        format!("[{}]", ts).with(Color::DarkGrey),
    );
    print!("  {}  ", "┊".with(Color::DarkBlue));
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Finish the streaming output (print a final newline).
pub fn stream_done() {
    println!();
}
