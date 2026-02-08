//! Think-block stripping, streaming marker filtering, and stream collection.
//!
//! Reasoning models (Kimi, DeepSeek R1, QwQ, etc.) may emit `<think>…</think>`
//! blocks at the start of their output.  The main agent also emits HTML-comment
//! markers (`<!--code_task:…-->`, `<!--web_search:…-->`) that must be hidden
//! from the device.

use crate::provider::StreamChunk;

/// Result of checking a stream buffer for `<think>` block prefix.
pub(crate) enum ThinkResult {
    /// Could still become `<think>` or `</think>` — keep buffering.
    Pending,
    /// Not a think block — emit the buffer as-is.
    PassThrough,
    /// Think block stripped; remainder (possibly empty) is the real content.
    Stripped(String),
}

/// Check whether `buf` starts with a `<think>…</think>` block or orphaned `</think>`.
pub(crate) fn check_think_block(buf: &str) -> ThinkResult {
    let trimmed = buf.trim_start();
    if trimmed.is_empty() {
        return ThinkResult::Pending;
    }
    if trimmed.starts_with("<think>") {
        return match trimmed.find("</think>") {
            Some(end) => {
                let after = trimmed[end + 8..].trim_start().to_string();
                ThinkResult::Stripped(after)
            }
            None => ThinkResult::Pending,
        };
    }
    if trimmed.starts_with("</think>") {
        let after = trimmed[8..].trim_start().to_string();
        return ThinkResult::Stripped(after);
    }
    if "<think>".starts_with(trimmed) || "</think>".starts_with(trimmed) {
        return ThinkResult::Pending;
    }
    ThinkResult::PassThrough
}

/// Find the safe-to-emit length of `text`, excluding any trailing bytes
/// that could be the start of a `<!--code_task:` or `<!--web_search:` marker.
fn safe_emit_end(text: &str) -> usize {
    const MARKERS: &[&[u8]] = &[b"<!--code_task:", b"<!--web_search:"];
    let bytes = text.as_bytes();
    let mut safe = bytes.len();
    for marker in MARKERS {
        let max_check = marker.len().min(bytes.len());
        for len in (1..=max_check).rev() {
            let start = bytes.len() - len;
            if marker.starts_with(&bytes[start..]) {
                safe = safe.min(start);
                break;
            }
        }
    }
    safe
}

/// Filters `<!--code_task:...-->` and `<!--web_search:...-->` markers out of
/// the streaming delta window so they never reach the device.
pub(crate) struct MarkerFilter {
    committed: usize,
    eating: bool,
}

impl MarkerFilter {
    pub fn new() -> Self {
        Self { committed: 0, eating: false }
    }

    /// Drain safe-to-emit slices from `full_response`.  Returns each slice
    /// that should be sent as a delta.
    pub fn drain<'a>(&mut self, full_response: &'a str) -> Vec<&'a str> {
        let mut deltas = Vec::new();
        loop {
            let remaining = &full_response[self.committed..];
            if remaining.is_empty() { break; }

            if self.eating {
                if let Some(end) = remaining.find("-->") {
                    self.committed += end + 3;
                    self.eating = false;
                    continue;
                }
                break;
            }

            let earliest = [
                remaining.find("<!--code_task:"),
                remaining.find("<!--web_search:"),
            ].into_iter().flatten().min();

            if let Some(start) = earliest {
                if start > 0 {
                    deltas.push(&remaining[..start]);
                }
                self.committed += start;
                self.eating = true;
                continue;
            }

            let safe = safe_emit_end(remaining);
            if safe > 0 {
                deltas.push(&remaining[..safe]);
                self.committed += safe;
            }
            break;
        }
        deltas
    }

    /// Flush any remaining buffered text (called on stream end).
    pub fn flush<'a>(&mut self, full_response: &'a str) -> Option<&'a str> {
        if !self.eating && self.committed < full_response.len() {
            let remaining = &full_response[self.committed..];
            if !remaining.is_empty() {
                self.committed = full_response.len();
                return Some(remaining);
            }
        }
        None
    }
}

/// Drain an mpsc receiver of StreamChunks into a single string.
pub(crate) async fn collect_stream(
    mut rx: tokio::sync::mpsc::Receiver<StreamChunk>,
) -> anyhow::Result<String> {
    let mut buf = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            StreamChunk::Text(t) => buf.push_str(&t),
            StreamChunk::Done => break,
            StreamChunk::Error(e) => return Err(anyhow::anyhow!("stream error: {}", e)),
        }
    }
    Ok(buf)
}
