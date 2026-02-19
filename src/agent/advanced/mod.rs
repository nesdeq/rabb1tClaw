//! Advanced orchestrator agent: an LLM-driven loop that plans and delegates
//! to the code agent and search agent to accomplish complex multi-step tasks.

mod agent;
mod tracker;

pub use agent::run_advanced_task;
pub(crate) use agent::{collect_api_env_vars, format_api_availability};
pub use tracker::{AdvancedTaskStatus, AdvancedTaskTracker, PendingQuestion};

/// Dispatch an answer to a pending advanced question matching prefix + `task_id`.
/// Returns true if an answer was dispatched.
pub async fn answer_pending_question(
    state: &crate::state::GatewayState,
    prefix: &str,
    task_id: u32,
    answer: &str,
) -> bool {
    let pq = {
        let mut questions = state.advanced_questions.write().await;
        let idx = questions.iter().position(|q| q.prefix == prefix && q.task_id == task_id);
        let Some(idx) = idx else { return false };
        questions.remove(idx)
    };
    let _ = pq.answer_tx.send(answer.to_string());
    tracing::debug!("[ADVANCED] Dispatched answer to task [{}]", pq.task_id);
    true
}
