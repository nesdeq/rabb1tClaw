//! Integration tests: probe real APIs to verify parameter support and response format.
//!
//! These tests call live APIs and require valid keys in ~/.rabb1tclaw/config.yaml.
//! Run with:  cargo test --test testmodels -- --ignored --nocapture
//!
//! Each test sends a minimal prompt and prints:
//!   - HTTP status
//!   - Full first SSE data line (raw JSON) to inspect field structure
//!   - Whether reasoning_content / thinking fields appear
//!   - Any errors from unsupported params

use reqwest::Client;
use serde_json::{json, Value};

// ============================================================================
// Config loading (standalone — no crate dependency needed)
// ============================================================================

fn load_test_config() -> Value {
    // Load .env from project root
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    if env_path.exists() {
        for item in dotenvy::from_path_iter(&env_path).expect("Failed to read .env") {
            if let Ok((k, v)) = item {
                std::env::set_var(&k, &v);
            }
        }
    }

    let openai_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let deepinfra_key = std::env::var("DEEPINFRA_API_KEY").unwrap_or_default();

    assert!(
        !openai_key.is_empty() || !anthropic_key.is_empty() || !deepinfra_key.is_empty(),
        "No API keys found. Put at least one in .env"
    );

    let mut providers = serde_json::Map::new();
    if !openai_key.is_empty() {
        providers.insert("openai".into(), json!({
            "api": "openai",
            "base_url": "https://api.openai.com/v1",
            "api_key": openai_key,
        }));
    }
    if !anthropic_key.is_empty() {
        providers.insert("anthropic".into(), json!({
            "api": "anthropic",
            "base_url": "https://api.anthropic.com/v1",
            "api_key": anthropic_key,
        }));
    }
    if !deepinfra_key.is_empty() {
        providers.insert("deepinfra".into(), json!({
            "api": "openai",
            "base_url": "https://api.deepinfra.com/v1/openai",
            "api_key": deepinfra_key,
        }));
    }

    json!({ "providers": providers })
}

fn get_provider(config: &Value, name: &str) -> (String, String) {
    let p = &config["providers"][name];
    let base_url = p["base_url"].as_str().unwrap().trim_end_matches('/').to_string();
    let api_key = p["api_key"].as_str().unwrap().to_string();
    (base_url, api_key)
}

// ============================================================================
// HTTP helpers
// ============================================================================

async fn post_chat(
    client: &Client,
    base_url: &str,
    api_key: &str,
    api_type: &str,
    body: &Value,
) -> (u16, String) {
    let req = match api_type {
        "anthropic" => {
            let url = format!("{}/messages", base_url);
            client
                .post(&url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
        }
        _ => {
            let url = format!("{}/chat/completions", base_url);
            client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
        }
    };

    let resp = req.json(body).send().await.expect("HTTP request failed");
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    (status, text)
}

/// Send a streaming request, collect first N SSE data lines, return (status, lines).
async fn post_chat_stream(
    client: &Client,
    base_url: &str,
    api_key: &str,
    api_type: &str,
    body: &Value,
    max_lines: usize,
) -> (u16, Vec<String>) {
    let req = match api_type {
        "anthropic" => {
            let url = format!("{}/messages", base_url);
            client
                .post(&url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
        }
        _ => {
            let url = format!("{}/chat/completions", base_url);
            client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
        }
    };

    let resp = req.json(body).send().await.expect("HTTP request failed");
    let status = resp.status().as_u16();

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return (status, vec![text]);
    }

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut lines = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("stream error");
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find('\n') {
            let line: String = buffer.drain(..=pos).collect();
            let line = line.trim().to_string();
            if line.starts_with("data: ") {
                let data = line.strip_prefix("data: ").unwrap().to_string();
                if data == "[DONE]" {
                    lines.push("[DONE]".to_string());
                    return (status, lines);
                }
                lines.push(data);
                if lines.len() >= max_lines {
                    return (status, lines);
                }
            }
        }
    }

    (status, lines)
}

fn print_header(label: &str) {
    println!("\n{}", "=".repeat(72));
    println!("  {}", label);
    println!("{}", "=".repeat(72));
}

