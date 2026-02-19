//! Agent invocation, streaming, and model resolution.

use crate::config::native::{AgentKind, DEFAULT_SYSTEM_PROMPT};
use crate::provider::{create_provider, ChatMessage, ChatRequest, LlmProvider, StreamChunk, ThinkingParams};
use crate::protocol::{now_ms, AgentParams, ErrorShape, EventFrame, OutgoingFrame};
use crate::state::{GatewayState, HandlerContext};
use anyhow::Result;
use chrono::Local;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::events::{emit_run_error, emit_stream_delta, emit_stream_done};
use super::stream::{check_think_block, MarkerFilter, ThinkResult};

pub const DEFAULT_SESSION_KEY: &str = "default:main";

/// Resolved model + provider + merged params for a single agent invocation.
pub(crate) struct ResolvedAgentModel {
    pub provider: Box<dyn LlmProvider>,
    pub provider_name: String,
    pub model_id: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub thinking: Option<ThinkingParams>,
    pub context_tokens: u32,
}

impl ResolvedAgentModel {
    /// Build a `ChatRequest` from resolved params — single source of truth.
    pub fn chat_request(&self, messages: Vec<ChatMessage>, system: Option<String>) -> ChatRequest {
        ChatRequest {
            model: self.model_id.clone(),
            messages,
            system,
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            reasoning_effort: self.reasoning_effort.clone(),
            thinking: self.thinking.clone(),
        }
    }
}

/// Bundles everything needed to execute a streaming LLM call.
struct StreamJob {
    tx: mpsc::Sender<OutgoingFrame>,
    state: Arc<GatewayState>,
    provider: Box<dyn LlmProvider>,
    request: ChatRequest,
    run_id: String,
    request_id: String,
    session_key: String,
    device_token: Option<String>,
    user_message: String,
    prefix: String,
    provider_name: String,
    model_id: String,
}

