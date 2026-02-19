//! Persistent task lifecycle log (`tasks.md`) per device.
//!
//! Append-only FIFO file recording task dispatch, completion, failure,
//! asking, and answered events with timestamps. Combined with live
//! running tasks from the in-memory trackers, this builds the `@@task`
//! block injected into user messages.

use crate::config::native::{device_dir, AgentKind};
use crate::protocol::now_secs;
use crate::state::GatewayState;
use chrono::Local;
use std::fs;
use std::path::PathBuf;

const TASK_LOG_FILE: &str = "tasks.md";

/// Read the configured max entries for the task log from the main agent config.
pub async fn max_entries(state: &GatewayState) -> usize {
    let cfg = state.gateway_config.read().await;
    cfg.agent_config(AgentKind::Main)
        .and_then(|a| a.task_log_max_entries)
        .unwrap_or(crate::cli::defaults::DEFAULT_TASK_LOG_MAX_ENTRIES)
}

/// Path to the task log for a given device prefix.
fn log_path(prefix: &str) -> PathBuf {
    device_dir(prefix).join(TASK_LOG_FILE)
}

/// Append one timestamped event line to the task log.
/// Newlines in `event` are flattened to spaces. FIFO keeps last `max_entries` lines.
pub fn append(prefix: &str, event: &str, max_entries: usize) {
    let path = log_path(prefix);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }

    let ts = Local::now().format("%H:%M:%S");
    let flat = event.replace('\n', " ");
    let line = format!("[{ts}] {flat}");

    // Read existing lines, append, and trim to max_entries
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    lines.push(line);

    // FIFO: keep only the last max_entries
    if lines.len() > max_entries {
        let drain = lines.len() - max_entries;
        lines.drain(..drain);
    }

    let content = lines.join("\n") + "\n";
    let _ = fs::write(&path, content);
}

/// Read the current task log content. Returns `None` if empty or missing.
pub fn read(prefix: &str) -> Option<String> {
    let content = fs::read_to_string(log_path(prefix)).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// Build the `@@task ... @@end` block combining persisted log + live running tasks.
/// Returns `None` if there's nothing to show.
pub async fn build_task_context(
    state: &GatewayState,
    prefix: &str,
    max_entries: usize,
) -> Option<String> {
    let log_content = read(prefix);

    // Collect live running tasks from all three trackers
    let now = now_secs();
    let mut live_lines: Vec<String> = Vec::new();

    for item in state.code_task_tracker.get_running(prefix).await {
        let elapsed = now.saturating_sub(item.started_at);
        live_lines.push(format!(
            "[live] running #{} code — {} ({}s)",
            item.id, item.description, elapsed
        ));
    }

    for item in state.search_query_tracker.get_running(prefix).await {
        let elapsed = now.saturating_sub(item.started_at);
        live_lines.push(format!(
            "[live] running #{} search — {} ({}s)",
            item.id, item.description, elapsed
        ));
    }

    for item in state.advanced_task_tracker.get_running(prefix).await {
        let elapsed = now.saturating_sub(item.started_at);
        let status_detail = match &item.status {
            crate::agent::advanced::AdvancedTaskStatus::NeedsInput { question } => {
                format!("asking: {question}")
            }
            crate::agent::advanced::AdvancedTaskStatus::Running { step, detail } => {
                format!("step {step}: {detail}, {elapsed}s")
            }
            _ => format!("{elapsed}s"),
        };
        live_lines.push(format!(
            "[live] running #{} advanced — {} ({})",
            item.id, item.description, status_detail
        ));
    }

    // Combine: persisted log + live running
    let mut parts: Vec<&str> = Vec::new();
    let log_ref = log_content.as_deref();
    if let Some(log) = log_ref {
        parts.push(log);
    }
    let live_block = live_lines.join("\n");
    if !live_lines.is_empty() {
        parts.push(&live_block);
    }

    if parts.is_empty() {
        return None;
    }

    // Trim total lines to max_entries (persisted lines may exceed if live lines added)
    let combined = parts.join("\n");
    let mut all_lines: Vec<&str> = combined.lines().collect();
    if all_lines.len() > max_entries {
        let drain = all_lines.len() - max_entries;
        all_lines.drain(..drain);
    }

    Some(all_lines.join("\n"))
}