fn print_test(label: &str, status: u16, body: &str) {
    let ok = if status >= 200 && status < 300 { "OK" } else { "FAIL" };
    println!("\n--- {} [{}] {} ---", label, status, ok);

    // Pretty-print truncated JSON
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        // Print first 600 chars
        if pretty.len() > 600 {
            println!("{}\n  ... (truncated, {} bytes total)", &pretty[..600], pretty.len());
        } else {
            println!("{}", pretty);
        }
    } else {
        let truncated = if body.len() > 400 { &body[..400] } else { body };
        println!("{}", truncated);
    }
}

fn print_stream_test(label: &str, status: u16, lines: &[String]) {
    let ok = if status >= 200 && status < 300 { "OK" } else { "FAIL" };
    println!("\n--- {} (stream) [{}] {} ---", label, status, ok);

    if status >= 400 {
        for line in lines {
            println!("  ERROR: {}", &line[..line.len().min(300)]);
        }
        return;
    }

    let mut has_content = false;
    let mut has_reasoning_content = false;
    let mut has_reasoning = false;
    let mut has_think_tag = false;
    let mut has_thinking_block = false;
    let mut content_sample = String::new();
    let mut reasoning_sample = String::new();

    for (i, line) in lines.iter().enumerate() {
        if line == "[DONE]" {
            println!("  [{}] [DONE]", i);
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            // OpenAI-compatible format
            if let Some(delta) = v.pointer("/choices/0/delta") {
                if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                    has_content = true;
                    if content_sample.len() < 100 {
                        content_sample.push_str(c);
                    }
                    if c.contains("<think>") || c.contains("</think>") {
                        has_think_tag = true;
                    }
                }
                if let Some(r) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
                    has_reasoning_content = true;
                    if reasoning_sample.len() < 100 {
                        reasoning_sample.push_str(r);
                    }
                }
                if let Some(r) = delta.get("reasoning").and_then(|r| r.as_str()) {
                    has_reasoning = true;
                    if reasoning_sample.len() < 100 {
                        reasoning_sample.push_str(r);
                    }
                }
            }
            // Anthropic format
            if let Some(typ) = v.get("type").and_then(|t| t.as_str()) {
                if typ == "content_block_start" {
                    if let Some(bt) = v.pointer("/content_block/type").and_then(|t| t.as_str()) {
                        if bt == "thinking" {
                            has_thinking_block = true;
                        }
                    }
                }
                if typ == "content_block_delta" {
                    if let Some(dt) = v.pointer("/delta/type").and_then(|t| t.as_str()) {
                        if dt == "thinking_delta" {
                            has_thinking_block = true;
                            if let Some(t) = v.pointer("/delta/thinking").and_then(|t| t.as_str()) {
                                if reasoning_sample.len() < 100 {
                                    reasoning_sample.push_str(t);
                                }
                            }
                        }
                        if dt == "text_delta" {
                            if let Some(t) = v.pointer("/delta/text").and_then(|t| t.as_str()) {
                                has_content = true;
                                if content_sample.len() < 100 {
                                    content_sample.push_str(t);
                                }
                            }
                        }
                    }
                }
            }

            // Print first 3 raw lines for inspection
            if i < 3 {
                let raw = serde_json::to_string(&v).unwrap();
                println!("  [{}] {}", i, &raw[..raw.len().min(200)]);
            }
        }
    }

    println!("\n  Collected {} SSE data lines", lines.len());
    println!("  has delta.content:           {}", has_content);
    println!("  has delta.reasoning_content: {}", has_reasoning_content);
    println!("  has delta.reasoning:         {}", has_reasoning);
    println!("  has <think> tags in content: {}", has_think_tag);
    println!("  has Anthropic thinking block:{}", has_thinking_block);
    if !content_sample.is_empty() {
        println!("  content sample: {:?}", &content_sample[..content_sample.len().min(80)]);
    }
    if !reasoning_sample.is_empty() {
        println!("  reasoning sample: {:?}", &reasoning_sample[..reasoning_sample.len().min(80)]);
    }
}

const PROMPT: &str = "What is 25 * 37? Reply with just the number.";

// ============================================================================
// Anthropic: claude-sonnet-4-5
// ============================================================================

#[tokio::test]
#[ignore]
async fn anthropic_sonnet_baseline() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "anthropic");
    let client = Client::new();

    print_header("Anthropic claude-sonnet-4-5 — baseline (no thinking)");

    let body = json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 100,
        "temperature": 0.7,
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "anthropic", &body, 30).await;
    print_stream_test("baseline", status, &lines);
}

