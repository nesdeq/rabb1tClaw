//! Conversation session management with per-device history and persistence.
//!
//! Sessions are encrypted at rest with AES-256-GCM, keyed from SHA-256 of the
//! device token. On-disk format: `nonce (12B) || ciphertext || GCM tag (16B)`.

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

    /// Add a turn to the session (pruning is token-based, handled at request time).
    pub fn add_turn(&mut self, turn: ConversationTurn) {
        self.turns.push(turn);
        self.updated_at_ms = now_ms();
    }
}

// ============================================================================
// Session Manager
// ============================================================================

/// Key for session lookup: (token_prefix, session_key)
type SessionKey = (String, String);

/// Summary of a session for listing (avoids cloning full turn content).
pub struct SessionSummary {
    pub key: String,
    pub turn_count: usize,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

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

    /// Load all sessions from disk on startup, decrypting with device tokens.
    pub async fn load_from_disk(&self, device_store: &DeviceStore) {
        // Build prefix -> full token lookup from active devices
        let mut token_map: HashMap<String, String> = HashMap::new();
        for device in device_store.devices.values() {
            if !device.revoked {
                token_map.insert(token_prefix(&device.token), device.token.clone());
            }
        }

        // Scan <prefix>/session/ dirs for .enc files
        let base = config_dir();
        let mut batch: Vec<(SessionKey, ConversationSession)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let prefix = entry.file_name().to_string_lossy().to_string();

                let token = match token_map.get(&prefix) {
                    Some(t) => t,
                    None => continue, // not a device dir, or revoked
                };

                let sess_dir = entry.path().join("session");
                if !sess_dir.is_dir() {
                    continue;
                }

                if let Ok(session_files) = std::fs::read_dir(&sess_dir) {
                    for file in session_files.flatten() {
                        let path = file.path();
                        let ext = path.extension().and_then(|e| e.to_str());
                        let session_key = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();

                        let session: Option<ConversationSession> = match ext {
                            Some("enc") => {
                                std::fs::read(&path)
                                    .ok()
                                    .and_then(|data| decrypt_session(token, &data).ok())
                                    .and_then(|yaml| serde_yml::from_str(&yaml).ok())
                            }
                            _ => None,
                        };

                        if let Some(session) = session {
                            batch.push(((prefix.clone(), session_key), session));
                        }
                    }
                }
            }
        }

        if !batch.is_empty() {
            let count = batch.len();
            let mut sessions = self.sessions.write().await;
            for (key, session) in batch {
                sessions.insert(key, session);
            }
            info!("Loaded {} conversation sessions from disk", count);
        }
    }

    /// List sessions for a device token (returns lightweight summaries).
    pub async fn list_sessions(&self, token: &str) -> Vec<SessionSummary> {
        let prefix = token_prefix(token);
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .filter(|((tp, _), _)| *tp == prefix)
            .map(|((_, sk), s)| SessionSummary {
                key: sk.clone(),
                turn_count: s.turns.len(),
                created_at_ms: s.created_at_ms,
                updated_at_ms: s.updated_at_ms,
            })
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

    /// Count user turns (completed exchanges) in a session.
    pub async fn user_turn_count(&self, token: &str, session_key: &str) -> usize {
        let key = make_key(token, session_key);
        let sessions = self.sessions.read().await;
        sessions
            .get(&key)
            .map(|s| s.turns.iter().filter(|t| t.role == "user").count())
            .unwrap_or(0)
    }

    /// Record a message: mutate in-memory under write lock, then persist to disk after releasing.
    pub async fn record_message(
        &self,
        token: &str,
        session_key: &str,
        role: &str,
        content: &str,
        run_id: Option<&str>,
    ) {
        let key = make_key(token, session_key);
        let snapshot = {
            let mut sessions = self.sessions.write().await;
            let session = sessions.entry(key).or_insert_with(ConversationSession::new);
            session.add_turn(ConversationTurn {
                role: role.to_string(),
                content: content.to_string(),
                timestamp_ms: now_ms(),
                run_id: run_id.map(|s| s.to_string()),
            });
            session.clone()
        }; // write lock released here
        if let Err(e) = save_session(token, session_key, &snapshot) {
            warn!("Failed to persist session: {}", e);
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
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

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
        .map_err(|e| anyhow::anyhow!("decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("invalid UTF-8: {}", e))
}

// ============================================================================
// Persistence
// ============================================================================

/// Per-device session directory: `~/.rabb1tclaw/<prefix>/session/`
pub(crate) fn session_dir(token_prefix: &str) -> PathBuf {
    device_dir(token_prefix).join("session")
}

/// Length of the device token prefix used for filesystem paths.
const TOKEN_PREFIX_LEN: usize = 8;

pub(crate) fn token_prefix(token: &str) -> String {
    // Tokens are hex (ASCII-safe), direct slicing is fine
    token[..token.len().min(TOKEN_PREFIX_LEN)].to_string()
}

fn make_key(token: &str, session_key: &str) -> SessionKey {
    (token_prefix(token), session_key.to_string())
}

/// Sanitize a session key for safe use as a filesystem path component.
pub(crate) fn sanitize_session_key(session_key: &str) -> String {
    session_key
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ':' { c } else { '_' })
        .collect()
}

/// Serialize, encrypt, and write a session to disk.
fn save_session(token: &str, session_key: &str, session: &ConversationSession) -> anyhow::Result<()> {
    let prefix = token_prefix(token);
    let safe_key = sanitize_session_key(session_key);
    let path = device_dir(&prefix).join("session").join(format!("{}.enc", safe_key));
    let yaml = serde_yml::to_string(session)?;
    let encrypted = encrypt_session(token, yaml.as_bytes())?;
    write_secure(&path, &encrypted)
}
