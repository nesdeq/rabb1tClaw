use crate::agent::runner::resolve_agent_model;
use crate::agent::search::SearchLimits;
use crate::agent::stream::collect_stream;
use crate::agent::tracker::truncate;
use crate::config::native::{AgentKind, ADVANCED_AGENT_SYSTEM_PROMPT};
use crate::provider::ChatMessage;
use crate::state::GatewayState;
use std::fmt::Write;
use std::io::Write as IoWrite;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot;
use tracing::{debug, info};

use super::tracker::{AdvancedTaskStatus, AdvancedTaskTracker, PendingQuestion};

/// Char-based threshold for triggering context compression (~70% of a reasonable token budget).
const CONTEXT_COMPRESS_CHARS: usize = 80_000;
/// Number of recent working-zone messages to keep uncompressed.
const CONTEXT_KEEP_RECENT: usize = 4;

// ============================================================================
// Task Log
// ============================================================================

/// Append-only log file for a single advanced task.
struct TaskLog {
    file: std::fs::File,
    start: Instant,
}

impl TaskLog {
    fn open(prefix: &str, task_id: &str) -> Option<Self> {
        let dir = crate::config::native::device_dir(prefix);
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("advanced_{task_id}.log"));
        let file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).ok()?;
        debug!("[ADVANCED] Task log: {}", path.display());
        Some(Self { file, start: Instant::now() })
    }

    fn log(&mut self, label: &str, body: &str) {
        let elapsed = self.start.elapsed().as_secs_f32();
        let _ = writeln!(self.file, "\n[{elapsed:.1}s] ── {label} ──");
        for line in body.lines() {
            let _ = writeln!(self.file, "  {line}");
        }
        let _ = self.file.flush();
    }

    fn divider(&mut self, step: u32) {
        let elapsed = self.start.elapsed().as_secs_f32();
        let rule = "=".repeat(72);
        let _ = writeln!(self.file, "\n{rule}\n[{elapsed:.1}s] STEP {step}\n{rule}");
        let _ = self.file.flush();
    }
}

// ============================================================================
// Directive Parsing
// ============================================================================

#[derive(Debug)]
enum Directive {
    Code(String),
    Search(String),
    Question(String),
    Done(String),
}

impl Directive {
    const fn label(&self) -> &'static str {
        match self {
            Self::Code(_) => "code",
            Self::Search(_) => "search",
            Self::Question(_) => "question",
            Self::Done(_) => "done",
        }
    }
    fn content(&self) -> &str {
        match self {
            Self::Code(s) | Self::Search(s) | Self::Question(s) | Self::Done(s) => s,
        }
    }
}

/// Extract fenced directives from LLM response text.
fn parse_directives(response: &str) -> Vec<Directive> {
    let mut directives = Vec::new();
    let mut search_from = 0;

    while search_from < response.len() {
        let remaining = &response[search_from..];

        // Find next ``` fence
        let Some(fence_start) = remaining.find("```") else { break };
        let after_fence = &remaining[fence_start + 3..];

        // Read the fence type (word after ```)
        let type_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let fence_type = after_fence[..type_end].trim();

        // Find closing ```
        let content_start = if type_end < after_fence.len() { type_end + 1 } else { type_end };
        let Some(close) = after_fence[content_start..].find("```") else {
            search_from += fence_start + 3;
            continue;
        };

        let content = after_fence[content_start..content_start + close].trim().to_string();
        search_from += fence_start + 3 + content_start + close + 3;

        match fence_type {
            "code" => directives.push(Directive::Code(content)),
            "search" => directives.push(Directive::Search(content)),
            "question" => directives.push(Directive::Question(content)),
            "done" => directives.push(Directive::Done(content)),
            _ => {} // ignore unknown fence types (e.g. ```python in reasoning)
        }
    }

    directives
}

// ============================================================================
// Context Compression
// ============================================================================

