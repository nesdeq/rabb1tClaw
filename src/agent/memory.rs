//! Memory subagent: extracts remember-worthy facts from conversation and persists
//! them to `memory.md` in the session directory. The memory is injected into the
//! main agent's system prompt on every request.

use crate::agent::session::{sanitize_session_key, session_dir, token_prefix};
use crate::provider::ChatMessage;
use crate::state::GatewayState;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

/// Read memory agent operational limits from config.
fn read_limits(cfg: &crate::config::GatewayConfig) -> (usize, usize) {
    let ac = cfg.agent_config(crate::config::native::AgentKind::Memory);
    let turn_interval = ac.and_then(|a| a.turn_interval).unwrap_or(crate::cli::defaults::DEFAULT_MEMORY_TURN_INTERVAL);
    let max_words = ac.and_then(|a| a.max_words).unwrap_or(crate::cli::defaults::DEFAULT_MEMORY_MAX_WORDS);
    (turn_interval, max_words)
}

use crate::config::native::MEMORY_AGENT_SYSTEM_PROMPT;

/// Sentinel value indicating no memory worth persisting.
const EMPTY_SENTINEL: &str = "<!-- empty -->";

// ============================================================================
// Public API
// ============================================================================

/// Load session memory from disk. Returns `None` if missing, empty, or sentinel.
pub fn load_session_memory(token: &str, session_key: &str) -> Option<String> {
    let path = memory_path(token, session_key);
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed == EMPTY_SENTINEL {
        return None;
    }
    Some(content)
}

/// Check turn count and conditionally run the memory subagent.
pub async fn maybe_run_memory_subagent(
    state: Arc<GatewayState>,
    token: String,
    session_key: String,
) {
    let (turn_interval, max_words) = {
        let cfg = state.gateway_config.read().await;
        read_limits(&cfg)
    };

    let user_turns = state
        .session_manager
        .user_turn_count(&token, &session_key)
        .await;

    if user_turns == 0 || turn_interval == 0 || user_turns % turn_interval != 0 {
        return;
    }

    info!(
        "Memory subagent triggered at exchange {} for {}",
        user_turns,
        &token[..8.min(token.len())]
    );

    if let Err(e) = run_memory_subagent(&state, &token, &session_key, turn_interval, max_words).await {
        warn!("Memory subagent failed: {}", e);
    }
}

// ============================================================================
// Internal
// ============================================================================

/// Path to `memory.md` for a given device + session.
fn memory_path(token: &str, session_key: &str) -> PathBuf {
    let safe_key = sanitize_session_key(session_key);
    session_dir(&token_prefix(token))
        .join(format!("{}.memory.md", safe_key))
}

/// Run the memory subagent: resolve model, build prompt, call LLM, write result.
async fn run_memory_subagent(
    state: &Arc<GatewayState>,
    token: &str,
    session_key: &str,
    turn_interval: usize,
    max_words: usize,
) -> anyhow::Result<()> {
    let resolved = crate::agent::runner::resolve_agent_model(
        state, crate::config::native::AgentKind::Memory,
    ).await
        .ok_or_else(|| anyhow::anyhow!("no active model configured"))?;

    // Get last N exchanges (2*turn_interval entries) from session history
    let history = state
        .session_manager
        .get_history(token, session_key)
        .await;
    let tail_count = turn_interval * 2;
    let recent = if history.len() > tail_count {
        &history[history.len() - tail_count..]
    } else {
        &history
    };

    // Load existing memory for merge context
    let existing_memory = load_session_memory(token, session_key);

    // Format the user message for the subagent
    let user_content = format_turns_for_subagent(recent, existing_memory.as_deref());

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: user_content,
    }];
    let request = resolved.chat_request(messages, Some(MEMORY_AGENT_SYSTEM_PROMPT.to_string()));
    let rx = resolved.provider.chat_stream(request).await?;
    let raw = collect_stream(rx).await?;
    let trimmed = raw.trim();

    // If subagent says nothing worth remembering, preserve existing memory
    if trimmed == EMPTY_SENTINEL || trimmed.is_empty() {
        info!("Memory subagent returned empty — keeping existing memory");
        return Ok(());
    }

    let output = enforce_word_limit(trimmed, max_words);

    // Write to disk (full replacement — subagent merges existing + new)
    let path = memory_path(token, session_key);
    crate::config::native::write_secure(&path, output.as_bytes())?;

    info!("Memory updated: {}", path.display());
    Ok(())
}

/// Format conversation turns + existing memory into the subagent's user message.
fn format_turns_for_subagent(
    turns: &[crate::agent::session::ConversationTurn],
    existing_memory: Option<&str>,
) -> String {
    let mut out = String::new();

    if let Some(mem) = existing_memory {
        let _ = write!(out, "## Existing Memory\n\n{}\n\n---\n\n", mem);
    }

    out.push_str("## Recent Exchanges\n\n");
    for turn in turns {
        let _ = write!(out, "**{}**: {}\n\n", turn.role, turn.content);
    }

    out
}

// Re-export collect_stream from its canonical location for backwards compat.
pub(crate) use crate::agent::stream::collect_stream;

/// Truncate text to at most `max_words` words.
fn enforce_word_limit(text: &str, max_words: usize) -> String {
    // Find the byte offset after the Nth word to avoid collecting into a Vec
    let end = text.split_whitespace()
        .take(max_words)
        .last()
        .and_then(|last_word| {
            let ptr = last_word.as_ptr() as usize - text.as_ptr() as usize;
            Some(ptr + last_word.len())
        });
    match end {
        Some(pos) if pos < text.len() => text[..pos].to_string(),
        _ => text.to_string(),
    }
}
