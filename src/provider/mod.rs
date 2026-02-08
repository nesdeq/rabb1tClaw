//! LLM provider implementations (OpenAI, Anthropic).

pub mod anthropic;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Shared HTTP client for all LLM requests (connection pooling, TCP_NODELAY)
static SHARED_CLIENT: OnceLock<Client> = OnceLock::new();

pub fn get_shared_client() -> &'static Client {
    SHARED_CLIENT.get_or_init(|| {
        Client::builder()
            .tcp_nodelay(true)
            .build()
            .expect("failed to build HTTP client")
    })
}

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub thinking: Option<ThinkingParams>,
}

#[derive(Debug, Clone)]
pub struct ThinkingParams {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

impl From<&crate::config::ThinkingConfig> for ThinkingParams {
    fn from(t: &crate::config::ThinkingConfig) -> Self {
        Self { enabled: t.enabled, budget_tokens: t.budget_tokens }
    }
}

/// A streaming chunk from the LLM
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Text(String),
    Done,
    Error(String),
}

// ============================================================================
// Provider Trait
// ============================================================================

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_stream(&self, request: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>>;
}

// ============================================================================
// Shared SSE Stream Processing
// ============================================================================

/// Process an SSE stream from an HTTP response, calling `parse_data` for each data line.
/// `parse_data` returns `Some(StreamChunk)` for relevant data, or `None` to skip.
pub async fn process_sse_stream(
    resp: reqwest::Response,
    tx: mpsc::Sender<StreamChunk>,
    provider_name: &str,
    parse_data: impl Fn(&str) -> Option<StreamChunk>,
) {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut total_chars = 0usize;

    while let Some(chunk_result) = stream.next().await {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(e) => {
                warn!("[{}] Stream error: {}", provider_name, e);
                let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Parse all complete lines via index scanning — single drain at the end
        let mut consumed = 0;
        loop {
            let rel_end = match buffer[consumed..].find('\n') {
                Some(pos) => pos,
                None => break,
            };
            let line_end = consumed + rel_end;

            // Parse inside a block so the buffer borrow is released before any await
            let maybe_chunk = {
                let line = buffer[consumed..line_end].trim();
                if line.is_empty() || line.starts_with(':') {
                    None
                } else {
                    line.strip_prefix("data: ").and_then(|data| parse_data(data))
                }
            };
            consumed = line_end + 1;

            let Some(chunk) = maybe_chunk else { continue };

            match &chunk {
                StreamChunk::Text(text) => {
                    total_chars += text.len();
                    debug!("[{}] chunk: {}", provider_name, text);
                }
                StreamChunk::Done => {
                    info!("[{}] Stream done, total {} chars", provider_name, total_chars);
                    let _ = tx.send(chunk).await;
                    return;
                }
                StreamChunk::Error(_) => {}
            }
            let _ = tx.send(chunk).await;
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }
    }

    info!("[{}] Stream ended, total {} chars", provider_name, total_chars);
    let _ = tx.send(StreamChunk::Done).await;
}

/// Send an HTTP request and start SSE processing, handling common error cases.
pub async fn send_and_stream(
    request: reqwest::RequestBuilder,
    tx: mpsc::Sender<StreamChunk>,
    provider_name: &str,
    parse_data: impl Fn(&str) -> Option<StreamChunk> + Send + 'static,
) {
    let resp = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("[{}] Request error: {}", provider_name, e);
            let _ = tx.send(StreamChunk::Error(e.to_string())).await;
            return;
        }
    };

    info!("[{}] Response status: {}", provider_name, resp.status());

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        warn!("[{}] Error {}: {}", provider_name, status, text);
        let _ = tx
            .send(StreamChunk::Error(format!(
                "{} API error {}: {}",
                provider_name, status, text
            )))
            .await;
        return;
    }

    process_sse_stream(resp, tx, provider_name, parse_data).await;
}

// ============================================================================
// Provider Factory
// ============================================================================

/// Create an LLM provider based on API type
pub fn create_provider(
    api_type: &str,
    base_url: &str,
    api_key: &str,
) -> Box<dyn LlmProvider> {
    match api_type {
        "anthropic-messages" | "anthropic" => {
            Box::new(anthropic::AnthropicProvider::new(base_url, api_key))
        }
        _ => {
            // Default to OpenAI-compatible API
            Box::new(openai::OpenAiProvider::new(base_url, api_key))
        }
    }
}

// ============================================================================
// Model Listing
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
}

#[derive(Debug, Deserialize)]
struct ModelListResponse {
    data: Vec<ModelInfo>,
}

/// List models from any supported API. Headers are set based on api_type.
pub async fn list_models(api_type: &str, base_url: &str, api_key: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = get_shared_client().get(&url);

    req = match api_type {
        "anthropic" | "anthropic-messages" => req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01"),
        _ => req.bearer_auth(api_key),
    };

    let resp = req.send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, text);
    }

    let list: ModelListResponse = resp.json().await?;
    Ok(list.data)
}