#[allow(clippy::too_many_lines)]
pub async fn handle_agent(ctx: &HandlerContext<'_>, params: Option<serde_json::Value>) -> Result<()> {
    let params: AgentParams = match params {
        Some(p) => serde_json::from_value(p).map_err(|e| {
            anyhow::anyhow!("invalid agent params: {e}")
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

    // Idempotency check: if this request is already in progress, wait for it
    let new_notify = Arc::new(tokio::sync::Notify::new());
    #[allow(clippy::option_if_let_else)] // borrow checker prevents map_or_else here
    let (notify, is_duplicate) = {
        let mut requests = ctx.state.active_requests.write().await;
        if let Some(existing_notify) = requests.get(&run_id) {
            (existing_notify.clone(), true)
        } else {
            requests.insert(run_id.clone(), new_notify.clone());
            drop(requests);
            (new_notify, false)
        }
    };

    // If this is a duplicate request, wait for the original to complete
    if is_duplicate {
        tracing::debug!("[MAIN] Waiting for in-progress request: {}", run_id);
        notify.notified().await;
        // The first request will have already sent the response
        return Ok(());
    }

    let accepted_at = now_ms();
    ctx.respond(json!({
        "runId": run_id,
        "status": "accepted",
        "acceptedAt": accepted_at,
    }))
    .await?;

    // Resolve agent model + provider
    let Some(resolved) = resolve_agent_model(ctx.state, AgentKind::Main).await else {
        emit_run_error(&ctx.tx, &run_id, &ctx.request_id, 0,
            "no LLM model configured").await;
        return Ok(());
    };

    // System prompt is compiled-in and not user-overridable
    let (code_max, search_max, advanced_max) = {
        let cfg = ctx.state.gateway_config.read().await;
        (
            cfg.agent_config(AgentKind::Code)
                .and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_CONCURRENT),
            cfg.agent_config(AgentKind::Search)
                .and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_CONCURRENT),
            cfg.agent_config(AgentKind::Advanced)
                .and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_ADVANCED_MAX_CONCURRENT),
        )
    };
    let task_log_max = crate::agent::tasklog::max_entries(&ctx.state).await;
    let mut system_prompt = DEFAULT_SYSTEM_PROMPT
        .replace("{code_max_concurrent}", &code_max.to_string())
        .replace("{search_max_concurrent}", &search_max.to_string())
        .replace("{advanced_max_concurrent}", &advanced_max.to_string())
        .replace("{context_tokens}", &resolved.context_tokens.to_string())
        .replace("{task_log_max_entries}", &task_log_max.to_string());

    // Inject live awareness context
    let _ = write!(system_prompt,
        "\n\nCurrent time: {}",
        Local::now().format("%A, %B %-d %Y — %H:%M %Z")
    );

    let state = ctx.state.clone();
    let device_token = ctx.device_token.clone();
    let prefix = device_token.as_ref()
        .map(|t| crate::agent::session::token_prefix(t))
        .unwrap_or_default();
    let session_key = DEFAULT_SESSION_KEY.to_string();

    tracing::info!("[{}] [MAIN] request received", prefix);

    // Inject persistent session memory (if any)
    if let Some(ref token) = device_token {
        if let Some(memory) = crate::agent::memory::load_session_memory(token) {
            system_prompt.push_str("\n\n## Session Memory\n");
            system_prompt.push_str(&memory);
        }
    }

    // Build task context block (persisted log + live running) for user message injection
    let task_context = if device_token.is_some() {
        crate::agent::tasklog::build_task_context(&state, &prefix, task_log_max).await
    } else {
        None
    };

    // Build messages from session history + new user message (single read lock)
    let mut messages: Vec<ChatMessage> = if let Some(ref token) = device_token {
        state.session_manager.get_history(token).await
            .into_iter()
            .map(|t| ChatMessage { role: t.role, content: t.content })
            .collect()
    } else {
        Vec::new()
    };
    // Prepend @@task block to user message for LLM (NOT persisted in session)
    let llm_message = task_context.as_ref().map_or_else(
        || message.clone(),
        |ctx| {
            tracing::debug!("[MAIN] Injecting task context ({} chars)", ctx.len());
            format!("@@task\n{ctx}\n@@end\n\n{message}")
        },
    );

    messages.push(ChatMessage {
        role: "user".to_string(),
        content: llm_message,
    });

    // Token-based FIFO: trim oldest user+assistant pairs to fit context budget
    trim_pairs_to_budget(&mut messages, resolved.context_tokens);

    // NOTE: user message is persisted inside the spawned task (off the hot path)

    tracing::debug!("[MAIN] System prompt ({} chars):\n{}", system_prompt.len(), system_prompt);

    let request = resolved.chat_request(messages, Some(system_prompt));

    let job = StreamJob {
        tx: ctx.tx.clone(),
        state,
        provider: resolved.provider,
        request,
        run_id,
        request_id: ctx.request_id.clone(),
        session_key,
        device_token,
        user_message: message,
        prefix,
        provider_name: resolved.provider_name,
        model_id: resolved.model_id,
    };

    tokio::spawn(async move { stream_agent_response(job).await });
    Ok(())
}

// ============================================================================
// Token-based conversation FIFO
// ============================================================================

/// Token count for a single message using tiktoken.
fn msg_tokens(m: &ChatMessage) -> u32 {
    u32::try_from(crate::agent::tracker::count_tokens(&m.content)).unwrap_or(u32::MAX)
}