/// Compress older working-zone messages using the LLM.
/// Keeps the first `pinned_count` messages intact (system setup, task, plan).
/// Compresses all but the last `keep_recent` messages in the working zone.
async fn compress_context(
    state: &GatewayState,
    messages: &mut Vec<ChatMessage>,
    pinned_count: usize,
    keep_recent: usize,
    log: &mut Option<TaskLog>,
) {
    let working_len = messages.len() - pinned_count;
    if working_len <= keep_recent + 2 {
        return; // nothing worth compressing
    }

    let mut compress_end = messages.len() - keep_recent;
    // Ensure we compress an odd number of messages so the summary (role=user)
    // is followed by an assistant message, not another user message.
    let compress_count = compress_end - pinned_count;
    if compress_count.is_multiple_of(2) {
        compress_end -= 1;
    }
    if compress_end <= pinned_count {
        return;
    }
    let to_compress = &messages[pinned_count..compress_end];
    let num_compressed = to_compress.len();

    // Build a summary request
    let mut summary_input = String::from(
        "Summarize the following orchestration steps into a concise progress report. \
         Include: what was attempted, what succeeded, what failed, key data/file paths, \
         and current state. Be factual and brief.\n\n"
    );
    for msg in to_compress {
        let _ = writeln!(summary_input, "**{}**: {}\n", msg.role, msg.content);
    }

    // Try to compress using the advanced agent's model
    let summary = match resolve_agent_model(state, AgentKind::Advanced).await {
        Some(resolved) => {
            let req = resolved.chat_request(
                vec![ChatMessage { role: "user".to_string(), content: summary_input }],
                Some("You are a concise summarizer. Output only the progress summary.".to_string()),
            );
            match resolved.provider.chat_stream(req).await {
                Ok(rx) => match collect_stream(rx).await {
                    Ok(s) => s,
                    Err(_) => return,
                },
                Err(_) => return,
            }
        }
        None => return,
    };

    // Replace compressed messages with a single summary message
    let summary_msg = ChatMessage {
        role: "user".to_string(),
        content: format!("**Progress summary (compressed):**\n{summary}"),
    };

    messages.splice(pinned_count..compress_end, [summary_msg]);

    if let Some(l) = log {
        l.log("CONTEXT COMPRESSED", &format!(
            "{} messages → 1 summary ({} messages now)", num_compressed, messages.len()
        ));
    }
    debug!("[ADVANCED] Context compressed: {} messages → 1 summary", num_compressed);
}

// ============================================================================
// Orchestration Loop
// ============================================================================

/// Read advanced agent operational limits from config.
fn read_limits(cfg: &crate::config::GatewayConfig) -> (u32, u64, usize) {
    let ac = cfg.agent_config(AgentKind::Advanced);
    let max_steps = ac.and_then(|a| a.max_iterations)
        .unwrap_or(crate::cli::defaults::DEFAULT_ADVANCED_MAX_STEPS);
    let total_timeout = ac.and_then(|a| a.exec_timeout_secs)
        .unwrap_or(crate::cli::defaults::DEFAULT_ADVANCED_TOTAL_TIMEOUT_SECS);
    let code_max_output = ac.and_then(|a| a.max_output_tokens)
        .unwrap_or(crate::cli::defaults::DEFAULT_ADVANCED_CODE_MAX_OUTPUT_TOKENS);
    (max_steps, total_timeout, code_max_output)
}

/// Collect available API keys from provider configs + common env vars.
pub(crate) fn collect_api_env_vars(cfg: &crate::config::GatewayConfig) -> Vec<(String, String)> {
    let mut env_vars = Vec::new();
    for (name, provider) in &cfg.providers {
        let env_key = format!("{}_API_KEY", name.to_uppercase().replace('-', "_"));
        env_vars.push((env_key, provider.api_key.clone()));
    }
    // Also check common env vars
    for key in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "SERP_API_KEY"] {
        if let Ok(val) = std::env::var(key) {
            if !env_vars.iter().any(|(k, _)| k == key) {
                env_vars.push((key.to_string(), val));
            }
        }
    }
    env_vars
}

/// Format available API info for system prompt (no actual keys).
pub(crate) fn format_api_availability(env_vars: &[(String, String)]) -> String {
    if env_vars.is_empty() {
        return "No API keys are available in the sandbox environment.".to_string();
    }
    let mut out = String::from("The following API keys are available as environment variables in the code sandbox:\n");
    for (key, _) in env_vars {
        let _ = writeln!(out, "- `{key}`");
    }
    out
}

