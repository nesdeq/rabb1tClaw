use crate::config::native::{AgentKind, CODE_AGENT_SYSTEM_PROMPT};
use crate::provider::ChatMessage;
use crate::state::GatewayState;
use std::sync::Arc;
use tracing::{debug, info};

use super::helpers::{extract_packages, extract_python_code, list_workspace};
use super::sandbox::{ensure_venv, execute_in_sandbox, pip_install, workspace_dir};
use std::path::Path;
use super::tracker::{CodeTaskStatus, CodeTaskTracker};
use crate::agent::tracker::truncate;

/// Read code agent operational limits from config.
fn read_limits(cfg: &crate::config::GatewayConfig) -> (u32, usize, u64) {
    let ac = cfg.agent_config(AgentKind::Code);
    let max_iterations = ac.and_then(|a| a.max_iterations).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_ITERATIONS);
    let max_output_tokens = ac.and_then(|a| a.max_output_tokens)
        .unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_OUTPUT_TOKENS);
    let exec_timeout_secs = ac.and_then(|a| a.exec_timeout_secs).unwrap_or(crate::cli::defaults::DEFAULT_CODE_EXEC_TIMEOUT_SECS);
    (max_iterations, max_output_tokens, exec_timeout_secs)
}

/// Run a code agent: resolve model, self-healing loop, store result.
pub async fn run_agent(
    state: Arc<GatewayState>,
    tracker: Arc<CodeTaskTracker>,
    token: String,
    task_id: u32,
    description: String,
) {
    let prefix = crate::agent::session::token_prefix(&token);

    debug!("[CODE] [{}] Started: {}", task_id, description);

    let (max_iterations, max_output_tokens, exec_timeout_secs, env_vars) = {
        let cfg = state.gateway_config.read().await;
        let (mi, mo, et) = read_limits(&cfg);
        let env = crate::agent::advanced::collect_api_env_vars(&cfg);
        drop(cfg);
        (mi, mo, et, env)
    };
    let task_log_max = crate::agent::tasklog::max_entries(&state).await;

    let result = run_code_loop(
        &state, &prefix, &format!("task_{task_id}"), &description,
        &env_vars, max_iterations, exec_timeout_secs, max_output_tokens, true,
    ).await;

    let status = match result {
        Ok((output, iterations)) => {
            info!("[{}] [CODE] [{}] completed ({} iters)", prefix, task_id, iterations);
            CodeTaskStatus::Completed { output }
        }
        Err((e, iterations)) => {
            info!("[{}] [CODE] [{}] failed ({} iters)", prefix, task_id, iterations);
            debug!("[CODE] [{}] error: {}", task_id, e);
            CodeTaskStatus::Failed { error: e.to_string() }
        }
    };

    let event = match &status {
        CodeTaskStatus::Completed { output } => format!("completed #{task_id} — {output}"),
        CodeTaskStatus::Failed { error } => format!("failed #{task_id} — {error}"),
        CodeTaskStatus::Running => unreachable!(),
    };
    tracker.complete(&prefix, task_id, status).await;
    crate::agent::tasklog::append(&prefix, &event, task_log_max);
}

/// Execute a script in the sandbox via `spawn_blocking`.
async fn exec_sandbox(
    workspace: &Path,
    python_prefix: &Path,
    script_name: &str,
    exec_timeout_secs: u64,
    env_vars: &[(String, String)],
) -> anyhow::Result<(bool, String, String)> {
    let ws = workspace.to_path_buf();
    let pfx = python_prefix.to_path_buf();
    let sn = script_name.to_string();
    let evs: Vec<(String, String)> = env_vars.to_vec();
    tokio::task::spawn_blocking(move || execute_in_sandbox(&ws, &pfx, &sn, exec_timeout_secs, &evs))
        .await
        .map_err(|e| anyhow::anyhow!("join error: {e}"))?
}

/// Format sandbox stdout into a truncated output string.
fn format_output(stdout: &str, max_output_tokens: usize) -> String {
    if stdout.trim().is_empty() {
        "(completed with no output)".to_string()
    } else {
        truncate(stdout, max_output_tokens)
    }
}

