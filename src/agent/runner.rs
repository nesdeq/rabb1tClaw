//! Agent invocation, streaming, and event emission.

use crate::provider::{create_provider, ChatMessage, ChatRequest, LlmProvider, StreamChunk};
use crate::protocol::{now_ms, *};
use crate::state::{GatewayState, HandlerContext, RunState};
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn handle_agent(ctx: &HandlerContext<'_>, params: Option<serde_json::Value>) -> Result<()> {
    let params: AgentParams = match params {
        Some(p) => serde_json::from_value(p).map_err(|e| {
            anyhow::anyhow!("invalid agent params: {}", e)
        })?,
        None => {
            return ctx
                .respond_error(ErrorShape::invalid_request("missing params"))
                .await;
        }
    };

    let run_id = params.idempotency_key.clone();
    let message = params.message.trim().to_string();

    if message.is_empty() {
        return ctx
            .respond_error(ErrorShape::invalid_request("message is empty"))
            .await;
    }

    // Check for duplicate run
    {
        let runs = ctx.state.active_runs.read().await;
        if let Some(existing) = runs.get(&run_id) {
            return ctx
                .respond(json!({
                    "runId": run_id,
                    "status": existing.status,
                    "cached": true,
                }))
                .await;
        }
    }

    // Mark run as in progress
    let accepted_at = now_ms();
    {
        let mut runs = ctx.state.active_runs.write().await;
        runs.insert(
            run_id.clone(),
            RunState {
                status: "accepted".to_string(),
            },
        );
    }

    ctx.respond(json!({
        "runId": run_id,
        "status": "accepted",
        "acceptedAt": accepted_at,
    }))
    .await?;

    // Try configured provider
    {
        let gateway_config = ctx.state.gateway_config.read().await;
        if let Some(ref provider_name) = gateway_config.active_provider {
            if let Some(provider_config) = gateway_config.providers.get(provider_name) {
                let provider = create_provider(
                    &provider_config.api,
                    &provider_config.base_url,
                    &provider_config.api_key,
                );

                return run_agent_with_provider(
                    ctx, &run_id, &message, &provider_config.model,
                    provider, params.extra_system_prompt,
                ).await;
            }
        }
    }

    // No provider configured
    emit_run_error(&ctx.tx, ctx.state, &run_id, &ctx.request_id, 0,
        "no LLM provider configured").await;
    ctx.state.active_runs.write().await.remove(&run_id);
    Ok(())
}

// ============================================================================
// Event Emission Helpers
// ============================================================================

/// Emit paired agent + chat delta events for a streaming text chunk.
async fn emit_stream_delta(
    tx: &mpsc::Sender<OutgoingFrame>,
    run_id: &str,
    session_key: &str,
    seq: u64,
    delta: &str,
    full_response: &str,
) {
    let now = now_ms();

    let agent_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": seq, "stream": "assistant",
        "ts": now, "data": { "delta": delta }
    }));
    let _ = tx.send(OutgoingFrame::Event(agent_event)).await;

    let chat_event = EventFrame::new("chat").with_payload(json!({
        "runId": run_id, "sessionKey": session_key, "seq": seq, "state": "delta",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": full_response }],
            "timestamp": now
        }
    }));
    let _ = tx.send(OutgoingFrame::Event(chat_event)).await;
}

/// Emit paired agent lifecycle end + chat final events.
async fn emit_stream_done(
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
async fn emit_run_error(
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
            run.status = "error".to_string();
        }
    }

    let error_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": seq, "stream": "lifecycle",
        "ts": ended_at, "data": { "phase": "error", "error": error_msg }
    }));
    let _ = tx.send(OutgoingFrame::Event(error_event)).await;

    let chat_error = EventFrame::new("chat").with_payload(json!({
        "runId": run_id, "sessionKey": "default:main",
        "seq": seq + 1, "state": "error", "error": error_msg
    }));
    let _ = tx.send(OutgoingFrame::Event(chat_error)).await;

    let error_response = ResponseFrame::error(
        request_id.to_string(),
        ErrorShape::unavailable(error_msg),
    );
    let _ = tx.send(OutgoingFrame::Response(error_response)).await;
}

