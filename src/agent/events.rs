//! Streaming event emission helpers for agent responses.

use crate::protocol::{now_ms, ErrorShape, EventFrame, OutgoingFrame, ResponseFrame};
use crate::state::{GatewayState, RunStatus};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::runner::DEFAULT_SESSION_KEY;

/// Emit paired agent + chat delta events for a streaming text chunk.
/// Only sends the incremental delta — full text is sent in the final event.
pub(crate) async fn emit_stream_delta(
    tx: &mpsc::Sender<OutgoingFrame>,
    run_id: &str,
    session_key: &str,
    seq: u64,
    delta: &str,
) {
    let now = now_ms();

    let agent_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": seq, "stream": "assistant",
        "ts": now, "data": { "delta": delta }
    }));
    let _ = tx.send(OutgoingFrame::Event(agent_event)).await;

    let chat_event = EventFrame::new("chat").with_payload(json!({
        "runId": run_id, "sessionKey": session_key, "seq": seq,
        "state": "delta", "delta": delta
    }));
    let _ = tx.send(OutgoingFrame::Event(chat_event)).await;
}

/// Emit paired agent lifecycle end + chat final events.
pub(crate) async fn emit_stream_done(
    tx: &mpsc::Sender<OutgoingFrame>,
    run_id: &str,
    request_id: &str,
    session_key: &str,
    seq: u64,
    started_at: u64,
    full_response: &str,
) {
    let ended_at = now_ms();

    let end_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": seq, "stream": "lifecycle",
        "ts": ended_at, "data": { "phase": "end", "startedAt": started_at, "endedAt": ended_at }
    }));
    let _ = tx.send(OutgoingFrame::Event(end_event)).await;

    let chat_final = EventFrame::new("chat").with_payload(json!({
        "runId": run_id, "sessionKey": session_key, "seq": seq + 1, "state": "final",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": full_response }],
            "timestamp": ended_at
        }
    }));
    let _ = tx.send(OutgoingFrame::Event(chat_final)).await;

    let final_response = ResponseFrame::ok(
        request_id.to_string(),
        json!({
            "runId": run_id, "status": "ok", "summary": "completed",
            "result": { "assistantTexts": [full_response] }
        }),
    );
    let _ = tx.send(OutgoingFrame::Response(final_response)).await;
}

/// Emit error events (lifecycle + chat) and send error response.
pub(crate) async fn emit_run_error(
    tx: &mpsc::Sender<OutgoingFrame>,
    state: &Arc<GatewayState>,
    run_id: &str,
    request_id: &str,
    seq: u64,
    error_msg: &str,
) {
    let ended_at = now_ms();

    // Update run state to error
    {
        let mut runs = state.active_runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.status = RunStatus::Error;
        }
    }

    let error_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": seq, "stream": "lifecycle",
        "ts": ended_at, "data": { "phase": "error", "error": error_msg }
    }));
    let _ = tx.send(OutgoingFrame::Event(error_event)).await;

    let chat_error = EventFrame::new("chat").with_payload(json!({
        "runId": run_id, "sessionKey": DEFAULT_SESSION_KEY,
        "seq": seq + 1, "state": "error", "error": error_msg
    }));
    let _ = tx.send(OutgoingFrame::Event(chat_error)).await;

    let error_response = ResponseFrame::error(
        request_id.to_string(),
        ErrorShape::unavailable(error_msg),
    );
    let _ = tx.send(OutgoingFrame::Response(error_response)).await;
}
