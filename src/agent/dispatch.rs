//! Method dispatch and simple handlers (health, config.get, chat, sessions, models).

use super::runner::{handle_agent, DEFAULT_SESSION_KEY};
use crate::protocol::{now_ms, ErrorShape};
use crate::state::{HandlerContext, RunStatus};
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
        "agent" | "chat.send" => handle_agent(ctx, params).await,
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
                "active_model" => json!(config.active_model),
                _ => serde_json::Value::Null,
            };
            return ctx.respond(json!({ "key": key, "value": value })).await;
        }
    }

    // Return full config (sanitized)
    let mut resp = json!({
        "gateway": {
            "port": config.gateway.port,
            "bind": config.gateway.bind,
        },
        "active_model": config.active_model,
    });
    if let Some(ref agents) = config.agents {
        let mut agents_obj = serde_json::Map::new();
        if let Some(ref a) = agents.main {
            agents_obj.insert("main".to_string(), agent_config_json(a));
        }
        if let Some(ref a) = agents.code {
            agents_obj.insert("code".to_string(), agent_config_json(a));
        }
        if let Some(ref a) = agents.memory {
            agents_obj.insert("memory".to_string(), agent_config_json(a));
        }
        if let Some(ref a) = agents.search {
            agents_obj.insert("search".to_string(), agent_config_json(a));
        }
        resp["agents"] = serde_json::Value::Object(agents_obj);
    }
    ctx.respond(resp).await
}

fn thinking_json(t: &crate::config::ThinkingConfig) -> serde_json::Value {
    json!({ "enabled": t.enabled, "budgetTokens": t.budget_tokens })
}

fn agent_config_json(a: &crate::config::native::AgentConfig) -> serde_json::Value {
    json!({
        "model": a.model,
        "maxTokens": a.max_tokens,
        "temperature": a.temperature,
        "topP": a.top_p,
        "frequencyPenalty": a.frequency_penalty,
        "presencePenalty": a.presence_penalty,
        "reasoningEffort": a.reasoning_effort,
        "thinking": a.thinking.as_ref().map(thinking_json),
    })
}

// ============================================================================
// Chat
// ============================================================================

async fn handle_chat_history(
    ctx: &HandlerContext<'_>,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let session_key = params
        .as_ref()
        .and_then(|p| p.get("sessionKey").and_then(|s| s.as_str()))
        .unwrap_or(DEFAULT_SESSION_KEY);

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
            run.status = RunStatus::Aborted;
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
        .map(|s| {
            json!({
                "key": s.key,
                "turnCount": s.turn_count,
                "createdAt": s.created_at_ms,
                "updatedAt": s.updated_at_ms,
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
        .models
        .iter()
        .map(|(key, m)| {
            let is_active = config.active_model.as_deref() == Some(key.as_str());
            let agents = crate::config::native::model_agent_roles(&config, key);

            json!({
                "id": m.model_id,
                "key": key,
                "provider": m.provider,
                "active": is_active,
                "agents": agents,
                "maxTokens": m.max_tokens,
                "temperature": m.temperature,
                "topP": m.top_p,
                "frequencyPenalty": m.frequency_penalty,
                "presencePenalty": m.presence_penalty,
                "reasoningEffort": m.reasoning_effort,
                "thinking": m.thinking.as_ref().map(thinking_json),
            })
        })
        .collect();

    ctx.respond(json!({
        "models": models,
    }))
    .await
}
