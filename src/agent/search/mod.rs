//! Web search agent: calls Serper.dev API, fetches page content, and injects
//! search results into the main agent's system prompt on the next user turn.

mod agent;
mod tracker;

pub use agent::{run_search, SearchLimits};
pub use tracker::{build_search_results_block, SearchQueryTracker};

/// Parse `<!--web_search: ... -->` markers from a response.
pub fn parse_web_search_markers(response: &str) -> Vec<String> {
    super::markers::parse_markers(response, super::markers::WEB_SEARCH_TAG)
}

/// Strip `<!--web_search: ... -->` markers from a response.
pub fn strip_web_search_markers(response: &str) -> String {
    super::markers::strip_markers(response, super::markers::WEB_SEARCH_TAG)
}
