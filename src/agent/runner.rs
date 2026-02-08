//! Agent invocation, streaming, and model resolution.

use crate::config::native::{AgentKind, DEFAULT_SYSTEM_PROMPT};
use crate::provider::{create_provider, ChatMessage, ChatRequest, LlmProvider, StreamChunk, ThinkingParams};
use crate::protocol::{now_ms, *};
use crate::state::{GatewayState, HandlerContext, RunState, RunStatus};
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
}

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
                    "status": existing.status.as_str(),
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
                status: RunStatus::Accepted,
            },
        );
    }

    ctx.respond(json!({
        "runId": run_id,
        "status": "accepted",
        "acceptedAt": accepted_at,
    }))
    .await?;

    // Resolve agent model + provider
    let Some(resolved) = resolve_agent_model(ctx.state, AgentKind::Main).await else {
        emit_run_error(&ctx.tx, ctx.state, &run_id, &ctx.request_id, 0,
            "no LLM model configured").await;
        ctx.state.active_runs.write().await.remove(&run_id);
        return Ok(());
    };

    // System prompt is compiled-in and not user-overridable
    let (mut system_prompt, min_context_messages) = {
        let cfg = ctx.state.gateway_config.read().await;
        let mc = cfg.agent_config(AgentKind::Main)
            .and_then(|a| a.min_context_messages).unwrap_or(crate::cli::defaults::DEFAULT_MIN_CONTEXT_MESSAGES);
        let code_max = cfg.agent_config(AgentKind::Code)
            .and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_CONCURRENT);
        let search_max = cfg.agent_config(AgentKind::Search)
            .and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_CONCURRENT);
        let prompt = DEFAULT_SYSTEM_PROMPT
            .replace("{code_max_concurrent}", &code_max.to_string())
            .replace("{search_max_concurrent}", &search_max.to_string());
        (prompt, mc)
    };

    // Inject live awareness context
    let _ = write!(system_prompt,
        "\n\n<!-- Awareness: {} -->",
        Local::now().format("%A, %B %-d %Y — %H:%M %Z")
    );

    let state = ctx.state.clone();
    let device_token = ctx.device_token.clone();
    let session_key = DEFAULT_SESSION_KEY.to_string();

    // Inject persistent session memory (if any)
    if let Some(ref token) = device_token {
        if let Some(memory) = crate::agent::memory::load_session_memory(token, &session_key) {
            system_prompt.push_str("\n\n<!-- Session Memory -->\n");
            system_prompt.push_str(&memory);
        }
    }

    // Read prune ages + status token limit from config
    let (code_prune, search_prune, code_status_tokens) = {
        let cfg = state.gateway_config.read().await;
        let cc = cfg.agent_config(AgentKind::Code);
        let cp = cc.and_then(|a| a.prune_age_secs).unwrap_or(crate::cli::defaults::DEFAULT_CODE_PRUNE_AGE_SECS);
        let cst = cc.and_then(|a| a.max_status_tokens).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_STATUS_TOKENS);
        let sp = cfg.agent_config(AgentKind::Search).and_then(|a| a.prune_age_secs).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_PRUNE_AGE_SECS);
        (cp, sp, cst)
    };

    // Compute token prefix once for code/search injection
    if let Some(ref token) = device_token {
        let prefix = crate::agent::session::token_prefix(token);

        // Inject background code task status (if any)
        // get_and_mark_delivered is atomic — no gap for complete() to sneak in
        if let Some(block) = crate::agent::code::build_task_status_block(
            &state.code_task_tracker, &prefix, code_prune, code_status_tokens,
        ).await {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&block);
        }

        // Inject web search results (if any)
        if let Some(block) = crate::agent::search::build_search_results_block(
            &state.search_query_tracker, &prefix, search_prune,
        ).await {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&block);
        }
    }

    // Build messages from session history + new user message (single read lock)
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

    // Token-based FIFO: trim oldest user+assistant pairs to fit context budget
    trim_pairs_to_budget(&mut messages, resolved.context_tokens, min_context_messages);

    // NOTE: user message is persisted inside the spawned task (off the hot path)

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
    };

    tokio::spawn(async move { stream_agent_response(job).await });
    Ok(())
}

// ============================================================================
// Token-based conversation FIFO
// ============================================================================

/// Token count for a single message using tiktoken.
fn msg_tokens(m: &ChatMessage) -> u32 {
    crate::agent::tracker::count_tokens(&m.content) as u32
}

