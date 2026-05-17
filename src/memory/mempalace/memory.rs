use std::sync::Arc;

use crate::embedding::Embedding;
use crate::llm::{ChatMessage, LlmApi};
use crate::memory::{Memory, MessageBuffer, PersistentState, DEFAULT_MAX_MESSAGES};

use super::classifier;
use super::config::MemPalaceConfig;
use super::dedup;
use super::normalize;
use super::palace::{ClosetEntry, DrawerEntry, HallEntry};
use super::retriever::Retriever;
use super::store::MemPalaceStore;

/// Persistent state for `MemPalaceMemory` across session migrations.
struct MemPalacePersistentState {
    store: MemPalaceStore,
}

/// MemPalace memory strategy — spatial memory organization with ChromaDB.
///
/// Implements the Memory Palace (Method of Loci) pattern for AI memory:
/// conversations are organized into Wings (projects/people), Rooms (topics),
/// and Halls (memory categories). All original text is preserved in Drawers,
/// while classified fragments are stored in ChromaDB for vector retrieval.
///
/// ## Architecture (100% MemPalace feature parity)
///
/// - **Wings**: Top-level partitions for projects or people
/// - **Rooms**: Specific topics within a wing
/// - **Halls**: Fixed-type corridors (facts, events, discoveries, preferences, advice)
/// - **Drawers**: Verbatim original text (never modified)
/// - **Closets**: Compressed summaries (generated when drawer count exceeds threshold)
/// - **Tunnels (passive)**: Auto-discovered cross-wing connections for same-named rooms
/// - **Tunnels (explicit)**: Agent-created cross-wing links
/// - **Knowledge Graph**: SPO triples with temporal validity, entity table
/// - **L0 Identity**: Always-loaded identity text (~100 tokens)
/// - **L1 Essential Story**: Auto-generated from top drawers (~500-800 tokens)
/// - **Agent Diary**: Personal journal for the AI agent
/// - **BM25 Hybrid Search**: Vector + keyword fusion ranking
/// - **AAAK Dialect**: Compressed symbolic memory format
/// - **WAL**: Write-ahead log for audit trail
/// - **Normalize**: Text noise stripping
/// - **Dedup**: Near-duplicate detection
/// - **Entity Detector**: Regex-based entity extraction
/// - **Query Sanitizer**: Search query cleanup
///
/// ## How it works
///
/// 1. **After each turn** (`reflect_on_turn`): The LLM classifier routes the
///    conversation into Wing/Room/Hall, stores verbatim text in Drawer,
///    classified fragment in ChromaDB, and extracts KG triples.
/// 2. **On `build_messages`**: L0 Identity + L1 Essential Story + pre-cached
///    context (from hybrid search + KG traversal + tunnel connections) is
///    injected into the system prompt.
pub struct MemPalaceMemory {
    /// The original system prompt (without memory injection).
    base_system_prompt: String,
    /// Conversation message buffer with sliding window.
    buffer: MessageBuffer,
    /// The persistent store (palace structure + drawers).
    store: MemPalaceStore,
    /// Embedding provider for vector search.
    #[allow(dead_code)]
    embedder: Arc<dyn Embedding>,
    /// ChromaDB-backed retriever.
    retriever: Retriever,
    /// Configuration.
    config: MemPalaceConfig,
    /// Cached context from the most recent retrieval.
    cached_context: Option<String>,
    /// Current turn number.
    turn_number: usize,
    /// Whether ChromaDB collection has been initialized.
    chroma_initialized: bool,
    /// Last classified wing_id (for spatial scoping on next retrieval).
    last_wing_id: Option<String>,
    /// Last classified room_id (for spatial scoping on next retrieval).
    last_room_id: Option<String>,
}

