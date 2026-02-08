//! Sandboxed code execution agent: spawns Python in a hakoniwa container,
//! self-heals on errors, and tracks results for injection into the main
//! agent's system prompt on the next user turn.

mod agent;
mod sandbox;
mod tracker;

pub use agent::run_agent;
pub use tracker::{build_task_status_block, CodeTaskTracker};

/// Parse `<!--code_task: ... -->` markers from a response.
pub fn parse_code_task_markers(response: &str) -> Vec<String> {
    super::markers::parse_markers(response, super::markers::CODE_TASK_TAG)
}

/// Strip `<!--code_task: ... -->` markers from a response.
pub fn strip_code_task_markers(response: &str) -> String {
    super::markers::strip_markers(response, super::markers::CODE_TASK_TAG)
}