/// Trim oldest user+assistant pairs from the front until within `budget` tokens.
/// The final element (current user message) is always preserved.
/// `min_messages`: minimum messages to keep (avoids trimming too aggressively).
fn trim_pairs_to_budget(messages: &mut Vec<ChatMessage>, budget: u32, min_messages: usize) {
    // Pre-compute token counts once to avoid re-tokenizing during the trim loop
    let counts: Vec<u32> = messages.iter().map(|m| msg_tokens(m)).collect();
    let total: u32 = counts.iter().sum();
    if total <= budget || messages.len() < min_messages {
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

    // 1. Get agent-specific config (if any)
    let agent_cfg = cfg.agent_config(kind);

    // 2. Model key: agent.model → active_model
    let model_key = agent_cfg
        .and_then(|a| a.model.as_ref())
        .or(cfg.active_model.as_ref())?;

    let mc = cfg.models.get(model_key)?;
    let pc = cfg.providers.get(&mc.provider)?;
    let provider = create_provider(&pc.api, &pc.base_url, &pc.api_key);

    // 3. Base params from model config (all kinds inherit these as baseline)
    let base_think = mc.thinking.as_ref().map(ThinkingParams::from);

    // 4. Agent overrides take priority over model config
    // context_tokens: agent config → model config → 200_000
    let ctx = agent_cfg.and_then(|a| a.context_tokens)
        .or(mc.context_tokens)
        .unwrap_or(crate::cli::defaults::DEFAULT_CONTEXT_TOKENS);

    Some(ResolvedAgentModel {
        provider,
        model_id: mc.model_id.clone(),
        max_tokens:        agent_cfg.and_then(|a| a.max_tokens).or(mc.max_tokens),
        temperature:       agent_cfg.and_then(|a| a.temperature).or(mc.temperature),
        top_p:             agent_cfg.and_then(|a| a.top_p).or(mc.top_p),
        frequency_penalty: agent_cfg.and_then(|a| a.frequency_penalty).or(mc.frequency_penalty),
        presence_penalty:  agent_cfg.and_then(|a| a.presence_penalty).or(mc.presence_penalty),
        reasoning_effort:  agent_cfg.and_then(|a| a.reasoning_effort.clone()).or(mc.reasoning_effort.clone()),
        thinking:          agent_cfg.and_then(|a| a.thinking.as_ref().map(ThinkingParams::from)).or(base_think),
        context_tokens:    ctx,
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
    } = job;

    let started_at = now_ms();

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
            token, &session_key, "user", &user_message, Some(&run_id),
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
                        if !think_done && !think_buf.is_empty() {
                            full_response.push_str(&think_buf);
                        }

                        if let Some(remaining) = marker_filter.flush(&full_response) {
                            emit_stream_delta(&tx, &run_id, &session_key, seq, remaining).await;
                            seq += 1;
                        }

                        // Strip code_task and web_search markers for session + final response
                        let clean_response = crate::agent::search::strip_web_search_markers(
                            &crate::agent::code::strip_code_task_markers(&full_response)
                        );

                        if let Some(ref token) = device_token {
                            // Record assistant message (single lock + persist)
                            state.session_manager.record_message(
                                token, &session_key, "assistant", &clean_response, Some(&run_id),
                            ).await;

                            dispatch_background_agents(
                                &state, token, &session_key, &full_response,
                            ).await;
                        }

                        // Send clean response (markers stripped) to device
                        emit_stream_done(&tx, &run_id, &request_id, &session_key, seq, started_at, &clean_response).await;
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

    state.active_runs.write().await.remove(&run_id);
}

// ============================================================================
// Background Agent Dispatch
// ============================================================================

/// Parse markers from the LLM response and spawn background agents (code, search, memory).
async fn dispatch_background_agents(
    state: &Arc<GatewayState>,
    token: &str,
    session_key: &str,
    full_response: &str,
) {
    let prefix = crate::agent::session::token_prefix(token);

    // Read concurrency limits from config
    let (code_max_conc, search_max_conc, search_limits) = {
        let cfg = state.gateway_config.read().await;
        let cc = cfg.agent_config(AgentKind::Code);
        let sc = cfg.agent_config(AgentKind::Search);
        (
            cc.and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_CONCURRENT),
            sc.and_then(|a| a.max_concurrent).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_CONCURRENT),
            crate::agent::search::SearchLimits {
                max_results: sc.and_then(|a| a.max_results).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_RESULTS),
                max_news: sc.and_then(|a| a.max_news).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_NEWS),
                max_people_also_ask: sc.and_then(|a| a.max_people_also_ask).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_PEOPLE_ALSO_ASK),
                max_total_tokens: sc.and_then(|a| a.max_total_tokens).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_TOTAL_TOKENS),
                max_deep_read_urls: sc.and_then(|a| a.max_deep_read_urls).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_DEEP_READ_URLS),
                max_page_tokens: sc.and_then(|a| a.max_page_tokens).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_MAX_PAGE_TOKENS),
                fetch_timeout_secs: sc.and_then(|a| a.fetch_timeout_secs).unwrap_or(crate::cli::defaults::DEFAULT_SEARCH_FETCH_TIMEOUT_SECS),
            },
        )
    };

    // Parse code_task markers and spawn code agents
    let tasks = crate::agent::code::parse_code_task_markers(full_response);
    for desc in tasks {
        let task_id = crate::protocol::short_id();
        let tracker = state.code_task_tracker.clone();
        if tracker.register(&prefix, task_id.clone(), desc.clone(), code_max_conc).await.is_some() {
            let st = state.clone();
            let tk = token.to_string();
            tokio::spawn(async move {
                crate::agent::code::run_agent(st, tracker, tk, task_id, desc).await;
            });
        } else {
            tracing::warn!("Code agent at capacity for {}", prefix);
        }
    }

    // Parse web_search markers and spawn search agents
    let searches = crate::agent::search::parse_web_search_markers(full_response);
    for query in searches {
        let query_id = crate::protocol::short_id();
        let tracker = state.search_query_tracker.clone();
        if tracker.register(&prefix, query_id.clone(), query.clone(), search_max_conc).await.is_some() {
            let st = state.clone();
            let pfx = prefix.clone();
            tokio::spawn(async move {
                crate::agent::search::run_search(st, tracker, pfx, query_id, query, search_limits).await;
            });
        } else {
            tracing::warn!("Search agent at capacity for {}", prefix);
        }
    }

    // Fire memory subagent (background, non-blocking)
    let st = state.clone();
    let tk = token.to_string();
    let sk = session_key.to_string();
    tokio::spawn(async move {
        crate::agent::memory::maybe_run_memory_subagent(st, tk, sk).await;
    });
}
