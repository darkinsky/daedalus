use std::path::Path;

use anyhow::{Context, Result};

use crate::memory::persistence::{MemoryPersistence, atomic_write};

use super::palace::{ClosetEntry, DrawerEntry, Palace};
use super::wal::WriteAheadLog;

/// Persistent store for MemPalace data.
///
/// Manages four persistence layers:
/// - `palace.json`: Palace metadata (wings, rooms, tunnels, KG triples, entities, diary)
/// - `drawers.jsonl`: Append-only verbatim conversation entries
/// - `closets.json`: Compressed summaries pointing to drawer entries
/// - `identity.txt`: L0 Identity text (optional)
///
/// Also maintains a WAL (Write-Ahead Log) for audit trail.
///
/// Hall entries are stored in ChromaDB (not on disk), so they are
/// managed by the Retriever, not the Store.
pub struct MemPalaceStore {
    /// The palace structure (wings, rooms, tunnels, KG, entities, diary).
    pub(super) palace: Palace,
    /// Verbatim drawer entries (append-only).
    pub(super) drawers: Vec<DrawerEntry>,
    /// Compressed closet summaries.
    pub(super) closets: Vec<ClosetEntry>,
    /// Write-ahead log for audit trail.
    wal: Option<WriteAheadLog>,
}

impl MemPalaceStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self {
            palace: Palace::new(),
            drawers: Vec::new(),
            closets: Vec::new(),
            wal: None,
        }
    }

    /// Create a store with WAL enabled.
    #[allow(dead_code)]
    pub fn with_wal(mut self, wal_dir: &Path) -> Self {
        self.wal = Some(WriteAheadLog::new(wal_dir, true));
        self
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.palace.is_empty() && self.drawers.is_empty()
    }

    /// Add a closet entry.
    pub fn add_closet(&mut self, entry: ClosetEntry) {
        self.closets.push(entry);
    }

    /// Get closet summaries for a specific room.
    #[allow(dead_code)]
    pub fn closets_for_room(&self, wing_id: &str, room_id: &str) -> Vec<&ClosetEntry> {
        self.closets
            .iter()
            .filter(|c| c.wing_id == wing_id && c.room_id == room_id)
            .collect()
    }

    /// Get IDs of drawers that have already been summarized into closets
    /// for a specific room.
    pub fn closeted_drawer_ids(&self, wing_id: &str, room_id: &str) -> std::collections::HashSet<uuid::Uuid> {
        self.closets
            .iter()
            .filter(|c| c.wing_id == wing_id && c.room_id == room_id)
            .flat_map(|c| c.source_drawer_ids.iter().copied())
            .collect()
    }

    /// Add a drawer entry.
    pub fn add_drawer(&mut self, entry: DrawerEntry) {
        // WAL log
        if let Some(ref wal) = self.wal {
            wal.log_simple("add_drawer", "wing", &entry.wing_id);
        }
        // Update room counts via Palace accessor
        if let Some(wing) = self.palace.wing_mut(&entry.wing_id) {
            if let Some(room) = wing.rooms.get_mut(&entry.room_id) {
                room.drawer_count += 1;
                room.touch();
            }
        }
        self.drawers.push(entry);
    }

    /// Increment hall count for a room.
    pub fn increment_hall_count(&mut self, wing_id: &str, room_id: &str) {
        if let Some(wing) = self.palace.wing_mut(wing_id) {
            if let Some(room) = wing.rooms.get_mut(room_id) {
                room.hall_count += 1;
            }
        }
    }

    /// Get all existing wing IDs.
    pub fn wing_ids(&self) -> Vec<String> {
        self.palace.wing_ids()
    }

    /// Get all existing room IDs across all wings.
    pub fn room_ids(&self) -> Vec<String> {
        self.palace.room_ids()
    }

    /// Get drawer entries for a specific room.
    #[allow(dead_code)]
    pub fn drawers_for_room(&self, wing_id: &str, room_id: &str) -> Vec<&DrawerEntry> {
        self.drawers
            .iter()
            .filter(|d| d.wing_id == wing_id && d.room_id == room_id)
            .collect()
    }

    /// Get drawer count for a specific room.
    #[allow(dead_code)]
    pub fn drawer_count_for_room(&self, wing_id: &str, room_id: &str) -> usize {
        self.drawers
            .iter()
            .filter(|d| d.wing_id == wing_id && d.room_id == room_id)
            .count()
    }

    /// Get all drawer contents for dedup checking.
    pub fn all_drawer_contents(&self) -> Vec<String> {
        self.drawers
            .iter()
            .map(|d| format!("{} {}", d.user_input, d.assistant_response))
            .collect()
    }

    /// Get total drawer count.
    pub fn total_drawers(&self) -> usize {
        self.drawers.len()
    }

    /// Get total closet count.
    #[allow(dead_code)]
    pub fn total_closets(&self) -> usize {
        self.closets.len()
    }

    /// Get un-closeted drawers for a specific room.
    ///
    /// Returns drawer entries that have NOT yet been summarized into a closet.
    /// Encapsulates the closet-checking logic so callers don't need to access
    /// raw drawers directly.
    pub fn uncloseted_drawers(&self, wing_id: &str, room_id: &str) -> Vec<&DrawerEntry> {
        let closeted_ids = self.closeted_drawer_ids(wing_id, room_id);
        self.drawers
            .iter()
            .filter(|d| d.wing_id == wing_id && d.room_id == room_id)
            .filter(|d| !closeted_ids.contains(&d.id))
            .collect()
    }

    /// Load L0 identity from file.
    fn load_identity(dir: &Path) -> Option<String> {
        let path = dir.join("identity.txt");
        if path.exists() {
            std::fs::read_to_string(&path).ok().map(|s| s.trim().to_string())
        } else {
            None
        }
    }

    /// Save L0 identity to file.
    fn save_identity(&self, dir: &Path) -> Result<()> {
        if let Some(ref identity) = self.palace.identity {
            let path = dir.join("identity.txt");
            atomic_write(&path, identity.as_bytes())
                .with_context(|| format!("Failed to write identity to: {}", path.display()))?;
        }
        Ok(())
    }

    /// Save palace metadata to a JSON file.
    fn save_palace(&self, dir: &Path) -> Result<()> {
        let path = dir.join("palace.json");
        let json = serde_json::to_string_pretty(&self.palace)
            .context("Failed to serialize palace")?;
        atomic_write(&path, json.as_bytes())
            .with_context(|| format!("Failed to write palace to: {}", path.display()))?;
        Ok(())
    }

    /// Save drawer entries to a JSONL file (append-only format).
    fn save_drawers(&self, dir: &Path) -> Result<()> {
        let path = dir.join("drawers.jsonl");
        let lines: Vec<String> = self.drawers
            .iter()
            .map(|d| serde_json::to_string(d))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to serialize drawer entries")?;
        let content = lines.join("\n");
        atomic_write(&path, content.as_bytes())
            .with_context(|| format!("Failed to write drawers to: {}", path.display()))?;
        Ok(())
    }

    /// Save closet entries to a JSON file.
    fn save_closets(&self, dir: &Path) -> Result<()> {
        if self.closets.is_empty() {
            return Ok(());
        }
        let path = dir.join("closets.json");
        let json = serde_json::to_string_pretty(&self.closets)
            .context("Failed to serialize closet entries")?;
        atomic_write(&path, json.as_bytes())
            .with_context(|| format!("Failed to write closets to: {}", path.display()))?;
        Ok(())
    }

    /// Load palace metadata from a JSON file.
    fn load_palace(dir: &Path) -> Result<Palace> {
        let path = dir.join("palace.json");
        if !path.exists() {
            return Ok(Palace::new());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read palace from: {}", path.display()))?;
        let palace: Palace = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse palace from: {}", path.display()))?;
        Ok(palace)
    }

    /// Load drawer entries from a JSONL file.
    fn load_drawers(dir: &Path) -> Result<Vec<DrawerEntry>> {
        let path = dir.join("drawers.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read drawers from: {}", path.display()))?;
        let drawers: Vec<DrawerEntry> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line))
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("Failed to parse drawers from: {}", path.display()))?;
        Ok(drawers)
    }

    /// Load closet entries from a JSON file.
    fn load_closets(dir: &Path) -> Result<Vec<ClosetEntry>> {
        let path = dir.join("closets.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read closets from: {}", path.display()))?;
        let closets: Vec<ClosetEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse closets from: {}", path.display()))?;
        Ok(closets)
    }
}