/// Trim oldest user+assistant pairs from the front until within `budget` tokens.
/// The final element (current user message) is always preserved.
fn trim_pairs_to_budget(messages: &mut Vec<ChatMessage>, budget: u32) {
    // Pre-compute token counts once to avoid re-tokenizing during the trim loop
    let counts: Vec<u32> = messages.iter().map(msg_tokens).collect();
    let total: u32 = counts.iter().sum();
    if total <= budget {
        return;
    }
    // Walk from front, accumulating pairs to drop
    let mut excess = total - budget;
    let mut pairs = 0;
    let max_pairs = (messages.len() - 1) / 2;
    while pairs < max_pairs && excess > 0 {
        let pair_cost = counts[pairs * 2] + counts[pairs * 2 + 1];
        pairs += 1;
        excess = excess.saturating_sub(pair_cost);
    }
    if pairs > 0 {
        messages.drain(..pairs * 2);
    }
}

// ============================================================================
// Shared agent model resolution
// ============================================================================

/// Resolve model + provider + merged params for a given agent kind.
///
/// Merge priority: agent override > model config > defaults.rs constant
pub(crate) async fn resolve_agent_model(
    state: &GatewayState,
    kind: AgentKind,
) -> Option<ResolvedAgentModel> {
    let cfg = state.gateway_config.read().await;
    let agent_cfg = cfg.agent_config(kind);

    let model_key = agent_cfg
        .and_then(|a| a.model.as_ref())
        .or(cfg.active_model.as_ref())?;

    let mc = cfg.models.get(model_key)?;
    let pc = cfg.providers.get(&mc.provider)?;

    // Extract all owned values while borrowing cfg
    let api = pc.api.clone();
    let base_url = pc.base_url.clone();
    let api_key = pc.api_key.clone();
    let pname = pc.name.clone().unwrap_or_else(|| {
        let mut s = api.clone();
        if let Some(c) = s.get_mut(0..1) { c.make_ascii_uppercase(); }
        s
    });
    let model_id = mc.model_id.clone();
    let base_think = mc.thinking.as_ref().map(ThinkingParams::from);
    let max_tokens = agent_cfg.and_then(|a| a.max_tokens).or(mc.max_tokens);
    let temperature = agent_cfg.and_then(|a| a.temperature).or(mc.temperature);
    let top_p = agent_cfg.and_then(|a| a.top_p).or(mc.top_p);
    let frequency_penalty = agent_cfg.and_then(|a| a.frequency_penalty).or(mc.frequency_penalty);
    let presence_penalty = agent_cfg.and_then(|a| a.presence_penalty).or(mc.presence_penalty);
    let reasoning_effort = agent_cfg.and_then(|a| a.reasoning_effort.clone()).or_else(|| mc.reasoning_effort.clone());
    let thinking = agent_cfg.and_then(|a| a.thinking.as_ref().map(ThinkingParams::from)).or(base_think);
    let ctx = agent_cfg.and_then(|a| a.context_tokens)
        .or(mc.context_tokens)
        .unwrap_or(crate::cli::defaults::DEFAULT_CONTEXT_TOKENS);
    drop(cfg);

    let provider = create_provider(&api, &base_url, &api_key);

    Some(ResolvedAgentModel {
        provider,
        provider_name: pname,
        model_id,
        max_tokens,
        temperature,
        top_p,
        frequency_penalty,
        presence_penalty,
        reasoning_effort,
        thinking,
        context_tokens: ctx,
    })
}

// ============================================================================
// Streaming Execution
// ============================================================================

