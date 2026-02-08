//! Shared HTML-comment marker parsing and stripping.
//!
//! Both the code agent (`<!--code_task: ... -->`) and search agent
//! (`<!--web_search: ... -->`) use identical marker syntax.  This module
//! provides generic helpers parameterised by the opening tag.

/// Opening tag for code-task markers.
pub const CODE_TASK_TAG: &str = "<!--code_task:";
/// Opening tag for web-search markers.
pub const WEB_SEARCH_TAG: &str = "<!--web_search:";

/// Scan `response` for `<!--{tag}: ... -->` markers, returning the trimmed
/// content of each.
pub fn parse_markers(response: &str, open: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut search_from = 0;
    let close = "-->";

    while let Some(start) = response[search_from..].find(open) {
        let abs_start = search_from + start + open.len();
        if let Some(end) = response[abs_start..].find(close) {
            let desc = response[abs_start..abs_start + end].trim().to_string();
            if !desc.is_empty() {
                results.push(desc);
            }
            search_from = abs_start + end + close.len();
        } else {
            break;
        }
    }
    results
}

/// Remove all `<!--{tag}: ... -->` markers from `response`.
pub fn strip_markers(response: &str, open: &str) -> String {
    let mut result = String::with_capacity(response.len());
    let mut search_from = 0;
    let close = "-->";

    while let Some(start) = response[search_from..].find(open) {
        let abs_start = search_from + start;
        result.push_str(&response[search_from..abs_start]);
        if let Some(end) = response[abs_start..].find(close) {
            search_from = abs_start + end + close.len();
        } else {
            // Unclosed marker — keep it as-is
            result.push_str(&response[abs_start..]);
            return result;
        }
    }
    result.push_str(&response[search_from..]);
    result
}
