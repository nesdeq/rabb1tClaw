//! Web search agent: calls Serper.dev API, fetches page content, and injects
//! search results into the main agent's system prompt on the next user turn.

mod agent;
mod tracker;

pub use agent::{run_search, run_search_inner, SearchLimits};
pub use tracker::SearchQueryTracker;
