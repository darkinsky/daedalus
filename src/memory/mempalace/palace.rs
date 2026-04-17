//! Core data model and spatial structure for MemPalace.
//!
//! Defines the Palace hierarchy: Wings → Rooms → Halls/Drawers/Closets,
//! plus knowledge graph entities/triples, tunnels, and diary entries.
//!
//! Query operations are split into separate modules:
//! - `knowledge_graph.rs`: KG queries, timeline, stats, seeding
//! - `graph.rs`: BFS traversal, tunnel discovery, graph stats
//! - `diary.rs`: Agent diary read/write
//! - `identity.rs`: L0/L1 identity and wake-up text

use std::collections::HashMap;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Hall Types ──

/// Fixed hall types — categorize memories by nature.
///
/// Inspired by MemPalace's spatial architecture, halls are fixed-type
/// corridors within a room that classify memories by their nature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HallType {
    /// Factual knowledge ("Postgres is used for auth")
    Facts,
    /// Temporal events ("Migrated auth on 2026-04-10")
    Events,
    /// Insights and discoveries ("Connection pooling fixes the timeout")
    Discoveries,
    /// User preferences ("Prefers async/await over callbacks")
    Preferences,
    /// Guidance and advice ("Always run migrations in a transaction")
    Advice,
}

impl HallType {
    /// All hall types for iteration.
    #[allow(dead_code)]
    pub fn all() -> &'static [HallType] {
        &[
            HallType::Facts,
            HallType::Events,
            HallType::Discoveries,
            HallType::Preferences,
            HallType::Advice,
        ]
    }

    /// Return the hall type name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            HallType::Facts => "facts",
            HallType::Events => "events",
            HallType::Discoveries => "discoveries",
            HallType::Preferences => "preferences",
            HallType::Advice => "advice",
        }
    }
}

impl std::fmt::Display for HallType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ── Hall Entry ──

/// A single classified memory fragment stored in a hall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HallEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// The classified memory content.
    pub content: String,
    /// Which hall type this entry belongs to.
    pub hall_type: HallType,
    /// Back-reference to the source drawer entry.
    pub source_drawer_id: Uuid,
    /// Wing this entry belongs to.
    pub wing_id: String,
    /// Room this entry belongs to.
    pub room_id: String,
    /// When this entry was created.
    pub created_at: DateTime<Local>,
}

impl HallEntry {
    /// Create a new hall entry.
    pub fn new(
        content: String,
        hall_type: HallType,
        source_drawer_id: Uuid,
        wing_id: String,
        room_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            content,
            hall_type,
            source_drawer_id,
            wing_id,
            room_id,
            created_at: Local::now(),
        }
    }

    /// Build the ChromaDB document text for this entry.
    pub fn to_chroma_document(&self) -> String {
        format!(
            "[{}/{}] ({}): {}",
            self.wing_id, self.room_id, self.hall_type, self.content
        )
    }

    /// Build ChromaDB metadata for this entry.
    pub fn to_chroma_metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("wing_id".into(), self.wing_id.clone()),
            ("room_id".into(), self.room_id.clone()),
            ("hall_type".into(), self.hall_type.to_string()),
            ("source_drawer_id".into(), self.source_drawer_id.to_string()),
            ("created_at".into(), self.created_at.to_rfc3339()),
        ])
    }
}

// ── Drawer Entry ──

/// Verbatim original text — never modified, never compressed.
///
/// Drawers store the raw conversation turns exactly as they happened.
/// This is the "ground truth" layer that closets summarize from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// The user's original input.
    pub user_input: String,
    /// The assistant's original response.
    pub assistant_response: String,
    /// Conversation turn number.
    pub turn_number: usize,
    /// Wing this entry belongs to.
    pub wing_id: String,
    /// Room this entry belongs to.
    pub room_id: String,
    /// When this entry was created.
    pub created_at: DateTime<Local>,
}

impl DrawerEntry {
    /// Create a new drawer entry.
    pub fn new(
        user_input: String,
        assistant_response: String,
        turn_number: usize,
        wing_id: String,
        room_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_input,
            assistant_response,
            turn_number,
            wing_id,
            room_id,
            created_at: Local::now(),
        }
    }
}

// ── Closet Entry ──

