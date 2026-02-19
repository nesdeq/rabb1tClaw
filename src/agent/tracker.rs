//! Generic background task tracker.
//!
//! Provides a single `BackgroundTracker<S>` parameterised over the status
//! enum, used by code, search, and advanced agents.

use crate::protocol::now_secs;
use std::collections::HashMap;
use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;
use tokio::sync::RwLock;

/// Status enums must report whether the item is still running and provide
/// a default "running" variant.
pub trait TaskStatus: Clone + Send + Sync + 'static {
    fn is_running(&self) -> bool;
    fn default_running() -> Self;
}

/// A single tracked background item.
#[derive(Debug, Clone)]
pub struct TrackedItem<S> {
    pub id: u32,
    pub description: String,
    pub status: S,
    pub started_at: u64,
    pub completed_at: Option<u64>,
}

/// Generic background tracker keyed by device-prefix.
pub struct BackgroundTracker<S: TaskStatus> {
    items: RwLock<HashMap<String, Vec<TrackedItem<S>>>>,
}

impl<S: TaskStatus> BackgroundTracker<S> {
    pub fn new() -> Self {
        Self {
            items: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new item. Returns `Some(())` on success, `None` if at
    /// capacity.
    pub async fn register(
        &self,
        prefix: &str,
        id: u32,
        description: String,
        max_concurrent: usize,
    ) -> Option<()> {
        let item = TrackedItem {
            id,
            description,
            status: S::default_running(),
            started_at: now_secs(),
            completed_at: None,
        };
        let mut items = self.items.write().await;
        let entry = items.entry(prefix.to_string()).or_default();

        let running = entry.iter().filter(|t| t.status.is_running()).count();
        if running >= max_concurrent {
            return None;
        }
        entry.push(item);
        drop(items);
        Some(())
    }

    /// Update an item to completed/failed.
    pub async fn complete(&self, prefix: &str, id: u32, status: S) {
        let mut items = self.items.write().await;
        if let Some(list) = items.get_mut(prefix) {
            if let Some(item) = list.iter_mut().find(|t| t.id == id) {
                item.status = status;
                item.completed_at = Some(now_secs());
            }
        }
    }

    /// Update an item's status without completing it (for progress reporting).
    pub async fn update_status(&self, prefix: &str, id: u32, status: S) {
        let mut items = self.items.write().await;
        if let Some(list) = items.get_mut(prefix) {
            if let Some(item) = list.iter_mut().find(|t| t.id == id) {
                item.status = status;
            }
        }
    }

    /// Get currently running items for a device (read-only, no side effects).
    pub async fn get_running(&self, prefix: &str) -> Vec<TrackedItem<S>> {
        let items = self.items.read().await;
        items.get(prefix)
            .map(|list| list.iter().filter(|t| t.status.is_running()).cloned().collect())
            .unwrap_or_default()
    }

}

/// Cached BPE tokenizer (expensive to initialize — singleton).
fn bpe() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().unwrap())
}

/// Count tokens using tiktoken `o200k_base` encoding (GPT-4o/5/o-series).
/// Close enough for Anthropic models too.
pub fn count_tokens(s: &str) -> usize {
    bpe().encode_with_special_tokens(s).len()
}

/// Truncate a string to at most `max_tokens` LLM tokens, appending "...".
pub fn truncate(s: &str, max_tokens: usize) -> String {
    let tokens = bpe().encode_with_special_tokens(s);
    if tokens.len() <= max_tokens {
        return s.to_string();
    }
    let text = bpe().decode(tokens[..max_tokens].to_vec()).unwrap_or_default();
    format!("{text}...")
}
