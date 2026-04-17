//! L0 Identity and L1 Essential Story for MemPalace.
//!
//! Manages the always-loaded identity text and auto-generated
//! essential story from top drawers. Separated from the core
//! Palace structure for single-responsibility.

use std::collections::HashMap;

use super::palace::{DrawerEntry, Palace};

/// Maximum character budget for L1 Essential Story (~800 tokens at 4 chars/token).
const L1_MAX_CHARS: usize = 3200;

impl Palace {
    /// Set the L0 identity text.
    pub fn set_identity(&mut self, text: String) {
        self.identity = Some(text);
    }

    /// Get the L0 identity text.
    pub fn identity_text(&self) -> &str {
        self.identity
            .as_deref()
            .unwrap_or("## L0 — IDENTITY\nNo identity configured.")
    }

    /// Generate L1 Essential Story from the highest-weight drawers.
    ///
    /// Pulls top drawers and formats as compact L1 text (~500-800 tokens).
    pub fn generate_l1_essential(&self, drawers: &[DrawerEntry]) -> String {
        if drawers.is_empty() {
            return "## L1 — No memories yet.".to_string();
        }

        // Group by room, take most recent per room
        let mut by_room: HashMap<String, Vec<&DrawerEntry>> = HashMap::new();
        for drawer in drawers {
            by_room
                .entry(drawer.room_id.clone())
                .or_default()
                .push(drawer);
        }

        let mut lines = vec!["## L1 — ESSENTIAL STORY".to_string()];
        let mut total_len = 0;

        for (room, entries) in by_room.iter() {
            let room_line = format!("\n[{}]", room);
            lines.push(room_line.clone());
            total_len += room_line.len();

            // Take last 3 entries per room
            let recent: Vec<&&DrawerEntry> = entries.iter().rev().take(3).collect();
            for entry in recent {
                let snippet = entry.user_input.chars().take(200).collect::<String>();
                let entry_line = format!("  - {}", snippet);
                if total_len + entry_line.len() > L1_MAX_CHARS {
                    lines.push("  ... (more in L3 search)".to_string());
                    return lines.join("\n");
                }
                lines.push(entry_line.clone());
                total_len += entry_line.len();
            }
        }

        lines.join("\n")
    }

    /// Generate wake-up text: L0 (identity) + L1 (essential story).
    pub fn wake_up(&self, drawers: &[DrawerEntry]) -> String {
        let mut parts = Vec::new();
        parts.push(self.identity_text().to_string());
        parts.push(String::new());
        parts.push(self.generate_l1_essential(drawers));
        parts.join("\n")
    }
}