/// Compressed summary pointing to original drawer content.
///
/// Closets are generated when the number of drawer entries in a room
/// exceeds the configured threshold. They provide quick access to
/// the essence of multiple conversations without reading every verbatim turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosetEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// The compressed summary text.
    pub summary: String,
    /// IDs of the drawer entries this closet summarizes.
    pub source_drawer_ids: Vec<Uuid>,
    /// Wing this closet belongs to.
    pub wing_id: String,
    /// Room this closet belongs to.
    pub room_id: String,
    /// When this closet was created.
    pub created_at: DateTime<Local>,
}

impl ClosetEntry {
    /// Create a new closet entry.
    pub fn new(
        summary: String,
        source_drawer_ids: Vec<Uuid>,
        wing_id: String,
        room_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            summary,
            source_drawer_ids,
            wing_id,
            room_id,
            created_at: Local::now(),
        }
    }
}

// ── Room ──

/// A Room is a specific topic within a Wing.
///
/// Rooms contain halls (classified entries), drawers (verbatim originals),
/// and closets (compressed summaries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    /// Room identifier (slug, e.g., "auth-migration").
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Parent wing ID.
    pub wing_id: String,
    /// When this room was created.
    pub created_at: DateTime<Local>,
    /// When this room was last accessed.
    pub last_accessed: DateTime<Local>,
    /// Number of drawer entries in this room.
    pub drawer_count: usize,
    /// Number of hall entries in this room.
    pub hall_count: usize,
}

impl Room {
    /// Create a new room.
    pub fn new(id: String, label: String, wing_id: String) -> Self {
        let now = Local::now();
        Self {
            id,
            label,
            wing_id,
            created_at: now,
            last_accessed: now,
            drawer_count: 0,
            hall_count: 0,
        }
    }

    /// Mark this room as accessed.
    pub fn touch(&mut self) {
        self.last_accessed = Local::now();
    }
}

// ── Wing ──

/// A Wing represents a person or project — the top-level partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wing {
    /// Wing identifier (slug, e.g., "project-daedalus").
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Rooms within this wing, keyed by room ID.
    pub rooms: HashMap<String, Room>,
    /// When this wing was created.
    pub created_at: DateTime<Local>,
}

impl Wing {
    /// Create a new wing.
    pub fn new(id: String, label: String) -> Self {
        Self {
            id,
            label,
            rooms: HashMap::new(),
            created_at: Local::now(),
        }
    }

    /// Get or create a room within this wing.
    pub fn get_or_create_room(&mut self, room_id: &str, room_label: &str) -> &mut Room {
        let wing_id = self.id.clone();
        self.rooms.entry(room_id.to_string()).or_insert_with(|| {
            Room::new(room_id.to_string(), room_label.to_string(), wing_id)
        })
    }
}

// ── Tunnel (passive — auto-discovered from shared room names) ──

/// Cross-wing connection for same-named rooms.
///
/// Tunnels automatically connect rooms with the same name across
/// different wings, enabling cross-project knowledge discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tunnel {
    /// The shared room name that connects wings.
    pub room_name: String,
    /// Wing IDs that share this room topic.
    pub connected_wings: Vec<String>,
}

// ── Explicit Tunnel (agent-created cross-wing links) ──

/// Agent-created cross-wing link between specific rooms.
///
/// Unlike passive tunnels (discovered from shared room names), explicit tunnels
/// are created by agents when they notice a connection between two specific
/// rooms in different wings/projects. Stored separately so they persist
/// across palace rebuilds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplicitTunnel {
    /// Unique tunnel identifier.
    pub id: String,
    /// Source wing.
    pub source_wing: String,
    /// Source room.
    pub source_room: String,
    /// Target wing.
    pub target_wing: String,
    /// Target room.
    pub target_room: String,
    /// Description of the connection.
    pub label: String,
    /// Optional specific source drawer ID.
    pub source_drawer_id: Option<String>,
    /// Optional specific target drawer ID.
    pub target_drawer_id: Option<String>,
    /// When this tunnel was created.
    pub created_at: DateTime<Local>,
}