/// Execute the streaming LLM call, emit events, and persist session.
async fn stream_agent_response(job: StreamJob) {
    let StreamJob {
        tx, state, provider, request, run_id, request_id,
        session_key, device_token, user_message,
        prefix, provider_name, model_id,
    } = job;

    let started_at = now_ms();
    tracing::info!("[{}] [MAIN] streaming {} {}", prefix, provider_name, model_id);

    // Emit lifecycle start event
    let start_event = EventFrame::new("agent").with_payload(json!({
        "runId": run_id, "seq": 0, "stream": "lifecycle",
        "ts": started_at, "data": { "phase": "start", "startedAt": started_at }
    }));
    let _ = tx.send(OutgoingFrame::Event(start_event)).await;

    // Start LLM call (spawns HTTP request internally, returns immediately)
    let stream_result = provider.chat_stream(request).await;

    // Record user message concurrently with in-flight HTTP request
    if let Some(ref token) = device_token {
        state.session_manager.record_message(
            token, "user", &user_message, Some(&run_id),
        ).await;
    }

    match stream_result {
        Ok(mut rx) => {
            let mut seq = 1u64;
            let mut full_response = String::new();
            let mut think_buf = String::new();
            let mut think_done = false;
            let mut marker_filter = MarkerFilter::new();

            while let Some(chunk) = rx.recv().await {
                match chunk {
                    StreamChunk::Text(text) => {
                        // Accumulate into full_response (think-block aware)
                        if think_done {
                            full_response.push_str(&text);
                        } else {
                            think_buf.push_str(&text);
                            match check_think_block(&think_buf) {
                                ThinkResult::Pending => continue,
                                ThinkResult::PassThrough => {
                                    think_done = true;
                                    let buf = std::mem::take(&mut think_buf);
                                    full_response.push_str(&buf);
                                }
                                ThinkResult::Stripped(remainder) => {
                                    think_done = true;
                                    think_buf.clear();
                                    if !remainder.is_empty() {
                                        full_response.push_str(&remainder);
                                    }
                                }
                            }
                        }

                        for delta in marker_filter.drain(&full_response) {
                            emit_stream_delta(&tx, &run_id, &session_key, seq, delta).await;
                            seq += 1;
                        }
                    }
                    StreamChunk::Done => {
                        tracing::info!("[{}] [MAIN] stream complete", prefix);

                        if !think_done && !think_buf.is_empty() {
                            full_response.push_str(&think_buf);
                        }

                        if let Some(remaining) = marker_filter.flush(&full_response) {
                            emit_stream_delta(&tx, &run_id, &session_key, seq, remaining).await;
                            seq += 1;
                        }

                        // Strip all @@dispatch blocks for session + final response
                        let clean_response = crate::agent::markers::strip_task_markers(&full_response);

                        if let Some(ref token) = device_token {
                            // Record assistant message (single lock + persist)
                            state.session_manager.record_message(
                                token, "assistant", &clean_response, Some(&run_id),
                            ).await;

                            dispatch_background_agents(
                                &state, token, &full_response,
                            ).await;
                        }

                        // Send clean response (markers stripped) to device
                        emit_stream_done(&tx, &run_id, &request_id, &session_key, seq, started_at, &clean_response).await;
                        tracing::info!("[{}] [MAIN] response sent", prefix);
                        break;
                    }
                    StreamChunk::Error(err) => {
                        emit_run_error(&tx, &run_id, &request_id, seq, &err).await;
                        break;
                    }
                }
            }
        }
        Err(e) => {
            emit_run_error(&tx, &run_id, &request_id, 1, &e.to_string()).await;
        }
    }

    // Cleanup: remove from active_requests and notify any waiting duplicates
    let notify = state.active_requests.write().await.remove(&run_id);
    if let Some(notify) = notify {
        notify.notify_waiters();
    }
}

// ============================================================================
// Background Agent Dispatch
// ============================================================================

