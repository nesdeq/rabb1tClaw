use crate::config::native::{AgentKind, CODE_AGENT_SYSTEM_PROMPT};
use crate::provider::ChatMessage;
use crate::state::GatewayState;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use super::sandbox::{ensure_venv, execute_in_sandbox, pip_install, workspace_dir};
use super::tracker::{CodeTaskStatus, CodeTaskTracker};
use crate::agent::tracker::truncate;

/// Read code agent operational limits from config.
fn read_limits(cfg: &crate::config::GatewayConfig) -> (u32, usize, u64) {
    let ac = cfg.agent_config(AgentKind::Code);
    let max_iterations = ac.and_then(|a| a.max_iterations).unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_ITERATIONS);
    let max_output_tokens = ac.and_then(|a| a.max_output_tokens)
        .or_else(|| ac.and_then(|a| a.max_output_chars)) // deprecated alias
        .unwrap_or(crate::cli::defaults::DEFAULT_CODE_MAX_OUTPUT_TOKENS);
    let exec_timeout_secs = ac.and_then(|a| a.exec_timeout_secs).unwrap_or(crate::cli::defaults::DEFAULT_CODE_EXEC_TIMEOUT_SECS);
    (max_iterations, max_output_tokens, exec_timeout_secs)
}

/// Extract the first ```python fenced code block from an LLM response.
fn extract_python_code(response: &str) -> Option<String> {
    let marker = "```python";
    let start = response.find(marker)?;
    let code_start = start + marker.len();
    // Skip optional newline after marker
    let code_start = if response[code_start..].starts_with('\n') {
        code_start + 1
    } else {
        code_start
    };
    let end = response[code_start..].find("```")?;
    let code = response[code_start..code_start + end].trim_end();
    if code.is_empty() {
        return None;
    }
    Some(code.to_string())
}

/// Extract package names from a ### Packages section.
fn extract_packages(response: &str) -> Vec<String> {
    // Look for ### Packages section, then a ``` block
    let header = "### Packages";
    let Some(idx) = response.find(header) else {
        return Vec::new();
    };
    let after = &response[idx + header.len()..];

    // Find the fenced block
    let Some(fence_start) = after.find("```") else {
        return Vec::new();
    };
    let inner_start = fence_start + 3;
    // Skip optional language tag on the fence line
    let inner_start = match after[inner_start..].find('\n') {
        Some(nl) => inner_start + nl + 1,
        None => return Vec::new(),
    };
    let Some(fence_end) = after[inner_start..].find("```") else {
        return Vec::new();
    };

    let block = &after[inner_start..inner_start + fence_end];
    block
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// List workspace contents (top-level only, for context).
fn list_workspace(workspace: &Path) -> String {
    let mut listing = String::new();
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".venv" {
                continue;
            }
            listing.push_str(&name.to_string_lossy());
            listing.push('\n');
        }
    }
    if listing.is_empty() {
        "(empty)".to_string()
    } else {
        listing
    }
}

/// Run a code agent: resolve model, self-healing loop, store result.
pub async fn run_agent(
    state: Arc<GatewayState>,
    tracker: Arc<CodeTaskTracker>,
    token: String,
    task_id: String,
    description: String,
) {
    let prefix = crate::agent::session::token_prefix(&token);

    info!("Code agent started: [{}] {}", task_id, description);

    let result = run_inner(&state, &prefix, &task_id, &description).await;

    let status = match result {
        Ok((output, iterations)) => {
            info!("Code agent [{}] completed in {} iterations", task_id, iterations);
            CodeTaskStatus::Completed { output, iterations }
        }
        Err((e, iterations)) => {
            warn!("Code agent [{}] failed after {} iterations: {}", task_id, iterations, e);
            CodeTaskStatus::Failed {
                error: e.to_string(),
                iterations,
            }
        }
    };

    tracker.complete(&prefix, &task_id, status).await;
}