impl ExplicitTunnel {
    /// Create a new explicit tunnel with a deterministic symmetric ID.
    pub fn new(
        source_wing: String,
        source_room: String,
        target_wing: String,
        target_room: String,
        label: String,
        source_drawer_id: Option<String>,
        target_drawer_id: Option<String>,
    ) -> Self {
        // Symmetric ID: sort endpoints so A→B and B→A resolve to the same ID
        let mut endpoints = vec![
            format!("{}/{}", source_wing, source_room),
            format!("{}/{}", target_wing, target_room),
        ];
        endpoints.sort();
        let id = format!(
            "tunnel_{}",
            &format!("{:x}", fnv1a_hash(&endpoints.join("|")))[..16]
        );
        Self {
            id,
            source_wing,
            source_room,
            target_wing,
            target_room,
            label,
            source_drawer_id,
            target_drawer_id,
            created_at: Local::now(),
        }
    }
}

/// FNV-1a hash for deterministic ID generation (not cryptographic).
///
/// Used for tunnel IDs and triple IDs where we need a fast,
/// deterministic hash without pulling in a crypto dependency.
pub(super) fn fnv1a_hash(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV prime
    }
    hash
}

// ── Knowledge Graph Entity ──

/// An entity node in the knowledge graph.
///
/// Entities are people, projects, tools, concepts — anything that can
/// participate in relationships. Matches the original MemPalace
/// `entities` SQLite table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Normalized entity ID (lowercase, underscored).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Entity type (person, project, tool, concept, animal, unknown).
    pub entity_type: String,
    /// Additional properties (gender, birthday, etc.).
    pub properties: HashMap<String, String>,
    /// When this entity was created.
    pub created_at: DateTime<Local>,
}

impl Entity {
    /// Create a new entity.
    #[allow(dead_code)]
    pub fn new(name: String, entity_type: String) -> Self {
        let id = name.to_lowercase().replace(' ', "_").replace('\'', "");
        Self {
            id,
            name,
            entity_type,
            properties: HashMap::new(),
            created_at: Local::now(),
        }
    }

    /// Create with properties.
    #[allow(dead_code)]
    pub fn with_properties(
        name: String,
        entity_type: String,
        properties: HashMap<String, String>,
    ) -> Self {
        let mut entity = Self::new(name, entity_type);
        entity.properties = properties;
        entity
    }
}

// ── Knowledge Graph Triple ──

/// SPO (Subject-Predicate-Object) triple for the knowledge graph.
///
/// Enhanced to match the original MemPalace knowledge_graph.py with
/// temporal validity (valid_from → valid_to), confidence scoring,
/// and source tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Triple {
    /// Unique triple identifier.
    pub id: String,
    /// Subject entity.
    pub subject: String,
    /// Predicate (relationship).
    pub predicate: String,
    /// Object entity.
    pub object: String,
    /// When this fact became true (optional).
    pub valid_from: Option<String>,
    /// When this fact stopped being true (None = still valid).
    pub valid_to: Option<String>,
    /// Confidence score (0.0 - 1.0, default 1.0).
    pub confidence: f64,
    /// Source closet ID (optional).
    pub source_closet: Option<String>,
    /// Source file path (optional).
    pub source_file: Option<String>,
    /// Source room ID.
    pub source_room: String,
    /// Source wing ID.
    pub source_wing: String,
    /// When this triple was extracted.
    pub created_at: DateTime<Local>,
}

impl Triple {
    /// Create a new valid triple.
    pub fn new(
        subject: String,
        predicate: String,
        object: String,
        source_room: String,
        source_wing: String,
    ) -> Self {
        let id = format!(
            "t_{}_{}_{}_{}",
            subject.to_lowercase().replace(' ', "_"),
            predicate.to_lowercase().replace(' ', "_"),
            object.to_lowercase().replace(' ', "_"),
            &format!("{:x}", fnv1a_hash(&Local::now().to_rfc3339()))[..12]
        );
        Self {
            id,
            subject,
            predicate,
            object,
            valid_from: None,
            valid_to: None,
            confidence: 1.0,
            source_closet: None,
            source_file: None,
            source_room,
            source_wing,
            created_at: Local::now(),
        }
    }

    /// Create a triple with temporal validity.
    #[allow(dead_code)]
    pub fn with_validity(
        subject: String,
        predicate: String,
        object: String,
        source_room: String,
        source_wing: String,
        valid_from: Option<String>,
        valid_to: Option<String>,
    ) -> Self {
        let mut triple = Self::new(subject, predicate, object, source_room, source_wing);
        triple.valid_from = valid_from;
        triple.valid_to = valid_to;
        triple
    }

    /// Check if this triple is still valid (no end date set).
    pub fn is_valid(&self) -> bool {
        self.valid_to.is_none()
    }