#[tokio::test]
#[ignore]
async fn anthropic_sonnet_thinking() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "anthropic");
    let client = Client::new();

    print_header("Anthropic claude-sonnet-4-5 — thinking enabled");

    let body = json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 8000,
        "thinking": {"type": "enabled", "budget_tokens": 5000},
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "anthropic", &body, 50).await;
    print_stream_test("thinking enabled", status, &lines);
}

#[tokio::test]
#[ignore]
async fn anthropic_sonnet_unsupported_params() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "anthropic");
    let client = Client::new();

    print_header("Anthropic claude-sonnet-4-5 — unsupported params");

    // Test frequency_penalty and presence_penalty (not in Anthropic API)
    let body = json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 100,
        "frequency_penalty": 0.5,
        "presence_penalty": 0.5,
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "anthropic", &body).await;
    print_test("frequency_penalty + presence_penalty", status, &text);

    // Test reasoning_effort
    let body = json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 100,
        "reasoning_effort": "high",
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "anthropic", &body).await;
    print_test("reasoning_effort", status, &text);
}

// ============================================================================
// OpenAI: gpt-4o (non-reasoning)
// ============================================================================

#[tokio::test]
#[ignore]
async fn openai_gpt4o_baseline() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "openai");
    let client = Client::new();

    print_header("OpenAI gpt-4o — baseline (all standard params)");

    let body = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are a calculator."},
            {"role": "user", "content": PROMPT}
        ],
        "max_tokens": 100,
        "temperature": 0.7,
        "top_p": 0.9,
        "frequency_penalty": 0.5,
        "presence_penalty": 0.5,
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 20).await;
    print_stream_test("all standard params", status, &lines);
}

#[tokio::test]
#[ignore]
async fn openai_gpt4o_reasoning_params() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "openai");
    let client = Client::new();

    print_header("OpenAI gpt-4o — reasoning params (should fail)");

    // max_completion_tokens on non-reasoning model
    let body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_completion_tokens": 100,
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("max_completion_tokens", status, &text);

    // reasoning_effort on non-reasoning model
    let body = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 100,
        "reasoning_effort": "medium",
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("reasoning_effort", status, &text);
}

// ============================================================================
// OpenAI: gpt-5.2 (reasoning)
// ============================================================================

#[tokio::test]
#[ignore]
async fn openai_gpt52_baseline() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "openai");
    let client = Client::new();

    print_header("OpenAI gpt-5.2 — baseline with max_completion_tokens");

    let body = json!({
        "model": "gpt-5.2",
        "messages": [
            {"role": "system", "content": "You are a calculator."},
            {"role": "user", "content": PROMPT}
        ],
        "max_completion_tokens": 200,
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 30).await;
    print_stream_test("max_completion_tokens", status, &lines);
}

#[tokio::test]
#[ignore]
async fn openai_gpt52_reasoning_effort() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "openai");
    let client = Client::new();

    print_header("OpenAI gpt-5.2 — reasoning_effort levels");

    for effort in &["low", "medium", "high"] {
        let body = json!({
            "model": "gpt-5.2",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_completion_tokens": 200,
            "reasoning_effort": effort,
            "stream": true,
        });

        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 20).await;
        print_stream_test(&format!("reasoning_effort={}", effort), status, &lines);
    }
}

#[tokio::test]
#[ignore]
async fn openai_gpt52_unsupported_params() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "openai");
    let client = Client::new();

    print_header("OpenAI gpt-5.2 — unsupported param tests");

    // max_tokens instead of max_completion_tokens
    let body = json!({
        "model": "gpt-5.2",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 100,
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("max_tokens (instead of max_completion_tokens)", status, &text);

    // temperature on reasoning model
    let body = json!({
        "model": "gpt-5.2",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_completion_tokens": 100,
        "temperature": 0.7,
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("temperature on reasoning model", status, &text);
}

// ============================================================================
// DeepInfra: moonshotai/Kimi-K2.5
// ============================================================================

#[tokio::test]
#[ignore]
async fn deepinfra_kimi_k25_baseline() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "deepinfra");
    let client = Client::new();

    print_header("DeepInfra moonshotai/Kimi-K2.5 — baseline");

    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [
            {"role": "system", "content": "You are a calculator."},
            {"role": "user", "content": PROMPT}
        ],
        "max_tokens": 200,
        "temperature": 1.0,
        "top_p": 0.95,
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
    print_stream_test("baseline (temp=1.0, top_p=0.95)", status, &lines);
}

#[tokio::test]
#[ignore]
async fn deepinfra_kimi_k25_all_params() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "deepinfra");
    let client = Client::new();

    print_header("DeepInfra moonshotai/Kimi-K2.5 — all params");

    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 200,
        "temperature": 0.7,
        "top_p": 0.9,
        "frequency_penalty": 0.3,
        "presence_penalty": 0.3,
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
    print_stream_test("all standard params", status, &lines);
}

