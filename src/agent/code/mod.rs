//! Sandboxed code execution agent: spawns Python in a hakoniwa container,
//! self-heals on errors, and tracks results for injection into the main
//! agent's system prompt on the next user turn.

mod agent;
pub(crate) mod helpers;
pub(crate) mod sandbox;
mod tracker;

pub use agent::run_agent;
pub(crate) use agent::run_code_loop;
pub use tracker::CodeTaskTracker;
