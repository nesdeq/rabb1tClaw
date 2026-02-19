use crate::agent::tracker::{BackgroundTracker, TaskStatus};

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