#[tokio::test]
#[ignore]
async fn deepinfra_kimi_k25_reasoning_params() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "deepinfra");
    let client = Client::new();

    print_header("DeepInfra moonshotai/Kimi-K2.5 — reasoning param tests");

    // max_completion_tokens (OpenAI-style, may not work on DeepInfra)
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_completion_tokens": 200,
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("max_completion_tokens", status, &text);

    // reasoning_effort
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 200,
        "reasoning_effort": "high",
        "stream": false,
    });

    let (status, text) = post_chat(&client, &base_url, &api_key, "openai", &body).await;
    print_test("reasoning_effort", status, &text);
}

#[tokio::test]
#[ignore]
async fn deepinfra_kimi_k25_thinking_toggle() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "deepinfra");
    let client = Client::new();

    print_header("DeepInfra moonshotai/Kimi-K2.5 — thinking toggle variants");

    // chat_template_kwargs style (vLLM)
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 200,
        "chat_template_kwargs": {"enable_thinking": true},
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
    print_stream_test("chat_template_kwargs.enable_thinking=true", status, &lines);

    // chat_template_kwargs disable
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 200,
        "chat_template_kwargs": {"enable_thinking": false},
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
    print_stream_test("chat_template_kwargs.enable_thinking=false", status, &lines);

    // DeepSeek-style thinking object
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 200,
        "thinking": {"type": "enabled"},
        "stream": true,
    });

    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
    print_stream_test("thinking={type:enabled}", status, &lines);
}

// ============================================================================
// DeepInfra: thinking level / budget probes
// ============================================================================

#[tokio::test]
#[ignore]
async fn deepinfra_thinking_levels_probe() {
    let config = load_test_config();
    let (base_url, api_key) = get_provider(&config, "deepinfra");
    let client = Client::new();

    print_header("DeepInfra Kimi-K2.5 — thinking level/budget probes");

    // 1. reasoning_effort (OpenAI-style) with thinking on
    for effort in &["low", "medium", "high"] {
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "reasoning_effort": effort,
            "chat_template_kwargs": {"enable_thinking": true},
            "stream": true,
        });
        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 60).await;
        print_stream_test(&format!("reasoning_effort={} + thinking=true", effort), status, &lines);
    }

    // 2. reasoning_effort alone (no chat_template_kwargs)
    for effort in &["low", "high"] {
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "reasoning_effort": effort,
            "stream": true,
        });
        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 60).await;
        print_stream_test(&format!("reasoning_effort={} (no kwargs)", effort), status, &lines);
    }

    // 3. thinking_budget in chat_template_kwargs
    for budget in &[100, 1000, 5000] {
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "chat_template_kwargs": {"enable_thinking": true, "thinking_budget": budget},
            "stream": true,
        });
        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 80).await;
        print_stream_test(&format!("thinking_budget={}", budget), status, &lines);
    }

    // 4. max_thinking_tokens (top-level, like some providers do)
    for tokens in &[100, 2000] {
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "max_thinking_tokens": tokens,
            "chat_template_kwargs": {"enable_thinking": true},
            "stream": true,
        });
        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 80).await;
        print_stream_test(&format!("max_thinking_tokens={}", tokens), status, &lines);
    }

    // 5. budget_tokens in chat_template_kwargs
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 500,
        "chat_template_kwargs": {"enable_thinking": true, "budget_tokens": 500},
        "stream": true,
    });
    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 80).await;
    print_stream_test("budget_tokens=500 in kwargs", status, &lines);

    // 6. enable_thinking as string values instead of bool
    for val in &["low", "medium", "high"] {
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "chat_template_kwargs": {"enable_thinking": val},
            "stream": true,
        });
        let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 60).await;
        print_stream_test(&format!("enable_thinking=\"{}\"", val), status, &lines);
    }

    // 7. thinking object (Anthropic-style) at top level
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_tokens": 500,
        "thinking": {"type": "enabled", "budget_tokens": 500},
        "stream": true,
    });
    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 80).await;
    print_stream_test("thinking={type:enabled, budget_tokens:500}", status, &lines);

    // 8. max_completion_tokens (does DeepInfra even accept this?)
    let body = json!({
        "model": "moonshotai/Kimi-K2.5",
        "messages": [{"role": "user", "content": PROMPT}],
        "max_completion_tokens": 500,
        "chat_template_kwargs": {"enable_thinking": true},
        "stream": true,
    });
    let (status, lines) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 60).await;
    print_stream_test("max_completion_tokens=500 + thinking", status, &lines);
}

