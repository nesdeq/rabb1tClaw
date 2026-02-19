//! Conversation session management with per-device history and persistence.
//!
//! Architecture: One device = One conversation session (no multi-session support).
//! Sessions are encrypted at rest with AES-256-GCM, keyed from SHA-256 of the
//! device token. On-disk format: `nonce (12B) || ciphertext || GCM tag (16B)`.
//!
//! Storage: `~/.rabb1tclaw/<token_prefix>/conversation.enc`

use crate::config::native::{config_dir, device_dir, write_secure};
use crate::config::DeviceStore;
use crate::protocol::now_ms;
use aes_gcm::{aead::Aead, Aes256Gcm, Key, KeyInit, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ============================================================================
// Types
// ============================================================================

/// A single message in a conversation (user or assistant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// A conversation session (one per device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSession {
    pub turns: Vec<ConversationTurn>,
}

impl ConversationSession {
    pub const fn new() -> Self {
        Self {
            turns: Vec::new(),
        }
    }

    /// Add a turn to the session (pruning is token-based, handled at request time).
    pub fn add_turn(&mut self, turn: ConversationTurn) {
        self.turns.push(turn);
    }
}

// ============================================================================
// Session Manager
// ============================================================================

/// Key for session lookup: `token_prefix` (one session per device)
type SessionKey = String;

/// Manages conversation sessions across all devices (one session per device).
pub struct SessionManager {
    sessions: RwLock<HashMap<SessionKey, ConversationSession>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Load all sessions from disk on startup, decrypting with device tokens.
    pub async fn load_from_disk(&self, device_store: &DeviceStore) {
        // Build prefix -> full token lookup from active devices
        let mut token_map: HashMap<String, String> = HashMap::new();
        for device in device_store.devices.values() {
            if !device.revoked {
                token_map.insert(token_prefix(&device.token), device.token.clone());
            }
        }

        // Scan <prefix>/conversation.enc files
        let base = config_dir();
        let mut batch: Vec<(SessionKey, ConversationSession)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let prefix = entry.file_name().to_string_lossy().to_string();

                let Some(token) = token_map.get(&prefix) else {
                    continue; // not a device dir, or revoked
                };

                let conv_path = entry.path().join("conversation.enc");
                if !conv_path.is_file() {
                    continue;
                }

                let session: Option<ConversationSession> = std::fs::read(&conv_path)
                    .ok()
                    .and_then(|data| decrypt_session(token, &data).ok())
                    .and_then(|yaml| serde_yml::from_str(&yaml).ok());

                if let Some(session) = session {
                    batch.push((prefix, session));
                }
            }
        }

        if !batch.is_empty() {
            let count = batch.len();
            {
                let mut sessions = self.sessions.write().await;
                for (key, session) in batch {
                    sessions.insert(key, session);
                }
            }
            info!("[SESSION] Loaded {} sessions from disk", count);
        }
    }

    /// Get chat history for a device.
    pub async fn get_history(&self, token: &str) -> Vec<ConversationTurn> {
        let prefix = token_prefix(token);
        let sessions = self.sessions.read().await;
        sessions
            .get(&prefix)
            .map(|s| s.turns.clone())
            .unwrap_or_default()
    }

    /// Count completed conversation turns (user+assistant pairs) in a session.
    /// 1 turn = 1 user message + 1 assistant response.
    pub async fn turn_count(&self, token: &str) -> usize {
        let prefix = token_prefix(token);
        let sessions = self.sessions.read().await;
        sessions
            .get(&prefix)
            .map_or(0, |s| {
                s.turns.iter().filter(|t| t.role == "user").count()
            })
    }

    /// Record a message: mutate in-memory under write lock, then persist to disk after releasing.
    pub async fn record_message(
        &self,
        token: &str,
        role: &str,
        content: &str,
        run_id: Option<&str>,
    ) {
        let prefix = token_prefix(token);
        let turn = ConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
            timestamp_ms: now_ms(),
            run_id: run_id.map(str::to_string),
        };
        let snapshot = {
            let mut sessions = self.sessions.write().await;
            let session = sessions.entry(prefix.clone()).or_insert_with(ConversationSession::new);
            session.add_turn(turn);
            let snap = session.clone();
            drop(sessions);
            snap
        };
        if let Err(e) = save_session(token, &snapshot) {
            warn!("[SESSION] Failed to persist: {}", e);
        }
    }
}

// ============================================================================
// Encryption
// ============================================================================

/// Derive AES-256 key from device token via SHA-256.
fn derive_key(token: &str) -> Key<Aes256Gcm> {
    let hash = Sha256::digest(token.as_bytes());
    let bytes: [u8; 32] = hash.into();
    bytes.into()
}

/// Encrypt plaintext with AES-256-GCM. Returns `nonce || ciphertext || tag`.
fn encrypt_session(token: &str, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(&derive_key(token));

    // 12-byte nonce from UUID v4 (unique per write)
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..12]);
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt `nonce || ciphertext || tag` with AES-256-GCM. Returns plaintext as UTF-8.
fn decrypt_session(token: &str, data: &[u8]) -> anyhow::Result<String> {
    anyhow::ensure!(data.len() > 12, "encrypted data too short");

    let cipher = Aes256Gcm::new(&derive_key(token));
    let nonce = Nonce::from_slice(&data[..12]);

    let plaintext = cipher
        .decrypt(nonce, &data[12..])
        .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("invalid UTF-8: {e}"))
}

// ============================================================================
// Persistence
// ============================================================================

/// Length of the device token prefix used for filesystem paths.
const TOKEN_PREFIX_LEN: usize = 8;

pub(crate) fn token_prefix(token: &str) -> String {
    // Tokens are hex (ASCII-safe), direct slicing is fine
    token[..token.len().min(TOKEN_PREFIX_LEN)].to_string()
}

/// Path to the conversation file for a device: `~/.rabb1tclaw/<prefix>/conversation.enc`
fn conversation_path(token: &str) -> PathBuf {
    let prefix = token_prefix(token);
    device_dir(&prefix).join("conversation.enc")
}

/// Serialize, encrypt, and write a session to disk.
fn save_session(token: &str, session: &ConversationSession) -> anyhow::Result<()> {
    let path = conversation_path(token);
    let yaml = serde_yml::to_string(session)?;
    let encrypted = encrypt_session(token, yaml.as_bytes())?;
    write_secure(&path, &encrypted)
}
