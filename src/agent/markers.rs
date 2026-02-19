//! `@@dispatch` block parsing and stripping.
//!
//! All agent dispatch and answer relay use a single block format:
//!
//!   @@dispatch
//!   [{"type":"code","desc":"..."}]
//!   @@end
//!
//!   @@dispatch
//!   [{"id":3,"answer":"..."}]
//!   @@end

const DISPATCH_OPEN: &str = "@@dispatch\n";
const BLOCK_CLOSE: &str = "\n@@end";

/// A parsed task marker from the LLM response.
#[derive(Debug)]
pub enum TaskMarker {
    /// Dispatch a new background task.
    Dispatch { task_type: String, desc: String },
    /// Relay an answer to a pending advanced agent question.
    Answer { id: u32, answer: String },
}

/// Parse all `@@dispatch ... @@end` blocks from a response, returning typed markers.
pub fn parse_task_markers(response: &str) -> Vec<TaskMarker> {
    let mut results = Vec::new();
    let mut search_from = 0;

    while let Some(open_offset) = response[search_from..].find(DISPATCH_OPEN) {
        let content_start = search_from + open_offset + DISPATCH_OPEN.len();
        if let Some(close_offset) = response[content_start..].find(BLOCK_CLOSE) {
            let raw = response[content_start..content_start + close_offset].trim();
            match serde_json::from_str::<Vec<serde_json::Value>>(raw) {
                Ok(arr) => {
                    for val in &arr {
                        if let Some(marker) = classify_marker(val) {
                            results.push(marker);
                        }
                    }
                }
                Err(e) => {
                    let preview: String = raw.chars().take(120).collect();
                    tracing::warn!("Failed to parse dispatch block JSON: {} — {:?}", e, preview);
                }
            }
            // Skip past \n@@end
            search_from = content_start + close_offset + BLOCK_CLOSE.len();
        } else {
            break;
        }
    }
    results
}

/// Remove all `@@dispatch\n...\n@@end` blocks from a response.
pub fn strip_task_markers(response: &str) -> String {
    let mut result = String::with_capacity(response.len());
    let mut search_from = 0;

    while let Some(open_offset) = response[search_from..].find(DISPATCH_OPEN) {
        let abs_start = search_from + open_offset;
        // Include any preceding newline that belongs to the block boundary
        let trim_start = if abs_start > 0 && response.as_bytes()[abs_start - 1] == b'\n' {
            abs_start - 1
        } else {
            abs_start
        };
        result.push_str(&response[search_from..trim_start]);

        let content_start = abs_start + DISPATCH_OPEN.len();
        if let Some(close_offset) = response[content_start..].find(BLOCK_CLOSE) {
            let block_end = content_start + close_offset + BLOCK_CLOSE.len();
            // Also skip trailing newline if present
            if block_end < response.len() && response.as_bytes()[block_end] == b'\n' {
                search_from = block_end + 1;
            } else {
                search_from = block_end;
            }
        } else {
            // Unclosed block — keep it as-is
            result.push_str(&response[abs_start..]);
            return result;
        }
    }
    result.push_str(&response[search_from..]);
    result
}

/// Classify a parsed JSON value into a dispatch or answer marker.
fn classify_marker(val: &serde_json::Value) -> Option<TaskMarker> {
    let id = val.get("id").and_then(serde_json::Value::as_u64);
    let answer = val.get("answer").and_then(serde_json::Value::as_str);
    let task_type = val.get("type").and_then(serde_json::Value::as_str);
    let desc = val.get("desc").and_then(serde_json::Value::as_str);

    // Answer marker: {"id": N, "answer": "..."}
    if let (Some(id), Some(answer)) = (id, answer) {
        return Some(TaskMarker::Answer {
            id: u32::try_from(id).unwrap_or(u32::MAX),
            answer: answer.to_string(),
        });
    }
    // Dispatch marker: {"type": "...", "desc": "..."}
    if let (Some(task_type), Some(desc)) = (task_type, desc) {
        return Some(TaskMarker::Dispatch {
            task_type: task_type.to_string(),
            desc: desc.to_string(),
        });
    }
    None
}