    /// Check if this triple is valid at a specific date.
    #[allow(dead_code)]
    pub fn is_valid_at(&self, date: &str) -> bool {
        let from_ok = match &self.valid_from {
            Some(from) => from.as_str() <= date,
            None => true,
        };
        let to_ok = match &self.valid_to {
            Some(to) => to.as_str() >= date,
            None => true,
        };
        from_ok && to_ok
    }

    /// Invalidate this triple (set end date).
    #[allow(dead_code)]
    pub fn invalidate(&mut self, ended: Option<String>) {
        self.valid_to = Some(
            ended.unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string()),
        );
    }
}

// ── Query result types ──

/// Direction for knowledge graph queries.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KgDirection {
    /// Entity as subject (entity → ?)
    Outgoing,
    /// Entity as object (? → entity)
    Incoming,
    /// Both directions
    Both,
}

/// A single fact from a KG query.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct KgFact {
    pub direction: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub confidence: f64,
    pub source_closet: Option<String>,
    pub current: bool,
}

/// Knowledge graph statistics.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct KgStats {
    pub entities: usize,
    pub triples: usize,
    pub current_facts: usize,
    pub expired_facts: usize,
    pub relationship_types: Vec<String>,
}

/// Graph traversal result node.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct TraversalNode {
    pub room: String,
    pub wings: Vec<String>,
    pub halls: Vec<String>,
    pub count: usize,
    pub hop: usize,
    pub connected_via: Option<Vec<String>>,
}

/// Graph statistics.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct GraphStats {
    pub total_rooms: usize,
    pub tunnel_rooms: usize,
    pub total_edges: usize,
    pub rooms_per_wing: HashMap<String, usize>,
    pub top_tunnels: Vec<TunnelInfo>,
}

/// Tunnel info for graph stats.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct TunnelInfo {
    pub room: String,
    pub wings: Vec<String>,
    pub count: usize,
}

// ── Agent Diary Entry ──

/// An agent diary entry — personal journal for the AI agent.
///
/// Each agent gets its own wing with a diary room. Entries are
/// timestamped and accumulate over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiaryEntry {
    /// Unique entry identifier.
    pub id: String,
    /// Agent name.
    pub agent_name: String,
    /// Diary entry content.
    pub content: String,
    /// Topic tag.
    pub topic: String,
    /// When this entry was created.
    pub created_at: DateTime<Local>,
    /// Date string (YYYY-MM-DD).
    pub date: String,
}

impl DiaryEntry {
    /// Create a new diary entry.
    pub fn new(agent_name: String, content: String, topic: String) -> Self {
        let now = Local::now();
        let id = format!(
            "diary_{}_{}_{}",
            agent_name.to_lowercase().replace(' ', "_"),
            now.format("%Y%m%d_%H%M%S"),
            &format!("{:x}", fnv1a_hash(&content))[..12]
        );
        Self {
            id,
            agent_name,
            content,
            topic,
            created_at: now,
            date: now.format("%Y-%m-%d").to_string(),
        }
    }
}

// ── Palace ──

/// The Palace — top-level container for the entire memory structure.
///
/// Inspired by the Method of Loci (memory palace technique), the Palace
/// organizes memories into a navigable spatial hierarchy:
/// Wings → Rooms → Halls/Drawers/Closets.
///
/// ## Module Organization
///
/// The Palace struct is defined here with core spatial operations.
/// Additional operations are implemented in separate modules:
/// - `knowledge_graph.rs`: KG queries, timeline, stats, seeding
/// - `graph.rs`: BFS traversal, tunnel discovery, graph stats
/// - `diary.rs`: Agent diary read/write
/// - `identity.rs`: L0/L1 identity and wake-up text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palace {
    /// All wings, keyed by wing ID.
    pub(super) wings: HashMap<String, Wing>,
    /// Cross-wing tunnels (passive, auto-discovered), keyed by room name.
    pub(super) tunnels: HashMap<String, Tunnel>,
    /// Explicit tunnels (agent-created cross-wing links).
    pub(super) explicit_tunnels: Vec<ExplicitTunnel>,
    /// Knowledge graph entities.
    pub(super) entities: HashMap<String, Entity>,
    /// Knowledge graph triples.
    pub(super) triples: Vec<Triple>,
    /// Agent diary entries.
    pub(super) diary: Vec<DiaryEntry>,
    /// L0 Identity text (loaded from identity file).
    #[serde(default)]
    pub(super) identity: Option<String>,
    /// Schema version for normalization.
    #[serde(default = "default_normalize_version")]
    pub(super) normalize_version: u32,
}