impl MemPalaceMemory {
    /// Create a new MemPalace memory with the given embedding provider.
    #[allow(dead_code)]
    pub fn new(
        system_prompt: &str,
        embedder: Arc<dyn Embedding>,
        config: MemPalaceConfig,
    ) -> Self {
        let retriever = Retriever::new(embedder.clone(), &config);
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            store: MemPalaceStore::new(),
            embedder,
            retriever,
            config,
            cached_context: None,
            turn_number: 0,
            chroma_initialized: false,
            last_wing_id: None,
            last_room_id: None,
        }
    }

    /// Create with an existing store (e.g., loaded from disk).
    pub fn with_store(
        system_prompt: &str,
        store: MemPalaceStore,
        embedder: Arc<dyn Embedding>,
        config: MemPalaceConfig,
    ) -> Self {
        let retriever = Retriever::new(embedder.clone(), &config);
        Self {
            base_system_prompt: system_prompt.to_string(),
            buffer: MessageBuffer::new(DEFAULT_MAX_MESSAGES),
            store,
            embedder,
            retriever,
            config,
            cached_context: None,
            turn_number: 0,
            chroma_initialized: false,
            last_wing_id: None,
            last_room_id: None,
        }
    }

    /// Build the effective system prompt by injecting L0/L1 wake-up + retrieved memory context.
    fn effective_system_prompt(&self) -> String {
        let mut parts = Vec::new();

        // L0 Identity + L1 Essential Story (always loaded, ~600-900 tokens)
        let wake_up = self.store.palace.wake_up(&self.store.drawers);
        if !wake_up.trim().is_empty() {
            parts.push(wake_up);
        }

        // Base system prompt
        parts.push(self.base_system_prompt.clone());

        // Pre-cached retrieval context
        if let Some(ref ctx) = self.cached_context {
            parts.push(ctx.clone());
        }

        parts.join("\n\n")
    }

    /// Ensure ChromaDB collection exists. Retries on each call if not yet initialized.
    async fn ensure_chroma(&mut self) {
        if self.chroma_initialized {
            return;
        }
        match self.retriever.ensure_collection().await {
            Ok(()) => {
                self.chroma_initialized = true;
                tracing::info!("ChromaDB collection initialized for MemPalace");
            }
            Err(e) => {
                // Log but don't set chroma_initialized — next call will retry.
                tracing::warn!(
                    error = %e,
                    "ChromaDB initialization failed (will retry next turn). \
                     Vector search is degraded. Ensure ChromaDB is running at {}",
                    self.config.chroma_url
                );
            }
        }
    }
}

impl Memory for MemPalaceMemory {
    fn add_user_message(&mut self, content: &str) {
        self.buffer.add_user(content);
    }

    fn add_assistant_message(&mut self, content: &str) {
        self.buffer.add_assistant(content);
    }

    fn build_messages(&self) -> Vec<ChatMessage> {
        self.buffer.build_messages_with_system(self.effective_system_prompt())
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cached_context = None;
    }

    fn turn_count(&self) -> usize {
        self.buffer.turn_count()
    }

    fn strategy_name(&self) -> &str {
        "mempalace"
    }

    fn take_persistent_state(&mut self) -> Option<PersistentState> {
        let state = MemPalacePersistentState {
            store: std::mem::replace(&mut self.store, MemPalaceStore::new()),
        };
        Some(PersistentState::new(state))
    }

    fn restore_persistent_state(&mut self, state: PersistentState) {
        match state.downcast::<MemPalacePersistentState>() {
            Ok(s) => {
                self.store = s.store;
            }
            Err(_) => {
                tracing::warn!("Persistent state type mismatch, state discarded");
            }
        }
    }

    fn persist(&self, workspace: &crate::workspace::Workspace) -> anyhow::Result<()> {
        use crate::memory::persistence::MemoryPersistence;
        self.store.save(&workspace.mempalace_dir())?;
        Ok(())
    }

    fn reflect_on_turn<'a>(
        &'a mut self,
        user_input: &'a str,
        assistant_response: &'a str,
        llm: &'a dyn LlmApi,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.turn_number += 1;

            // Step 1: Ensure ChromaDB collection exists
            self.ensure_chroma().await;

