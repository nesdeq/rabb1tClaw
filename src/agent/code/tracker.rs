use crate::agent::tracker::{BackgroundTracker, TaskStatus};

#[derive(Debug, Clone)]
pub enum CodeTaskStatus {
    Running,
    Completed { output: String },
    Failed { error: String },
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