fn default_normalize_version() -> u32 {
    2
}

impl Palace {
    /// Create a new empty palace.
    pub fn new() -> Self {
        Self {
            wings: HashMap::new(),
            tunnels: HashMap::new(),
            explicit_tunnels: Vec::new(),
            entities: HashMap::new(),
            triples: Vec::new(),
            diary: Vec::new(),
            identity: None,
            normalize_version: 2,
        }
    }

    // ── Wing/Room spatial operations ──

    /// Get or create a wing.
    pub fn get_or_create_wing(&mut self, wing_id: &str, wing_label: &str) -> &mut Wing {
        self.wings.entry(wing_id.to_string()).or_insert_with(|| {
            Wing::new(wing_id.to_string(), wing_label.to_string())
        })
    }

    /// Ensure a room exists in a wing, and update tunnel connections.
    pub fn ensure_room(&mut self, wing_id: &str, wing_label: &str, room_id: &str, room_label: &str) {
        // Create wing and room
        let wing = self.get_or_create_wing(wing_id, wing_label);
        wing.get_or_create_room(room_id, room_label);

        // Update tunnel: connect wings that share the same room name
        let tunnel = self.tunnels.entry(room_id.to_string()).or_insert_with(|| {
            Tunnel {
                room_name: room_id.to_string(),
                connected_wings: Vec::new(),
            }
        });
        if !tunnel.connected_wings.contains(&wing_id.to_string()) {
            tunnel.connected_wings.push(wing_id.to_string());
        }
    }

    /// Get a mutable reference to a wing.
    pub(super) fn wing_mut(&mut self, wing_id: &str) -> Option<&mut Wing> {
        self.wings.get_mut(wing_id)
    }

    /// Get wing IDs.
    pub fn wing_ids(&self) -> Vec<String> {
        self.wings.keys().cloned().collect()
    }

