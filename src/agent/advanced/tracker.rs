use crate::agent::tracker::{BackgroundTracker, TaskStatus};
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub enum AdvancedTaskStatus {
    Running { step: u32, detail: String },
    NeedsInput { question: String },
    Completed { summary: String },
    Failed { error: String },
}

impl TaskStatus for AdvancedTaskStatus {
    fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. } | Self::NeedsInput { .. })
    }
    fn default_running() -> Self {
        Self::Running { step: 0, detail: "initializing".to_string() }
    }
}

pub type AdvancedTaskTracker = BackgroundTracker<AdvancedTaskStatus>;

/// Pending question awaiting a user answer.
pub struct PendingQuestion {
    pub prefix: String,
    pub task_id: u32,
    pub answer_tx: oneshot::Sender<String>,
}