/// Shared code execution loop used by both the standalone code agent and the advanced agent.
/// Resolves model, creates workspace+venv, runs the self-healing LLM->extract->execute loop.
/// When `verify` is true, successful executions are validated by asking the LLM if the
/// output satisfies the original task (standalone code agent behavior).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) async fn run_code_loop(
    state: &Arc<GatewayState>,
    prefix: &str,
    task_id: &str,
    description: &str,
    env_vars: &[(String, String)],
    max_iterations: u32,
    exec_timeout_secs: u64,
    max_output_tokens: usize,
    verify: bool,
) -> Result<(String, u32), (anyhow::Error, u32)> {
    // Resolve code agent model + provider
    let resolved = crate::agent::runner::resolve_agent_model(state, AgentKind::Code).await
        .ok_or_else(|| (anyhow::anyhow!("no active model configured"), 0))?;

    let workspace = workspace_dir(prefix);

    // Helper to tag anyhow errors with iteration count
    macro_rules! fail {
        ($iter:expr, $e:expr) => {
            return Err(($e.into(), $iter))
        };
    }

    // Create workspace + venv (blocking I/O — run on spawn_blocking)
    let ws = workspace.clone();
    let eto = exec_timeout_secs;
    let venv_result = tokio::task::spawn_blocking(move || ensure_venv(&ws, eto)).await;
    let python_prefix = match venv_result {
        Err(e) => fail!(0, anyhow::anyhow!("join error: {}", e)),
        Ok(Err(e)) => fail!(0, e),
        Ok(Ok(prefix)) => prefix,
    };

    let listing = list_workspace(&workspace);

    // Build system prompt with env var awareness
    let api_info = crate::agent::advanced::format_api_availability(env_vars);
    let system_prompt = CODE_AGENT_SYSTEM_PROMPT.replace("{available_apis}", &api_info);

    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: format!("Task: {description}\n\nWorkspace contents:\n{listing}"),
    }];

    let mut last_error = String::new();
    let script_name = format!("{task_id}.py");

    for iteration in 1..=max_iterations {
        // LLM call
        let request = resolved.chat_request(messages.clone(), Some(system_prompt.clone()));

        let rx = match resolved.provider.chat_stream(request).await {
            Ok(rx) => rx,
            Err(e) => fail!(iteration, e),
        };
        let response = match crate::agent::stream::collect_stream(rx).await {
            Ok(r) => r,
            Err(e) => fail!(iteration, e),
        };

        // Extract packages and install
        let packages = extract_packages(&response);
        if !packages.is_empty() {
            let ws = workspace.clone();
            let pfx = python_prefix.clone();
            let eto = exec_timeout_secs;
            let evs: Vec<(String, String)> = env_vars.to_vec();
            let pip_result = tokio::task::spawn_blocking(move || pip_install(&ws, &pfx, &packages, eto, &evs)).await;
            let (ok, pip_out) = match pip_result {
                Err(e) => fail!(iteration, anyhow::anyhow!("join error: {}", e)),
                Ok(Err(e)) => fail!(iteration, e),
                Ok(Ok(v)) => v,
            };

            if !ok {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: response.clone(),
                });
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!("pip install failed:\n{}\nFix it.", truncate(&pip_out, max_output_tokens)),
                });
                last_error = pip_out;
                continue;
            }
        }

        // Extract code
        let Some(code) = extract_python_code(&response) else {
            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: response.clone(),
            });
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: "No Python code block found. Include a ```python block.".to_string(),
            });
            last_error = "no code block in response".to_string();
            continue;
        };

        // Write script to workspace
        let script_path = workspace.join(&script_name);
        if let Err(e) = std::fs::write(&script_path, &code) {
            fail!(iteration, anyhow::anyhow!(e));
        }

        // Execute in sandbox
        let (ok, stdout, stderr) = match exec_sandbox(&workspace, &python_prefix, &script_name, exec_timeout_secs, env_vars).await {
            Ok(v) => v,
            Err(e) => fail!(iteration, e),
        };

        if ok {
            let output = format_output(&stdout, max_output_tokens);

            if !verify {
                return Ok((output, iteration));
            }

            // Verify: ask LLM if results satisfy the original task
            let artifacts = list_workspace(&workspace);
            let verify_msg = format!(
                "Execution succeeded.\n\n\
                 **stdout:**\n{output}\n\n\
                 **Workspace files:**\n{artifacts}\n\n\
                 **Original task:** {description}\n\n\
                 Does the output and any created files fully satisfy the task? \
                 If yes, respond with exactly `LGTM`. \
                 If not, explain what's wrong and provide a fixed ```python block."
            );

            // On last iteration, accept whatever we got
            if iteration == max_iterations {
                return Ok((output, iteration));
            }

            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: response,
            });
            messages.push(ChatMessage {
                role: "user".to_string(),
                content: verify_msg,
            });

            let verify_request = resolved.chat_request(messages.clone(), Some(system_prompt.clone()));

            let Ok(vrx) = resolved.provider.chat_stream(verify_request).await else {
                return Ok((output, iteration));
            };
            let Ok(verdict) = crate::agent::stream::collect_stream(vrx).await else {
                return Ok((output, iteration));
            };

            if verdict.trim().starts_with("LGTM") {
                return Ok((output, iteration));
            }

            // LLM says it's not right — check if it included a fix inline
            debug!("[CODE] [{}] Verification failed, retrying", task_id);
            last_error = "verification: result did not satisfy task".to_string();

            // If the verdict contains a code fix, extract and execute it
            // directly instead of burning another LLM call.
            if let Some(fix_code) = extract_python_code(&verdict) {
                let _ = std::fs::write(workspace.join(&script_name), &fix_code);
                if let Ok((true, fix_stdout, _)) = exec_sandbox(&workspace, &python_prefix, &script_name, exec_timeout_secs, env_vars).await {
                    return Ok((format_output(&fix_stdout, max_output_tokens), iteration));
                }
            }

            messages.push(ChatMessage {
                role: "assistant".to_string(),
                content: verdict,
            });
            continue;
        }

        // Feed error back to LLM for self-healing
        let error_source = if stderr.trim().is_empty() { &stdout } else { &stderr };
        last_error = truncate(error_source, max_output_tokens);

        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: response,
        });
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: format!(
                "Execution failed (iteration {iteration}/{max_iterations}):\n{last_error}\nFix it."
            ),
        });
    }

    Err((anyhow::anyhow!("{last_error}"), max_iterations))
}
