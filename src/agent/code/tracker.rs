use crate::agent::tracker::{BackgroundTracker, TaskStatus, TrackedItem, truncate};
use crate::protocol::now_secs;
use std::fmt::Write;

#[derive(Debug, Clone)]
pub enum CodeTaskStatus {
    Running,
    Completed { output: String, iterations: u32 },
    Failed { error: String, iterations: u32 },
}

impl TaskStatus for CodeTaskStatus {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
    fn default_running() -> Self {
        Self::Running
    }
}

pub type CodeTaskTracker = BackgroundTracker<CodeTaskStatus>;

/// Build a status block for system prompt injection.
/// Items are visible from dispatch until the completed result is delivered.
/// `get_and_mark_delivered` is atomic so `complete()` can't sneak in between.
pub async fn build_task_status_block(
    tracker: &CodeTaskTracker,
    prefix: &str,
    prune_age_secs: u64,
    max_status_tokens: usize,
) -> Option<String> {
    let tasks = tracker.get_and_mark_delivered(prefix, prune_age_secs).await;
    if tasks.is_empty() {
        return None;
    }

    let relevant: Vec<&TrackedItem<CodeTaskStatus>> = tasks
        .iter()
        .filter(|t| !t.delivered || t.status.is_running())
        .collect();

    if relevant.is_empty() {
        return None;
    }

    let mut block = String::from("<!-- Background Tasks -->\n");
    let now = now_secs();

    for task in &relevant {
        match &task.status {
            CodeTaskStatus::Running => {
                let elapsed = now.saturating_sub(task.started_at);
                let _ = writeln!(block,
                    "- [running] \"{}\" (started {}s ago)",
                    task.description, elapsed
                );
            }
            CodeTaskStatus::Completed { output, iterations } => {
                let summary = truncate(output, max_status_tokens);
                let _ = writeln!(block,
                    "- [completed] \"{}\": {} ({} iterations)",
                    task.description, summary, iterations
                );
            }
            CodeTaskStatus::Failed { error, iterations } => {
                let summary = truncate(error, max_status_tokens);
                let _ = writeln!(block,
                    "- [failed] \"{}\": {} ({} iterations)",
                    task.description, summary, iterations
                );
            }
        }
    }

    block.push_str("<!-- End Background Tasks -->");
    Some(block)
}