/// Run an advanced orchestration task.
pub async fn run_advanced_task(
    state: Arc<GatewayState>,
    tracker: Arc<AdvancedTaskTracker>,
    token: String,
    task_id: u32,
    description: String,
) {
    let prefix = crate::agent::session::token_prefix(&token);
    let task_id_str = task_id.to_string();

    debug!("[ADVANCED] [{}] Started: {}", task_id, description);

    let mut log = TaskLog::open(&prefix, &task_id_str);
    if let Some(ref mut l) = log {
        l.log("TASK STARTED", &description);
    }

    let task_log_max = crate::agent::tasklog::max_entries(&state).await;

    let status = match run_inner(&state, &tracker, &prefix, task_id, &description, &mut log).await {
        Ok((summary, steps)) => {
            info!("[{}] [ADVANCED] [{}] completed ({} steps)", prefix, task_id, steps);
            if let Some(ref mut l) = log {
                l.log("TASK COMPLETED", &format!("steps: {steps}\n{summary}"));
            }
            AdvancedTaskStatus::Completed { summary }
        }
        Err((e, steps)) => {
            info!("[{}] [ADVANCED] [{}] failed ({} steps)", prefix, task_id, steps);
            debug!("[ADVANCED] [{}] error: {}", task_id, e);
            if let Some(ref mut l) = log {
                l.log("TASK FAILED", &format!("steps: {steps}\nerror: {e}"));
            }
            AdvancedTaskStatus::Failed { error: e.to_string() }
        }
    };

    let event = match &status {
        AdvancedTaskStatus::Completed { summary } => format!("completed #{task_id} — {summary}"),
        AdvancedTaskStatus::Failed { error } => format!("failed #{task_id} — {error}"),
        _ => unreachable!(),
    };
    tracker.complete(&prefix, task_id, status).await;
    crate::agent::tasklog::append(&prefix, &event, task_log_max);

    // Clean up any pending questions for this task
    let mut questions = state.advanced_questions.write().await;
    questions.retain(|q| q.task_id != task_id);
}

/// Format an error message with accumulated progress notes.
fn format_error_with_progress(base: &str, progress: &[String]) -> String {
    if progress.is_empty() {
        return base.to_string();
    }
    format!("{} — progress: {}", base, progress.join("; "))
}