// ============================================================================
// Run all — convenience
// ============================================================================

#[tokio::test]
#[ignore]
async fn all_models_probe() {
    let config = load_test_config();
    let client = Client::new();

    println!("\n\n######################################################");
    println!("#  rabb1tClaw API Parameter Probe                    #");
    println!("######################################################\n");

    // Check which providers are configured
    if let Some(p) = config["providers"].as_object() {
        println!("Configured providers:");
        for k in p.keys() {
            println!("  - {}", k);
        }
    }

    // --- Anthropic ---
    if config["providers"]["anthropic"]["api_key"].as_str().is_some() {
        let (base_url, api_key) = get_provider(&config, "anthropic");

        print_header("1. Anthropic claude-sonnet-4-5 — baseline");
        let body = json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 100, "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "anthropic", &body, 20).await;
        print_stream_test("baseline", s, &l);

        print_header("2. Anthropic claude-sonnet-4-5 — thinking");
        let body = json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 8000,
            "thinking": {"type": "enabled", "budget_tokens": 5000},
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "anthropic", &body, 40).await;
        print_stream_test("thinking enabled", s, &l);
    } else {
        println!("\n  [SKIP] No anthropic provider configured");
    }

    // --- OpenAI ---
    if config["providers"]["openai"]["api_key"].as_str().is_some() {
        let (base_url, api_key) = get_provider(&config, "openai");

        print_header("3. OpenAI gpt-4o — all params");
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 100, "temperature": 0.7, "top_p": 0.9,
            "frequency_penalty": 0.3, "presence_penalty": 0.3,
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 20).await;
        print_stream_test("all params", s, &l);

        print_header("4. OpenAI gpt-5.2 — max_completion_tokens + reasoning_effort");
        let body = json!({
            "model": "gpt-5.2",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_completion_tokens": 200, "reasoning_effort": "low",
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 30).await;
        print_stream_test("reasoning_effort=low", s, &l);
    } else {
        println!("\n  [SKIP] No openai provider configured");
    }

    // --- DeepInfra ---
    if config["providers"]["deepinfra"]["api_key"].as_str().is_some() {
        let (base_url, api_key) = get_provider(&config, "deepinfra");

        print_header("5. DeepInfra Kimi-K2.5 — baseline");
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 200, "temperature": 1.0, "top_p": 0.95,
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 40).await;
        print_stream_test("baseline", s, &l);

        print_header("6. DeepInfra Kimi-K2.5 — thinking toggle");
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 500,
            "chat_template_kwargs": {"enable_thinking": true},
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 60).await;
        print_stream_test("enable_thinking=true", s, &l);

        print_header("7. DeepInfra Kimi-K2.5 — thinking disabled");
        let body = json!({
            "model": "moonshotai/Kimi-K2.5",
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 200,
            "chat_template_kwargs": {"enable_thinking": false},
            "stream": true,
        });
        let (s, l) = post_chat_stream(&client, &base_url, &api_key, "openai", &body, 30).await;
        print_stream_test("enable_thinking=false", s, &l);
    } else {
        println!("\n  [SKIP] No deepinfra provider configured");
    }

    println!("\n\n######################################################");
    println!("#  Probe complete                                    #");
    println!("######################################################\n");
}
