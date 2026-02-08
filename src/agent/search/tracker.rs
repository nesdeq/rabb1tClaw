use crate::agent::tracker::{BackgroundTracker, TaskStatus, TrackedItem};
use crate::protocol::now_secs;
use std::fmt::Write;

#[derive(Debug, Clone)]
pub enum SearchQueryStatus {
    Running,
    Completed { context: String },
    Failed { error: String },
}

impl TaskStatus for SearchQueryStatus {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
    fn default_running() -> Self {
        Self::Running
    }
}

pub type SearchQueryTracker = BackgroundTracker<SearchQueryStatus>;

/// Build a search results block for system prompt injection.
/// Items are visible from dispatch until the completed result is delivered.
/// `get_and_mark_delivered` is atomic so `complete()` can't sneak in between.
pub async fn build_search_results_block(
    tracker: &SearchQueryTracker,
    prefix: &str,
    prune_age_secs: u64,
) -> Option<String> {
    let queries = tracker.get_and_mark_delivered(prefix, prune_age_secs).await;
    if queries.is_empty() {
        return None;
    }

    let relevant: Vec<&TrackedItem<SearchQueryStatus>> = queries
        .iter()
        .filter(|q| !q.delivered || q.status.is_running())
        .collect();

    if relevant.is_empty() {
        return None;
    }

    let mut block = String::from("<!-- Web Search Results -->\n");
    let now = now_secs();

    for q in &relevant {
        match &q.status {
            SearchQueryStatus::Running => {
                let elapsed = now.saturating_sub(q.started_at);
                let _ = writeln!(block,
                    "- [running] \"{}\" (started {}s ago)",
                    q.description, elapsed
                );
            }
            SearchQueryStatus::Completed { context } => {
                let _ = writeln!(block,
                    "- [completed] \"{}\":\n{}",
                    q.description, context
                );
            }
            SearchQueryStatus::Failed { error } => {
                let _ = writeln!(block,
                    "- [failed] \"{}\": {}",
                    q.description, error
                );
            }
        }
    }

    block.push_str("<!-- End Web Search Results -->");
    Some(block)
}