impl Default for MemPalaceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MemPalaceStore {
    fn clone(&self) -> Self {
        Self {
            palace: self.palace.clone(),
            drawers: self.drawers.clone(),
            closets: self.closets.clone(),
            wal: None, // WAL is not cloned
        }
    }
}

impl MemoryPersistence for MemPalaceStore {
    fn save(&self, path: &Path) -> Result<()> {
        // `path` is the mempalace directory
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create mempalace dir: {}", path.display()))?;

        self.save_palace(path)?;
        self.save_drawers(path)?;
        self.save_closets(path)?;
        self.save_identity(path)?;

        tracing::debug!(
            path = %path.display(),
            wings = self.palace.wing_count(),
            drawers = self.drawers.len(),
            closets = self.closets.len(),
            triples = self.palace.triple_count(),
            entities = self.palace.entity_count(),
            diary = self.palace.diary_count(),
            "MemPalaceStore saved"
        );
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::debug!(path = %path.display(), "No MemPalace directory found, using default");
            return Ok(Self::new());
        }

        let mut palace = Self::load_palace(path)?;
        let drawers = Self::load_drawers(path)?;
        let closets = Self::load_closets(path)?;

        // Load L0 identity
        if let Some(identity) = Self::load_identity(path) {
            palace.set_identity(identity);
        }

        tracing::info!(
            path = %path.display(),
            wings = palace.wing_count(),
            drawers = drawers.len(),
            closets = closets.len(),
            triples = palace.triple_count(),
            entities = palace.entity_count(),
            diary = palace.diary_count(),
            "MemPalaceStore loaded from disk"
        );

        Ok(Self { palace, drawers, closets, wal: None })
    }
}
