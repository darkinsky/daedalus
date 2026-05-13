//! Session state serialization and persistence.
//!
//! Handles saving/loading conversation messages and the consolidation cursor
//! to/from disk. Extracted from `memory.rs` to keep persistence concerns
//! separate from the core memory logic.

use crate::llm::ChatMessage;
use crate::memory::persistence::atomic_write;

// ── Serializable message ──

/// Serializable content part for disk persistence.
///
/// Only `Text` parts are persisted. Image parts (which contain large base64
/// data) are intentionally dropped during serialization to avoid bloating
/// the session file. After a restart, image context is lost — this is an
/// acceptable trade-off since images are typically transient input.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
enum SerializableContentPart {
    Text { text: String },
    /// Placeholder for an image that was present but not persisted.
    ImagePlaceholder {
        #[serde(default)]
        detail: Option<String>,
    },
}

/// Serializable representation of a ChatMessage for disk persistence.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SerializableMessage {
    role: String,
    content: String,
    /// Whether this message is semantically preserved (not compressible).
    /// Defaults to `false` for backward compatibility with old session files.
    #[serde(default)]
    preserved: bool,
    /// Rich content parts (text + image placeholders).
    /// Empty by default for backward compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    content_parts: Vec<SerializableContentPart>,
}

impl From<&ChatMessage> for SerializableMessage {
    fn from(msg: &ChatMessage) -> Self {
        let content_parts: Vec<SerializableContentPart> = msg.content_parts.iter().filter_map(|part| {
            match part {
                crate::llm::ContentPart::Text { text } => {
                    Some(SerializableContentPart::Text { text: text.clone() })
                }
                crate::llm::ContentPart::Image { detail, .. } => {
                    // Store a placeholder instead of the full base64 image data.
                    Some(SerializableContentPart::ImagePlaceholder {
                        detail: detail.clone(),
                    })
                }
            }
        }).collect();

        Self {
            role: msg.role.to_string(),
            content: msg.content.clone(),
            preserved: msg.preserved,
            content_parts,
        }
    }
}

impl SerializableMessage {
    pub(super) fn to_chat_message(&self) -> ChatMessage {
        let mut msg = match self.role.as_str() {
            "system" => ChatMessage::system(&self.content),
            "user" => ChatMessage::user(&self.content),
            "assistant" => ChatMessage::assistant(&self.content),
            "tool" => ChatMessage::tool(&self.content),
            _ => ChatMessage::user(&self.content), // fallback
        };

        // Restore preserved flag
        msg.preserved = self.preserved;

        // Restore text content parts (image placeholders are dropped since
        // we don't have the original image data)
        if !self.content_parts.is_empty() {
            let parts: Vec<crate::llm::ContentPart> = self.content_parts.iter().filter_map(|part| {
                match part {
                    SerializableContentPart::Text { text } => {
                        Some(crate::llm::ContentPart::Text { text: text.clone() })
                    }
                    SerializableContentPart::ImagePlaceholder { .. } => {
                        // Image data was not persisted; drop the placeholder.
                        // The text content field still contains the user's text.
                        None
                    }
                }
            }).collect();
            msg.content_parts = parts;
        }

        msg
    }
}

// ── Session state ──

/// Serializable session state: messages + consolidation cursor.
///
/// The `consolidation_cursor` must be persisted alongside messages so that
/// after a restart we know which messages have already been consolidated.
/// Without it, all messages would appear unconsolidated, causing duplicate
/// consolidation.
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionState {
    /// Index of the first unconsolidated message.
    consolidation_cursor: usize,
    /// All conversation messages in chronological order.
    messages: Vec<SerializableMessage>,
}

/// Loaded session state from disk.
pub(super) struct LoadedSessionState {
    pub messages: Vec<ChatMessage>,
    pub consolidation_cursor: usize,
}

/// Save session state (messages + consolidation cursor) to a JSON file atomically.
pub(super) fn save_session_state(
    messages: &[ChatMessage],
    consolidation_cursor: usize,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let state = SessionState {
        consolidation_cursor,
        messages: messages.iter().map(SerializableMessage::from).collect(),
    };
    let json = serde_json::to_string(&state)
        .map_err(|e| anyhow::anyhow!("Failed to serialize session state: {}", e))?;
    atomic_write(path, json.as_bytes())?;
    Ok(())
}

/// Load session state from a JSON file.
/// Returns empty state if the file doesn't exist.
/// Handles backward compatibility with the old format (plain message array).
pub(super) fn load_session_state(path: &std::path::Path) -> anyhow::Result<LoadedSessionState> {
    if !path.exists() {
        return Ok(LoadedSessionState {
            messages: Vec::new(),
            consolidation_cursor: 0,
        });
    }
    let data = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read session state: {}", e))?;

    // Try new format first (object with consolidation_cursor + messages)
    if let Ok(state) = serde_json::from_str::<SessionState>(&data) {
        return Ok(LoadedSessionState {
            messages: state.messages.iter().map(|m| m.to_chat_message()).collect(),
            consolidation_cursor: state.consolidation_cursor,
        });
    }

    // Fallback: old format (plain array of messages, no cursor)
    let serializable: Vec<SerializableMessage> = serde_json::from_str(&data)
        .map_err(|e| anyhow::anyhow!("Failed to deserialize session state: {}", e))?;
    Ok(LoadedSessionState {
        messages: serializable.iter().map(|m| m.to_chat_message()).collect(),
        consolidation_cursor: 0,
    })
}
