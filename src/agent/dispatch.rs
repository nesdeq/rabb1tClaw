//! Method dispatch and simple handlers (health, config.get, chat, sessions, models).

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
    tracing::info!("method={}", method);
    match method {
        "health" => handle_health(ctx).await,
        "config.get" => handle_config_get(ctx, params).await,
        "agent" => handle_agent(ctx, params).await,
        "chat.send" => handle_chat_send(ctx, params).await,
        "chat.history" => handle_chat_history(ctx, params).await,
        "chat.abort" => handle_chat_abort(ctx, params).await,
        "sessions.list" => handle_sessions_list(ctx).await,
        "models.list" => handle_models_list(ctx).await,
        _ => {
            ctx.respond_error(ErrorShape::not_found(format!(
                "unknown method: {}",
                method
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
// Config
// ============================================================================

async fn handle_config_get(
    ctx: &HandlerContext<'_>,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let config = ctx.state.gateway_config.read().await;

    // If a specific key is requested, return just that value
    if let Some(params) = params {
        if let Some(key) = params.get("key").and_then(|k| k.as_str()) {
            let value = match key {
                "gateway.port" => json!(config.gateway.port),
                "gateway.bind" => json!(config.gateway.bind),
                "active_provider" => json!(config.active_provider),
                _ => serde_json::Value::Null,
            };
            return ctx.respond(json!({ "key": key, "value": value })).await;
        }
    }

    // Return full config (sanitized)
    ctx.respond(json!({
        "gateway": {
            "port": config.gateway.port,
            "bind": config.gateway.bind,
        },
        "active_provider": config.active_provider,
    }))
    .await
}

// ============================================================================
// Chat
// ============================================================================

async fn handle_chat_send(
    ctx: &HandlerContext<'_>,
    params: Option<serde_json::Value>,
) -> Result<()> {
    // For now, delegate to agent handler
    handle_agent(ctx, params).await
}

async fn handle_chat_history(
    ctx: &HandlerContext<'_>,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let session_key = params
        .as_ref()
        .and_then(|p| p.get("sessionKey").and_then(|s| s.as_str()))
        .unwrap_or("default:main");

    let token = match &ctx.device_token {
        Some(t) => t,
        None => {
            return ctx.respond(json!({
                "messages": [],
                "hasMore": false,
            })).await;
        }
    };

    let history = ctx.state.session_manager.get_history(token, session_key).await;

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

async fn handle_chat_abort(
    ctx: &HandlerContext<'_>,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let run_id = params
        .and_then(|p| p.get("runId").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_default();

    // Mark run as aborted
    {
        let mut runs = ctx.state.active_runs.write().await;
        if let Some(run) = runs.get_mut(&run_id) {
            run.status = "aborted".to_string();
        }
    }

    ctx.respond(json!({
        "runId": run_id,
        "aborted": true,
    }))
    .await
}

// ============================================================================
// Sessions
// ============================================================================

async fn handle_sessions_list(ctx: &HandlerContext<'_>) -> Result<()> {
    let token = match &ctx.device_token {
        Some(t) => t,
        None => {
            return ctx.respond(json!({ "sessions": [] })).await;
        }
    };

    let sessions = ctx.state.session_manager.list_sessions(token).await;

    let list: Vec<serde_json::Value> = sessions
        .into_iter()
        .map(|(key, session)| {
            json!({
                "key": key,
                "turnCount": session.turns.len(),
                "createdAt": session.created_at_ms,
                "updatedAt": session.updated_at_ms,
            })
        })
        .collect();

    ctx.respond(json!({
        "sessions": list,
    }))
    .await
}

// ============================================================================
// Models
// ============================================================================

async fn handle_models_list(ctx: &HandlerContext<'_>) -> Result<()> {
    let config = ctx.state.gateway_config.read().await;

    let models: Vec<serde_json::Value> = config
        .providers
        .iter()
        .map(|(name, provider)| {
            json!({
                "id": provider.model,
                "name": provider.name.as_deref().unwrap_or(name),
                "provider": name,
                "api": provider.api,
            })
        })
        .collect();

    ctx.respond(json!({
        "models": models,
    }))
    .await
}