/// Parse `@@dispatch` blocks and spawn background agents.
async fn dispatch_background_agents(
    state: &Arc<GatewayState>,
    token: &str,
    full_response: &str,
) {
    use crate::agent::markers::{parse_task_markers, TaskMarker};
    use std::collections::HashSet;

    let prefix = crate::agent::session::token_prefix(token);

    let (code_max_conc, search_max_conc, advanced_max_conc, search_limits) = {
        let cfg = state.gateway_config.read().await;
        (
            cfg.agent_config(AgentKind::Code).and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_CONCURRENT),
            cfg.agent_config(AgentKind::Search).and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_CONCURRENT),
            cfg.agent_config(AgentKind::Advanced).and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_ADVANCED_MAX_CONCURRENT),
            crate::agent::search::SearchLimits::from_config(&cfg),
        )
    };
    let task_log_max = crate::agent::tasklog::max_entries(state).await;

    let mut seen_dispatches: HashSet<(String, String)> = HashSet::new();
    for marker in parse_task_markers(full_response) {
        match marker {
            TaskMarker::Dispatch { task_type, desc } => {
                if !seen_dispatches.insert((task_type.clone(), desc.clone())) {
                    tracing::debug!("[MAIN] Skipping duplicate marker: type={}, desc={}",
                        task_type, crate::agent::tracker::truncate(&desc, 80));
                    continue;
                }
                match task_type.as_str() {
                "code" => {
                    let task_id = state.next_id();
                    let tracker = state.code_task_tracker.clone();
                    if tracker.register(&prefix, task_id, desc.clone(), code_max_conc).await.is_some() {
                        tracing::info!("[{}] [MAIN] code [{}] dispatched", prefix, task_id);
                        tracing::debug!("[MAIN] code [{}] desc: {}", task_id, desc);
                        crate::agent::tasklog::append(&prefix, &format!("dispatched #{task_id} code — {desc}"), task_log_max);
                        let st = state.clone();
                        let tk = token.to_string();
                        tokio::spawn(async move {
                            crate::agent::code::run_agent(st, tracker, tk, task_id, desc).await;
                        });
                    } else {
                        tracing::debug!("[MAIN] Code agent at capacity for {}", prefix);
                    }
                }
                "search" => {
                    let task_id = state.next_id();
                    let tracker = state.search_query_tracker.clone();
                    if tracker.register(&prefix, task_id, desc.clone(), search_max_conc).await.is_some() {
                        tracing::info!("[{}] [MAIN] search [{}] dispatched", prefix, task_id);
                        tracing::debug!("[MAIN] search [{}] desc: {}", task_id, desc);
                        crate::agent::tasklog::append(&prefix, &format!("dispatched #{task_id} search — {desc}"), task_log_max);
                        let st = state.clone();
                        let pfx = prefix.clone();
                        let sl = search_limits;
                        tokio::spawn(async move {
                            crate::agent::search::run_search(st, tracker, pfx, task_id, desc, sl).await;
                        });
                    } else {
                        tracing::debug!("[MAIN] Search agent at capacity for {}", prefix);
                    }
                }
                "advanced" => {
                    let task_id = state.next_id();
                    let tracker = state.advanced_task_tracker.clone();
                    if tracker.register(&prefix, task_id, desc.clone(), advanced_max_conc).await.is_some() {
                        tracing::info!("[{}] [MAIN] advanced [{}] dispatched", prefix, task_id);
                        tracing::debug!("[MAIN] advanced [{}] desc: {}", task_id, desc);
                        crate::agent::tasklog::append(&prefix, &format!("dispatched #{task_id} advanced — {desc}"), task_log_max);
                        let st = state.clone();
                        let tk = token.to_string();
                        tokio::spawn(async move {
                            crate::agent::advanced::run_advanced_task(st, tracker, tk, task_id, desc).await;
                        });
                    } else {
                        tracing::debug!("[MAIN] Advanced agent at capacity for {}", prefix);
                    }
                }
                other => {
                    tracing::debug!("[MAIN] Unknown task type in marker: {}", other);
                }
                }
            }
            TaskMarker::Answer { id, answer } => {
                if crate::agent::advanced::answer_pending_question(state, &prefix, id, &answer).await {
                    crate::agent::tasklog::append(&prefix, &format!("answered #{id} — {answer}"), task_log_max);
                }
            }
        }
    }

    // Fire memory subagent (background, non-blocking)
    let st = state.clone();
    let tk = token.to_string();
    tokio::spawn(async move {
        crate::agent::memory::maybe_run_memory_subagent(st, tk).await;
    });
}
