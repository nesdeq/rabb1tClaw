// Method dispatch and simple handlers (health, chat).

use super::runner::handle_agent;
use crate::protocol::{now_ms, ErrorShape};
use crate::state::HandlerContext;
use anyhow::Result;
use serde_json::json;

// ============================================================================
// Method Dispatch
// ============================================================================

/// Dispatch a method call to the appropriate handler
pub async fn dispatch_method(
    ctx: &HandlerContext<'_>,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<()> {
    tracing::debug!("[MAIN] method={}", method);
    match method {
        "health" => handle_health(ctx).await,
        "agent" | "chat.send" => handle_agent(ctx, params).await,
        "chat.history" => handle_chat_history(ctx, params).await,
        _ => {
            ctx.respond_error(ErrorShape::not_found(format!(
                "unknown method: {method}"
            )))
            .await
        }
    }
}

// ============================================================================
// Health
// ============================================================================

async fn handle_health(ctx: &HandlerContext<'_>) -> Result<()> {
    let uptime_ms = now_ms() - ctx.state.started_at;

    ctx.respond(json!({
        "ok": true,
        "uptimeMs": uptime_ms,
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .await
}

// ============================================================================
// Chat
// ============================================================================

async fn handle_chat_history(
    ctx: &HandlerContext<'_>,
    _params: Option<serde_json::Value>,
) -> Result<()> {
    let Some(token) = &ctx.device_token else {
        return ctx.respond(json!({
            "messages": [],
            "hasMore": false,
        })).await;
    };

    let history = ctx.state.session_manager.get_history(token).await;

    let messages: Vec<serde_json::Value> = history
        .iter()
        .map(|turn| {
            json!({
                "role": turn.role,
                "content": [{ "type": "text", "text": turn.content }],
                "timestamp": turn.timestamp_ms,
                "runId": turn.run_id,
            })
        })
        .collect();

    ctx.respond(json!({
        "messages": messages,
        "hasMore": false,
    }))
    .await
}