            // Step 2: Normalize and dedup
            let (clean_input, clean_response) = match self.normalize_and_dedup(user_input, assistant_response) {
                Some(pair) => pair,
                None => return, // Near-duplicate, skip
            };

            // Step 3: Classify the conversation turn
            let classification = match self.classify_turn(&clean_input, &clean_response, llm).await {
                Some(c) => c,
                None => return, // Classification failed
            };

            // Step 4: Store in palace structure
            let drawer_id = self.store_classified_turn(&classification, &clean_input, &clean_response);

            // Step 5: Store in ChromaDB
            self.store_in_chroma(&classification, drawer_id).await;

            // Step 6: Add knowledge graph triples
            for triple in classification.triples {
                self.store.palace.add_triple(triple);
            }

            // Step 7: Generate closet if threshold reached
            self.maybe_generate_closet(&classification.wing_id, &classification.room_id, llm).await;

            // Step 8: Update spatial scope and pre-retrieve context
            self.last_wing_id = Some(classification.wing_id.clone());
            self.last_room_id = Some(classification.room_id.clone());
            self.pre_retrieve_context(&clean_input).await;

            // Step 9: Auto-write diary entry every 10 turns
            if self.turn_number % 10 == 0 {
                let summary = format!(
                    "Session turn {}. Working on {}/{}. {} total drawers.",
                    self.turn_number,
                    classification.wing_id,
                    classification.room_id,
                    self.store.total_drawers()
                );
                self.store.palace.diary_write(
                    "daedalus",
                    &summary,
                    "session-checkpoint",
                );
            }
        })
    }
}

// ── Extracted pipeline steps for reflect_on_turn ──

impl MemPalaceMemory {
    /// Normalize input text and check for near-duplicates.
    ///
    /// Returns `Some((clean_input, clean_response))` if content is new,
    /// or `None` if it's a near-duplicate that should be skipped.
    fn normalize_and_dedup(
        &self,
        user_input: &str,
        assistant_response: &str,
    ) -> Option<(String, String)> {
        let clean_input = normalize::strip_noise(user_input);
        let clean_response = normalize::strip_noise(assistant_response);

        let existing_contents = self.store.all_drawer_contents();
        let combined = format!("{} {}", clean_input, clean_response);
        if dedup::is_duplicate(&combined, &existing_contents, self.config.dedup_threshold) {
            tracing::debug!("Skipping near-duplicate content");
            return None;
        }

        Some((clean_input, clean_response))
    }

