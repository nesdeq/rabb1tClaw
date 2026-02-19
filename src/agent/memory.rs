//! Memory subagent: extracts remember-worthy facts from conversation and persists
//! them to `memory.md` in the device directory. The memory is injected into the
//! main agent's system prompt on every request.

use crate::agent::session::token_prefix;
use crate::config::native::device_dir;
use crate::provider::ChatMessage;
use crate::state::GatewayState;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info};

use crate::agent::stream::collect_stream;
use crate::config::native::MEMORY_AGENT_SYSTEM_PROMPT;

/// Read memory agent operational limits from config.
fn read_limits(cfg: &crate::config::GatewayConfig) -> (usize, usize) {
    let ac = cfg.agent_config(crate::config::native::AgentKind::Memory);
    let turn_interval = ac.and_then(|a| a.turn_interval).unwrap_or(crate::cli::defaults::DEFAULT_MEMORY_TURN_INTERVAL);
    let max_words = ac.and_then(|a| a.max_words).unwrap_or(crate::cli::defaults::DEFAULT_MEMORY_MAX_WORDS);
    (turn_interval, max_words)
}

/// Sentinel value indicating no memory worth persisting.
const EMPTY_SENTINEL: &str = "<!-- empty -->";

// ============================================================================
// Public API
// ============================================================================

/// Load session memory from disk. Returns `None` if missing, empty, or sentinel.
pub fn load_session_memory(token: &str) -> Option<String> {
    let path = memory_path(token);
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
) {
    let (turn_interval, max_words) = {
        let cfg = state.gateway_config.read().await;
        read_limits(&cfg)
    };

    let turn_count = state.session_manager.turn_count(&token).await;

    if turn_count == 0 || turn_interval == 0 || turn_count % turn_interval != 0 {
        return;
    }

    let prefix = token_prefix(&token);
    info!("[{}] [MEMORY] triggered", prefix);

    if let Err(e) = run_memory_subagent(&state, &token, turn_interval, max_words).await {
        info!("[{}] [MEMORY] failed", prefix);
        debug!("[MEMORY] error: {}", e);
    }
}

// ============================================================================
// Internal
// ============================================================================

/// Path to `memory.md` for a given device: `~/.rabb1tclaw/<prefix>/memory.md`
fn memory_path(token: &str) -> PathBuf {
    let prefix = token_prefix(token);
    device_dir(&prefix).join("memory.md")
}

/// Run the memory subagent: resolve model, build prompt, call LLM, write result.
async fn run_memory_subagent(
    state: &Arc<GatewayState>,
    token: &str,
    turn_interval: usize,
    max_words: usize,
) -> anyhow::Result<()> {
    let resolved = crate::agent::runner::resolve_agent_model(
        state, crate::config::native::AgentKind::Memory,
    ).await
        .ok_or_else(|| anyhow::anyhow!("no active model configured"))?;

    // Get last N turns (turn_interval pairs = 2*turn_interval messages) from session history
    let history = state
        .session_manager
        .get_history(token)
        .await;
    let tail_count = turn_interval * 2;
    let recent = if history.len() > tail_count {
        &history[history.len() - tail_count..]
    } else {
        &history
    };

    // Load existing memory for merge context
    let existing_memory = load_session_memory(token);

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
    let prefix = token_prefix(token);
    if trimmed == EMPTY_SENTINEL || trimmed.is_empty() {
        info!("[{}] [MEMORY] no change", prefix);
        return Ok(());
    }

    let output = enforce_word_limit(trimmed, max_words);

    // Write to disk (full replacement — subagent merges existing + new)
    let path = memory_path(token);
    crate::config::native::write_secure(&path, output.as_bytes())?;

    info!("[{}] [MEMORY] updated", prefix);
    Ok(())
}

/// Format conversation turns + existing memory into the subagent's user message.
fn format_turns_for_subagent(
    turns: &[crate::agent::session::ConversationTurn],
    existing_memory: Option<&str>,
) -> String {
    let mut out = String::new();

    if let Some(mem) = existing_memory {
        let _ = write!(out, "## Existing Memory\n\n{mem}\n\n---\n\n");
    }

    out.push_str("## Recent Exchanges\n\n");
    for turn in turns {
        let _ = write!(out, "**{}**: {}\n\n", turn.role, turn.content);
    }

    out
}

/// Truncate text to at most `max_words` words.
fn enforce_word_limit(text: &str, max_words: usize) -> String {
    // Find the byte offset after the Nth word to avoid collecting into a Vec
    let end = text.split_whitespace()
        .take(max_words)
        .last()
        .map(|last_word| {
            let ptr = last_word.as_ptr() as usize - text.as_ptr() as usize;
            ptr + last_word.len()
        });
    match end {
        Some(pos) if pos < text.len() => text[..pos].to_string(),
        _ => text.to_string(),
    }
}