#[allow(clippy::too_many_lines)] // orchestration loop is inherently sequential
async fn run_inner(
    state: &Arc<GatewayState>,
    tracker: &Arc<AdvancedTaskTracker>,
    prefix: &str,
    task_id: u32,
    description: &str,
    log: &mut Option<TaskLog>,
) -> Result<(String, u32), (anyhow::Error, u32)> {
    let (max_steps, total_timeout_secs, code_max_output) = {
        let cfg = state.gateway_config.read().await;
        read_limits(&cfg)
    };
    let code_max_iters = crate::cli::defaults::DEFAULT_ADVANCED_CODE_MAX_ITERATIONS;
    let code_timeout = crate::cli::defaults::DEFAULT_ADVANCED_CODE_EXEC_TIMEOUT_SECS;

    let resolved = resolve_agent_model(state, AgentKind::Advanced).await
        .ok_or_else(|| (anyhow::anyhow!("no model configured for advanced agent"), 0))?;

    if let Some(ref mut l) = log {
        l.log("CONFIG", &format!(
            "model: {}\nmax_steps: {}\ntotal_timeout: {}s\ncode_max_iters: {}\ncode_timeout: {}s",
            resolved.model_id, max_steps, total_timeout_secs, code_max_iters, code_timeout
        ));
    }

    // Collect API keys for sandbox injection
    let env_vars = {
        let cfg = state.gateway_config.read().await;
        collect_api_env_vars(&cfg)
    };

    let api_info = format_api_availability(&env_vars);

    if let Some(ref mut l) = log {
        let key_names: Vec<&str> = env_vars.iter().map(|(k, _)| k.as_str()).collect();
        l.log("ENV VARS AVAILABLE", &key_names.join(", "));
    }

    // Build system prompt with API availability
    let system_prompt = ADVANCED_AGENT_SYSTEM_PROMPT.replace("{available_apis}", &api_info);

    // Initialize conversation with task description
    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: format!("## Task\n\n{description}"),
    }];

    // pinned_count tracks messages that must never be compressed.
    // Starts at 1 (the task message). After the first LLM turn (plan), becomes 2.
    let mut pinned_count: usize = 1;
    let start_time = Instant::now();
    // Time spent waiting for user answers — subtracted from elapsed to pause the clock.
    let mut question_wait = std::time::Duration::ZERO;
    // Brief notes after each directive — included in error messages on failure.
    let mut progress: Vec<String> = Vec::new();

    for step in 1..=max_steps {
        // Check total timeout (paused while waiting for user input)
        let effective = start_time.elapsed().saturating_sub(question_wait);
        if effective.as_secs() >= total_timeout_secs {
            let base = format!("total timeout ({total_timeout_secs}s) exceeded");
            return Err((anyhow::anyhow!("{}", format_error_with_progress(&base, &progress)), step));
        }

        if let Some(ref mut l) = log {
            l.divider(step);
        }

        // Update tracker status
        tracker.update_status(prefix, task_id, AdvancedTaskStatus::Running {
            step,
            detail: if step == 1 { "planning".to_string() } else { "executing".to_string() },
        }).await;

        // LLM call
        let request = resolved.chat_request(messages.clone(), Some(system_prompt.clone()));
        let rx = match resolved.provider.chat_stream(request).await {
            Ok(rx) => rx,
            Err(e) => return Err((e, step)),
        };
        let response = match collect_stream(rx).await {
            Ok(r) => r,
            Err(e) => return Err((e, step)),
        };

        if let Some(ref mut l) = log {
            l.log("LLM RESPONSE", &response);
        }

        // Pin the plan (first LLM response)
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: response.clone(),
        });
        if step == 1 {
            pinned_count = 2; // task + plan
        }

        // Parse directives from response
        let directives = parse_directives(&response);

        if directives.is_empty() {
            if let Some(ref mut l) = log {
                l.log("NO DIRECTIVE", "prompting LLM to emit a directive");
            }
            // No directive — LLM is just thinking/planning. Prompt it to act.
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: "Continue. Emit a ```code, ```search, ```question, or ```done directive.".to_string(),
            });
            continue;
        }

        // Process the first directive only (one per turn)
        let directive = &directives[0];

        if let Some(ref mut l) = log {
            l.log(&format!("DIRECTIVE: {}", directive.label()), directive.content());
        }

        match directive {
            Directive::Done(summary) => {
                info!("[{}] [ADVANCED] [{}] step {}: done", prefix, task_id, step);
                return Ok((summary.clone(), step));
            }

            Directive::Code(task_desc) => {
                info!("[{}] [ADVANCED] [{}] step {}: code", prefix, task_id, step);
                debug!("[ADVANCED] [{}] step {} code desc: {}", task_id, step, truncate(task_desc, 80));
                tracker.update_status(prefix, task_id, AdvancedTaskStatus::Running {
                    step,
                    detail: format!("running code: {}", truncate(task_desc, 50)),
                }).await;

                // Refresh env vars from current config (keys may have been rotated)
                let fresh_env = {
                    let cfg = state.gateway_config.read().await;
                    collect_api_env_vars(&cfg)
                };

                let result = run_code_subtask(
                    state, prefix, task_desc, &fresh_env,
                    code_max_iters, code_timeout, code_max_output,
                ).await;

                let feedback = match &result {
                    Ok(output) => {
                        if let Some(ref mut l) = log {
                            l.log("CODE RESULT: OK", output);
                        }
                        debug!("[ADVANCED] [{}] step {} code succeeded ({} chars)", task_id, step, output.len());
                        progress.push(format!("step {} code ok: {}", step, truncate(task_desc, 50)));
                        format!("**Code task succeeded.**\n\nOutput:\n{output}")
                    }
                    Err(e) => {
                        if let Some(ref mut l) = log {
                            l.log("CODE RESULT: FAILED", &e.to_string());
                        }
                        debug!("[ADVANCED] [{}] step {} code failed: {}", task_id, step, e);
                        progress.push(format!("step {} code failed: {}", step, truncate(&e.to_string(), 50)));
                        format!("**Code task failed:** {e}")
                    }
                };

                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: feedback,
                });
            }

            Directive::Search(query) => {
                let search_tag = format!("adv{task_id}s{step}");
                info!("[{}] [ADVANCED] [{}] step {}: search", prefix, task_id, step);
                debug!("[ADVANCED] [{}] step {} search query: {}", task_id, step, truncate(query, 80));
                tracker.update_status(prefix, task_id, AdvancedTaskStatus::Running {
                    step,
                    detail: format!("searching: {}", truncate(query, 50)),
                }).await;

                let result = run_search_subtask(state, &search_tag, query).await;

                let feedback = match &result {
                    Ok(context) => {
                        if let Some(ref mut l) = log {
                            l.log("SEARCH RESULT: OK", &truncate(context, 200));
                        }
                        debug!("[ADVANCED] [{}] step {} search succeeded ({} chars)", task_id, step, context.len());
                        progress.push(format!("step {} search ok: {}", step, truncate(query, 50)));
                        format!("**Search results for \"{query}\":**\n\n{context}")
                    }
                    Err(e) => {
                        if let Some(ref mut l) = log {
                            l.log("SEARCH RESULT: FAILED", &e.to_string());
                        }
                        debug!("[ADVANCED] [{}] step {} search failed: {}", task_id, step, e);
                        progress.push(format!("step {} search failed: {}", step, truncate(&e.to_string(), 50)));
                        format!("**Search failed for \"{query}\":** {e}")
                    }
                };

                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: feedback,
                });
            }

            Directive::Question(question) => {
                info!("[{}] [ADVANCED] [{}] step {}: question", prefix, task_id, step);
                debug!("[ADVANCED] [{}] step {} question: {}", task_id, step, truncate(question, 80));
                if let Some(ref mut l) = log {
                    l.log("WAITING FOR USER INPUT", question);
                }

                // Pause and wait for user answer
                tracker.update_status(prefix, task_id, AdvancedTaskStatus::NeedsInput {
                    question: question.clone(),
                }).await;

                let task_log_max = crate::agent::tasklog::max_entries(state).await;
                crate::agent::tasklog::append(prefix, &format!("asking #{task_id} — {question}"), task_log_max);

                let (answer_tx, answer_rx) = oneshot::channel::<String>();

                // Store the pending question (scoped to this device via prefix)
                {
                    let mut questions = state.advanced_questions.write().await;
                    questions.push(PendingQuestion {
                        prefix: prefix.to_string(),
                        task_id,
                        answer_tx,
                    });
                }

                // Wait for answer (no timeout — blocks until user responds)
                // Total task timeout clock pauses during this wait.
                let wait_start = Instant::now();
                let Ok(answer) = answer_rx.await else {
                    question_wait += wait_start.elapsed();
                    return Err((anyhow::anyhow!("question channel closed"), step));
                };
                question_wait += wait_start.elapsed();

                if let Some(ref mut l) = log {
                    l.log("USER ANSWERED", &answer);
                }

                progress.push(format!("step {step} question answered"));

                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!("**User answer:** {answer}"),
                });
            }
        }

        // Context compression check
        let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        if total_chars > CONTEXT_COMPRESS_CHARS {
            compress_context(state, &mut messages, pinned_count, CONTEXT_KEEP_RECENT, log).await;
        }
    }

    let base = format!("max steps ({max_steps}) exceeded");
    Err((anyhow::anyhow!("{}", format_error_with_progress(&base, &progress)), max_steps))
}

// ============================================================================
// Subtask Runners
// ============================================================================

/// Run a code subtask using the shared code execution loop.
async fn run_code_subtask(
    state: &Arc<GatewayState>,
    prefix: &str,
    description: &str,
    env_vars: &[(String, String)],
    max_iterations: u32,
    exec_timeout_secs: u64,
    max_output_tokens: usize,
) -> Result<String, anyhow::Error> {
    let task_id = format!("adv_{}", crate::protocol::short_id());
    crate::agent::code::run_code_loop(
        state, prefix, &task_id, description,
        env_vars, max_iterations, exec_timeout_secs, max_output_tokens, false,
    ).await
        .map(|(output, _)| output)
        .map_err(|(e, _)| e)
}

/// Run a search subtask using the search agent pipeline directly.
async fn run_search_subtask(
    state: &Arc<GatewayState>,
    tag: &str,
    query: &str,
) -> Result<String, anyhow::Error> {
    let limits = {
        let cfg = state.gateway_config.read().await;
        SearchLimits::from_config(&cfg)
    };

    // Call search pipeline directly (inline, not via tracker)
    crate::agent::search::run_search_inner(state, tag, query, &limits).await
}