    /// Classify a conversation turn via LLM.
    ///
    /// Returns `Some(ClassificationResult)` on success, or `None` on failure.
    async fn classify_turn(
        &self,
        clean_input: &str,
        clean_response: &str,
        llm: &dyn LlmApi,
    ) -> Option<classifier::ClassificationResult> {
        let existing_wings = self.store.wing_ids();
        let existing_rooms = self.store.room_ids();

        match classifier::classify_turn(
            clean_input,
            clean_response,
            &existing_wings,
            &existing_rooms,
            llm,
        ).await {
            Ok(c) => {
                tracing::debug!(
                    wing = %c.wing_id,
                    room = %c.room_id,
                    hall = %c.hall_type,
                    memory = %c.memory,
                    triples = c.triples.len(),
                    "Classified conversation turn"
                );
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to classify conversation turn, skipping memory storage"
                );
                None
            }
        }
    }

    /// Store a classified turn in the palace structure (wing/room/drawer).
    ///
    /// Returns the drawer ID for back-referencing from hall entries.
    fn store_classified_turn(
        &mut self,
        classification: &classifier::ClassificationResult,
        clean_input: &str,
        clean_response: &str,
    ) -> uuid::Uuid {
        self.store.palace.ensure_room(
            &classification.wing_id,
            &classification.wing_label,
            &classification.room_id,
            &classification.room_label,
        );

        let drawer = DrawerEntry::new(
            clean_input.to_string(),
            clean_response.to_string(),
            self.turn_number,
            classification.wing_id.clone(),
            classification.room_id.clone(),
        );
        let drawer_id = drawer.id;
        self.store.add_drawer(drawer);
        drawer_id
    }

    /// Store a classified memory fragment in ChromaDB.
    async fn store_in_chroma(
        &mut self,
        classification: &classifier::ClassificationResult,
        drawer_id: uuid::Uuid,
    ) {
        if !self.chroma_initialized {
            return;
        }

        let hall_entry = HallEntry::new(
            classification.memory.clone(),
            classification.hall_type,
            drawer_id,
            classification.wing_id.clone(),
            classification.room_id.clone(),
        );

        if let Err(e) = self.retriever.add_entry(&hall_entry).await {
            tracing::warn!(error = %e, "Failed to add hall entry to ChromaDB");
        } else {
            self.store.increment_hall_count(
                &classification.wing_id,
                &classification.room_id,
            );
        }
    }

    /// Generate a closet summary if the un-closeted drawer threshold is reached.
    async fn maybe_generate_closet(
        &mut self,
        wing_id: &str,
        room_id: &str,
        llm: &dyn LlmApi,
    ) {
        let uncloseted = self.store.uncloseted_drawers(wing_id, room_id);
        let uncloseted_count = uncloseted.len();

        if uncloseted_count < self.config.closet_threshold {
            return;
        }

        tracing::info!(
            wing = %wing_id,
            room = %room_id,
            uncloseted = uncloseted_count,
            threshold = self.config.closet_threshold,
            "Closet threshold reached, generating compressed summary"
        );

        if let Err(e) = self.generate_closet(wing_id, room_id, llm).await {
            tracing::warn!(error = %e, "Failed to generate closet summary");
        }
    }

    /// Pre-retrieve context for the next turn.
    async fn pre_retrieve_context(&mut self, query: &str) {
        if !self.chroma_initialized {
            return;
        }

        match self.retriever.retrieve_context(
            query,
            &self.store.palace,
            &self.store,
            self.last_wing_id.as_deref(),
            None, // Don't restrict to room — allow cross-room discovery
        ).await {
            Ok(ctx) => {
                self.cached_context = ctx;
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to pre-retrieve MemPalace context");
            }
        }
    }

    /// Generate a closet (compressed summary) for un-closeted drawers in a room.
    ///
    /// Collects all drawer entries that haven't been summarized yet,
    /// sends them to the LLM for summarization, and stores the result
    /// as a new ClosetEntry.
    async fn generate_closet(
        &mut self,
        wing_id: &str,
        room_id: &str,
        llm: &dyn LlmApi,
    ) -> anyhow::Result<()> {
        use super::prompts::{CLOSET_SYSTEM_PROMPT, closet_prompt};

        let uncloseted = self.store.uncloseted_drawers(wing_id, room_id);

        if uncloseted.is_empty() {
            return Ok(());
        }

        // Build drawer texts for the prompt
        let drawer_texts: Vec<String> = uncloseted
            .iter()
            .map(|d| format!("User: {}\nAssistant: {}", d.user_input, d.assistant_response))
            .collect();

        let source_ids: Vec<uuid::Uuid> = uncloseted.iter().map(|d| d.id).collect();

        // Call LLM to generate summary
        let messages = vec![
            ChatMessage::system(CLOSET_SYSTEM_PROMPT),
            ChatMessage::user(closet_prompt(&drawer_texts)),
        ];

        let response = llm.chat(&messages, None).await?;
        let summary = response.content.trim().to_string();

        if summary.is_empty() {
            anyhow::bail!("LLM returned empty closet summary");
        }

        // Store the closet entry
        let closet = ClosetEntry::new(
            summary.clone(),
            source_ids,
            wing_id.to_string(),
            room_id.to_string(),
        );

        tracing::info!(
            wing = %wing_id,
            room = %room_id,
            drawers_summarized = closet.source_drawer_ids.len(),
            summary_len = summary.len(),
            "Generated closet summary"
        );

        self.store.add_closet(closet);

        Ok(())
    }
}
