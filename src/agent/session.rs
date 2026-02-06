//! Conversation session management with per-device history and persistence.

use crate::config::native::config_dir;
use crate::protocol::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{info, warn};

const MAX_TURNS: usize = 50;

// ============================================================================
// Types
// ============================================================================

/// A single message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// A conversation session (per device + session key)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSession {
    pub turns: Vec<ConversationTurn>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl ConversationSession {
    pub fn new() -> Self {
        let now = now_ms();
        Self {
            turns: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    /// Add a turn, pruning oldest turns if over MAX_TURNS
    pub fn add_turn(&mut self, turn: ConversationTurn) {
        self.turns.push(turn);
        self.updated_at_ms = now_ms();

        // Auto-prune: keep only the last MAX_TURNS entries
        if self.turns.len() > MAX_TURNS {
            let excess = self.turns.len() - MAX_TURNS;
            self.turns.drain(..excess);
        }
    }

    /// Add user message
    pub fn add_user_message(&mut self, content: &str, run_id: Option<&str>) {
        self.add_turn(ConversationTurn {
            role: "user".to_string(),
            content: content.to_string(),
            timestamp_ms: now_ms(),
            run_id: run_id.map(|s| s.to_string()),
        });
    }

    /// Add assistant message
    pub fn add_assistant_message(&mut self, content: &str, run_id: Option<&str>) {
        self.add_turn(ConversationTurn {
            role: "assistant".to_string(),
            content: content.to_string(),
            timestamp_ms: now_ms(),
            run_id: run_id.map(|s| s.to_string()),
        });
    }
}

// ============================================================================
// Session Manager
// ============================================================================

/// Key for session lookup: (token_prefix, session_key)
type SessionKey = (String, String);

/// Manages conversation sessions across all devices
pub struct SessionManager {
    sessions: RwLock<HashMap<SessionKey, ConversationSession>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Load all sessions from disk on startup
    pub async fn load_from_disk(&self) {
        let sessions_dir = sessions_dir();
        if !sessions_dir.exists() {
            return;
        }

        let mut loaded = 0;
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let token_prefix = entry.file_name().to_string_lossy().to_string();

                if let Ok(session_files) = std::fs::read_dir(entry.path()) {
                    for file in session_files.flatten() {
                        let path = file.path();
                        if path.extension().map(|e| e == "yaml").unwrap_or(false) {
                            let session_key = path
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();

                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(session) = serde_yml::from_str::<ConversationSession>(&content) {
                                    let key = (token_prefix.clone(), session_key);
                                    self.sessions.write().await.insert(key, session);
                                    loaded += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        if loaded > 0 {
            info!("Loaded {} conversation sessions from disk", loaded);
        }
    }

    /// Get or create a session for the given token + session key
    pub async fn get_or_create(&self, token: &str, session_key: &str) -> ConversationSession {
        let key = make_key(token, session_key);
        let sessions = self.sessions.read().await;
        sessions.get(&key).cloned().unwrap_or_else(ConversationSession::new)
    }

    /// Update a session and persist to disk
    pub async fn update(&self, token: &str, session_key: &str, session: ConversationSession) {
        let key = make_key(token, session_key);

        // Persist to disk
        if let Err(e) = save_session(&key.0, &key.1, &session) {
            warn!("Failed to persist session: {}", e);
        }

        // Update in-memory
        self.sessions.write().await.insert(key, session);
    }

    /// List sessions for a device token
    pub async fn list_sessions(&self, token: &str) -> Vec<(String, ConversationSession)> {
        let prefix = token_prefix(token);
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .filter(|((tp, _), _)| *tp == prefix)
            .map(|((_, sk), s)| (sk.clone(), s.clone()))
            .collect()
    }

    /// Get chat history for a session
    pub async fn get_history(&self, token: &str, session_key: &str) -> Vec<ConversationTurn> {
        let key = make_key(token, session_key);
        let sessions = self.sessions.read().await;
        sessions
            .get(&key)
            .map(|s| s.turns.clone())
            .unwrap_or_default()
    }
}

// ============================================================================
// Persistence
// ============================================================================

fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

fn token_prefix(token: &str) -> String {
    // Use first 8 chars of token as directory name
    token.chars().take(8).collect()
}

fn make_key(token: &str, session_key: &str) -> SessionKey {
    (token_prefix(token), session_key.to_string())
}

fn save_session(token_prefix: &str, session_key: &str, session: &ConversationSession) -> anyhow::Result<()> {
    // Sanitize session_key for filesystem
    let safe_key: String = session_key
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ':' { c } else { '_' })
        .collect();

    let path = sessions_dir().join(token_prefix).join(format!("{}.yaml", safe_key));
    let content = serde_yml::to_string(session)?;
    crate::config::native::write_secure(&path, &content)
}
