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
    /// capacity. Retains at most `max_retained` finished items per device.
    pub async fn register(
        &self,
        prefix: &str,
        id: u32,
        description: String,
        max_concurrent: usize,
        max_retained: usize,
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

        // Drop the oldest finished items so history cannot grow without bound
        let mut excess = (entry.len() - running).saturating_sub(max_retained);
        if excess > 0 {
            entry.retain(|t| {
                if excess > 0 && !t.status.is_running() {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
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

    /// Total tracked items for a device, running or finished.
    #[cfg(test)]
    pub async fn tracked_len(&self, prefix: &str) -> usize {
        self.items.read().await.get(prefix).map_or(0, Vec::len)
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
    // A token boundary can fall inside a multi-byte character, which makes the
    // prefix invalid UTF-8; step back until it decodes rather than lose it all.
    let mut take = max_tokens;
    loop {
        if take == 0 {
            return "...".to_string();
        }
        if let Ok(text) = bpe().decode(tokens[..take].to_vec()) {
            return format!("{text}...");
        }
        take -= 1;
    }
}
