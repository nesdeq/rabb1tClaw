//! Shared utilities for Python code extraction and workspace management.

use std::path::Path;

/// Extract Python code from a markdown fenced code block.
pub(crate) fn extract_python_code(response: &str) -> Option<String> {
    let marker = "```python";
    let start = response.find(marker)?;
    let code_start = start + marker.len();
    // Skip optional newline after marker
    let code_start = if response[code_start..].starts_with('\n') {
        code_start + 1
    } else {
        code_start
    };
    let end = response[code_start..].find("```")?;
    let code = response[code_start..code_start + end].trim_end();
    if code.is_empty() {
        return None;
    }
    Some(code.to_string())
}

/// Extract package names from a ### Packages section.
pub(crate) fn extract_packages(response: &str) -> Vec<String> {
    // Look for ### Packages section, then a ``` block
    let header = "### Packages";
    let Some(idx) = response.find(header) else {
        return Vec::new();
    };
    let after = &response[idx + header.len()..];

    // Find the fenced block
    let Some(fence_start) = after.find("```") else {
        return Vec::new();
    };
    let inner_start = fence_start + 3;
    // Skip optional language tag on the fence line
    let inner_start = match after[inner_start..].find('\n') {
        Some(nl) => inner_start + nl + 1,
        None => return Vec::new(),
    };
    let Some(fence_end) = after[inner_start..].find("```") else {
        return Vec::new();
    };

    let block = &after[inner_start..inner_start + fence_end];
    block
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// List workspace contents (top-level only, for context).
pub(crate) fn list_workspace(workspace: &Path) -> String {
    let mut listing = String::new();
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".venv" {
                continue;
            }
            listing.push_str(&name.to_string_lossy());
            listing.push('\n');
        }
    }
    if listing.is_empty() {
        "(empty)".to_string()
    } else {
        listing
    }
}