// ============================================================================
// Agent Execution
// ============================================================================

async fn run_agent_with_provider(
    ctx: &HandlerContext<'_>,
    run_id: &str,
    message: &str,
    model: &str,
    provider: Box<dyn LlmProvider>,
    extra_system_prompt: Option<String>,
) -> Result<()> {
    let tx = ctx.tx.clone();
    let run_id = run_id.to_string();
    let message = message.to_string();
    let model = model.to_string();
    let request_id = ctx.request_id.clone();
    let state = ctx.state.clone();
    let device_token = ctx.device_token.clone();
    let session_key = "default:main".to_string();

    // Build messages from session history + new message
    let mut messages: Vec<ChatMessage> = if let Some(ref token) = device_token {
        state.session_manager.get_history(token, &session_key).await
            .into_iter()
            .map(|t| ChatMessage { role: t.role, content: t.content })
            .collect()
    } else {
        Vec::new()
    };
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: message.clone(),
    });

    // Record user message in session
    if let Some(ref token) = device_token {
        let mut session = state.session_manager.get_or_create(token, &session_key).await;
        session.add_user_message(&message, Some(&run_id));
        state.session_manager.update(token, &session_key, session).await;
    }

    tokio::spawn(async move {
        stream_agent_response(
            tx, state, provider, messages, model, run_id,
            request_id, session_key, device_token, extra_system_prompt,
        ).await;
    });

    Ok(())
}

/// Execute the streaming LLM call, emit events, and persist session.
async fn stream_agent_response(
    tx: mpsc::Sender<OutgoingFrame>,
    state: Arc<GatewayState>,
    provider: Box<dyn LlmProvider>,
    messages: Vec<ChatMessage>,
    model: String,
    run_id: String,
    request_id: String,
    session_key: String,
    device_token: Option<String>,
    extra_system_prompt: Option<String>,
) {
    let started_at = now_ms();

    // Emit lifecycle start event
    let start_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": 0, "stream": "lifecycle",
        "ts": started_at, "data": { "phase": "start", "startedAt": started_at }
    }));
    let _ = tx.send(OutgoingFrame::Event(start_event)).await;

    let system_prompt = extra_system_prompt
        .unwrap_or_else(|| "You are a helpful AI assistant.".to_string());

    let request = ChatRequest {
        model, messages,
        max_tokens: Some(4096),
        temperature: None,
        system: Some(system_prompt),
    };

    match provider.chat_stream(request).await {
        Ok(mut rx) => {
            let mut seq = 1u64;
            let mut full_response = String::new();

            while let Some(chunk) = rx.recv().await {
                match chunk {
                    StreamChunk::Text(text) => {
                        full_response.push_str(&text);
                        emit_stream_delta(&tx, &run_id, &session_key, seq, &text, &full_response).await;
                        seq += 1;
                    }
                    StreamChunk::Done => {
                        // Save assistant response to session
                        if let Some(ref token) = device_token {
                            let mut session = state.session_manager.get_or_create(token, &session_key).await;
                            session.add_assistant_message(&full_response, Some(&run_id));
                            state.session_manager.update(token, &session_key, session).await;
                        }

                        emit_stream_done(&tx, &run_id, &request_id, &session_key, seq, started_at, &full_response).await;
                        break;
                    }
                    StreamChunk::Error(err) => {
                        emit_run_error(&tx, &state, &run_id, &request_id, seq, &err).await;
                        break;
                    }
                }
            }
        }
        Err(e) => {
            emit_run_error(&tx, &state, &run_id, &request_id, 1, &e.to_string()).await;
        }
    }

    // Always clean up the active run, regardless of outcome
    state.active_runs.write().await.remove(&run_id);
}