    /// Get all room IDs across all wings (deduplicated).
    pub fn room_ids(&self) -> Vec<String> {
        self.wings.values()
            .flat_map(|w| w.rooms.keys().cloned())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    /// Get the number of wings.
    pub fn wing_count(&self) -> usize {
        self.wings.len()
    }

    /// Get the number of triples.
    pub fn triple_count(&self) -> usize {
        self.triples.len()
    }

    /// Get the number of entities.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Get the number of diary entries.
    pub fn diary_count(&self) -> usize {
        self.diary.len()
    }

    // ── Knowledge Graph: Entity operations ──

    /// Add or update an entity in the knowledge graph.
    #[allow(dead_code)]
    pub fn add_entity(&mut self, entity: Entity) -> String {
        let id = entity.id.clone();
        self.entities.insert(id.clone(), entity);
        id
    }

    /// Get an entity by name.
    #[allow(dead_code)]
    pub fn get_entity(&self, name: &str) -> Option<&Entity> {
        let id = name.to_lowercase().replace(' ', "_").replace('\'', "");
        self.entities.get(&id)
    }

    // ── Knowledge Graph: Triple operations ──

    /// Add a triple to the knowledge graph.
    ///
    /// Deduplicates against existing open triples: if an identical
    /// (subject, predicate, object) triple already exists with valid_to = None,
    /// the new triple is silently dropped. This matches the original MemPalace
    /// knowledge_graph.py behavior.
    ///
    /// Auto-creates entities if they don't exist.
    pub fn add_triple(&mut self, triple: Triple) -> Option<String> {
        // Auto-create entities
        let sub_id = triple.subject.to_lowercase().replace(' ', "_").replace('\'', "");
        let obj_id = triple.object.to_lowercase().replace(' ', "_").replace('\'', "");
        if !self.entities.contains_key(&sub_id) {
            self.entities.insert(
                sub_id.clone(),
                Entity::new(triple.subject.clone(), "unknown".to_string()),
            );
        }
        if !self.entities.contains_key(&obj_id) {
            self.entities.insert(
                obj_id.clone(),
                Entity::new(triple.object.clone(), "unknown".to_string()),
            );
        }

        // Dedup check
        let already_exists = self.triples.iter().any(|t| {
            t.is_valid()
                && t.subject.eq_ignore_ascii_case(&triple.subject)
                && t.predicate.eq_ignore_ascii_case(&triple.predicate)
                && t.object.eq_ignore_ascii_case(&triple.object)
        });
        if already_exists {
            return None;
        }

        let id = triple.id.clone();
        self.triples.push(triple);
        Some(id)
    }

    /// Invalidate a triple (mark as no longer valid).
    #[allow(dead_code)]
    pub fn invalidate_triple(
        &mut self,
        subject: &str,
        predicate: &str,
        object: &str,
        ended: Option<String>,
    ) {
        let end_date = ended.unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
        for triple in &mut self.triples {
            if triple.is_valid()
                && triple.subject.eq_ignore_ascii_case(subject)
                && triple.predicate.eq_ignore_ascii_case(predicate)
                && triple.object.eq_ignore_ascii_case(object)
            {
                triple.valid_to = Some(end_date.clone());
            }
        }
    }

    // ── Passive Tunnel operations ──

    /// Get wing IDs connected to a room via tunnels.
    pub fn tunnel_wings(&self, room_id: &str) -> Vec<String> {
        self.tunnels
            .get(room_id)
            .map(|t| t.connected_wings.clone())
            .unwrap_or_default()
    }

    // ── Explicit Tunnel CRUD ──

    /// Create an explicit cross-wing tunnel.
    #[allow(dead_code)]
    pub fn create_tunnel(
        &mut self,
        source_wing: String,
        source_room: String,
        target_wing: String,
        target_room: String,
        label: String,
        source_drawer_id: Option<String>,
        target_drawer_id: Option<String>,
    ) -> ExplicitTunnel {
        let tunnel = ExplicitTunnel::new(
            source_wing,
            source_room,
            target_wing,
            target_room,
            label,
            source_drawer_id,
            target_drawer_id,
        );

        // Dedup: remove existing tunnel with same ID
        self.explicit_tunnels.retain(|t| t.id != tunnel.id);
        self.explicit_tunnels.push(tunnel.clone());
        tunnel
    }

    /// Delete an explicit tunnel by ID.
    #[allow(dead_code)]
    pub fn delete_tunnel(&mut self, tunnel_id: &str) -> bool {
        let before = self.explicit_tunnels.len();
        self.explicit_tunnels.retain(|t| t.id != tunnel_id);
        self.explicit_tunnels.len() < before
    }

    /// List explicit tunnels, optionally filtered by wing.
    #[allow(dead_code)]
    pub fn list_tunnels(&self, wing: Option<&str>) -> Vec<&ExplicitTunnel> {
        self.explicit_tunnels
            .iter()
            .filter(|t| {
                if let Some(w) = wing {
                    t.source_wing == w || t.target_wing == w
                } else {
                    true
                }
            })
            .collect()
    }

    /// Follow tunnels from a specific room to see connected rooms.
    pub fn follow_tunnels(&self, wing: &str, room: &str) -> Vec<&ExplicitTunnel> {
        self.explicit_tunnels
            .iter()
            .filter(|t| {
                (t.source_wing == wing && t.source_room == room)
                    || (t.target_wing == wing && t.target_room == room)
            })
            .collect()
    }

    // ── Utility ──

    /// Check if the palace is empty.
    pub fn is_empty(&self) -> bool {
        self.wings.is_empty()
    }

    /// Get total number of rooms across all wings.
    #[allow(dead_code)]
    pub fn total_rooms(&self) -> usize {
        self.wings.values().map(|w| w.rooms.len()).sum()
    }
}

impl Default for Palace {
    fn default() -> Self {
        Self::new()
    }
}

// ── Entity Facts (for seeding) ──

/// Structured entity facts for seeding the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityFacts {
    pub full_name: Option<String>,
    pub entity_type: Option<String>,
    pub gender: Option<String>,
    pub birthday: Option<String>,
    pub parent: Option<String>,
    pub partner: Option<String>,
    pub relationship: Option<String>,
    #[serde(default)]
    pub interests: Vec<String>,
}

impl Default for EntityFacts {
    fn default() -> Self {
        Self {
            full_name: None,
            entity_type: None,
            gender: None,
            birthday: None,
            parent: None,
            partner: None,
            relationship: None,
            interests: Vec::new(),
        }
    }
}