async fn run_inner(
    state: &Arc<GatewayState>,
    prefix: &str,
    task_id: &str,
    description: &str,
) -> Result<(String, u32), (anyhow::Error, u32)> {
    // Read operational limits from config
    let (max_iterations, max_output_tokens, exec_timeout_secs) = {
        let cfg = state.gateway_config.read().await;
        read_limits(&cfg)
    };

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

    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: format!("Task: {}\n\nWorkspace contents:\n{}", description, listing),
    }];

    let mut last_error = String::new();

    for iteration in 1..=max_iterations {
        // LLM call
        let request = resolved.chat_request(messages.clone(), Some(CODE_AGENT_SYSTEM_PROMPT.to_string()));

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
            let pip_result = tokio::task::spawn_blocking(move || pip_install(&ws, &pfx, &packages, eto)).await;
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
        let script_name = format!("task_{}.py", task_id);
        let script_path = workspace.join(&script_name);
        if let Err(e) = std::fs::write(&script_path, &code) {
            fail!(iteration, anyhow::anyhow!(e));
        }

        // Execute in sandbox
        let ws = workspace.clone();
        let pfx = python_prefix.clone();
        let sn = script_name.clone();
        let eto = exec_timeout_secs;
        let exec_result =
            tokio::task::spawn_blocking(move || execute_in_sandbox(&ws, &pfx, &sn, eto)).await;
        let (ok, stdout, stderr) = match exec_result {
            Err(e) => fail!(iteration, anyhow::anyhow!("join error: {}", e)),
            Ok(Err(e)) => fail!(iteration, e),
            Ok(Ok(v)) => v,
        };

        if ok {
            let output = if stdout.trim().is_empty() {
                "(completed with no output)".to_string()
            } else {
                truncate(&stdout, max_output_tokens)
            };

            // Verify: ask LLM if results satisfy the original task
            let artifacts = list_workspace(&workspace);
            let verify_msg = format!(
                "Execution succeeded.\n\n\
                 **stdout:**\n{}\n\n\
                 **Workspace files:**\n{}\n\n\
                 **Original task:** {}\n\n\
                 Does the output and any created files fully satisfy the task? \
                 If yes, respond with exactly `LGTM`. \
                 If not, explain what's wrong and provide a fixed ```python block.",
                output, artifacts, description
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

            let verify_request = resolved.chat_request(messages.clone(), Some(CODE_AGENT_SYSTEM_PROMPT.to_string()));

            let vrx = match resolved.provider.chat_stream(verify_request).await {
                Ok(rx) => rx,
                Err(_) => return Ok((output, iteration)),
            };
            let verdict = match crate::agent::stream::collect_stream(vrx).await {
                Ok(r) => r,
                Err(_) => return Ok((output, iteration)),
            };

            if verdict.trim().starts_with("LGTM") {
                return Ok((output, iteration));
            }

            // LLM says it's not right — check if it included a fix inline
            info!("Code agent [{}] verification failed, retrying", task_id);
            last_error = "verification: result did not satisfy task".to_string();

            // If the verdict contains a code fix, extract and execute it
            // directly instead of burning another LLM call.
            if let Some(fix_code) = extract_python_code(&verdict) {
                let sn = format!("task_{}.py", task_id);
                let _ = std::fs::write(workspace.join(&sn), &fix_code);

                let ws = workspace.clone();
                let pfx = python_prefix.clone();
                let eto = exec_timeout_secs;
                let fix_result =
                    tokio::task::spawn_blocking(move || execute_in_sandbox(&ws, &pfx, &sn, eto)).await;
                if let Ok(Ok((true, fix_stdout, _))) = fix_result {
                    let fix_output = if fix_stdout.trim().is_empty() {
                        "(completed with no output)".to_string()
                    } else {
                        truncate(&fix_stdout, max_output_tokens)
                    };
                    return Ok((fix_output, iteration));
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
                "Execution failed (iteration {}/{}):\n{}\nFix it.",
                iteration, max_iterations, last_error
            ),
        });
    }

    Err((anyhow::anyhow!("{}", last_error), max_iterations))
}
