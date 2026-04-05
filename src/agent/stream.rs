//! Think-block stripping, streaming marker filtering, and stream collection.
//!
//! Reasoning models (Kimi, `DeepSeek` R1, `QwQ`, etc.) may emit `<think>…</think>`
//! blocks at the start of their output.  The main agent also emits
//! `@@dispatch` blocks that must be hidden from the device.

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
        return trimmed.find("</think>").map_or(ThinkResult::Pending, |end| {
            ThinkResult::Stripped(trimmed[end + 8..].trim_start().to_string())
        });
    }
    if let Some(after) = trimmed.strip_prefix("</think>") {
        return ThinkResult::Stripped(after.trim_start().to_string());
    }
    if "<think>".starts_with(trimmed) || "</think>".starts_with(trimmed) {
        return ThinkResult::Pending;
    }
    ThinkResult::PassThrough
}

/// Find the safe-to-emit length of `text`, excluding any trailing bytes
/// that could be the start of a `\n@@dispatch\n` or `@@dispatch\n` marker.
fn safe_emit_end(text: &str) -> usize {
    // Ordered longest-first: "\n@@dispatch\n" always gives the most conservative
    // (smallest) hold-back, so matching it first and returning immediately is correct.
    const MARKERS: &[&[u8]] = &[b"\n@@dispatch\n", b"@@dispatch\n"];
    let bytes = text.as_bytes();

    for marker in MARKERS {
        let max_check = marker.len().min(bytes.len());
        for len in (1..=max_check).rev() {
            let start = bytes.len() - len;
            if marker.starts_with(&bytes[start..]) {
                return start;
            }
        }
    }
    bytes.len()
}

/// Filters `@@dispatch ... @@end` blocks out of the streaming delta window
/// so they never reach the device.
pub(crate) struct MarkerFilter {
    committed: usize,
    eating: bool,
}

impl MarkerFilter {
    pub const fn new() -> Self {
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
                // Look for \n@@end at end of block, or \n@@end\n mid-response
                if let Some(pos) = remaining.find("\n@@end") {
                    let end_tag_len = "\n@@end".len();
                    let block_end = pos + end_tag_len;
                    // Skip trailing newline if present
                    if block_end < remaining.len() && remaining.as_bytes()[block_end] == b'\n' {
                        self.committed += block_end + 1;
                    } else {
                        self.committed += block_end;
                    }
                    self.eating = false;
                    continue;
                }
                // Not found yet — wait for more data
                break;
            }

            // Look for @@dispatch\n (preceded by \n or at start)
            if let Some(pos) = find_dispatch_start(remaining) {
                if pos > 0 {
                    deltas.push(&remaining[..pos]);
                }
                self.committed += pos;
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
        if self.eating || self.committed >= full_response.len() {
            return None;
        }
        let remaining = &full_response[self.committed..];
        self.committed = full_response.len();
        Some(remaining)
    }
}

/// Find the start of a `@@dispatch\n` marker in `text`, which must either
/// be at position 0 or preceded by `\n`.
fn find_dispatch_start(text: &str) -> Option<usize> {
    let mut search_from = 0;
    loop {
        let pos = text[search_from..].find("@@dispatch\n")?;
        let abs = search_from + pos;
        if abs == 0 || text.as_bytes()[abs - 1] == b'\n' {
            // Include the preceding \n in what we eat (so it doesn't leak)
            return if abs > 0 && text.as_bytes()[abs - 1] == b'\n' {
                Some(abs - 1)
            } else {
                Some(abs)
            };
        }
        // False positive (e.g. "foo@@dispatch\n") — skip past
        search_from = abs + 1;
    }
}

/// Drain an mpsc receiver of `StreamChunk`s into a single string.
pub(crate) async fn collect_stream(
    mut rx: tokio::sync::mpsc::Receiver<StreamChunk>,
) -> anyhow::Result<String> {
    let mut buf = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            StreamChunk::Text(t) => buf.push_str(&t),
            StreamChunk::Done => break,
            StreamChunk::Error(e) => return Err(anyhow::anyhow!("stream error: {e}")),
        }
    }
    Ok(buf)
}
