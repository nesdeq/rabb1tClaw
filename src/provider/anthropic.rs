//! Anthropic provider implementation.

use super::{get_shared_client, send_and_stream, ChatRequest, LlmProvider, StreamChunk};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
}

impl AnthropicProvider {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct AnthropicThinking {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: u32,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    delta: Option<AnthropicDelta>,
    // content_block_start events carry a content_block with a type field
    content_block: Option<AnthropicContentBlock>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
}

fn parse_anthropic_sse(data: &str) -> Option<StreamChunk> {
    let event: AnthropicStreamEvent = serde_json::from_str(data).ok()?;

    match event.event_type.as_str() {
        "content_block_start" => {
            // Filter out thinking blocks — only emit text blocks
            if let Some(block) = &event.content_block {
                if block.block_type == "thinking" {
                    return None;
                }
            }
            None
        }
        "content_block_delta" => {
            let delta = event.delta?;
            // Filter thinking deltas (type: "thinking_delta")
            if delta.delta_type.as_deref() == Some("thinking_delta") {
                return None;
            }
            let text = delta.text?;
            if text.is_empty() {
                return None;
            }
            Some(StreamChunk::Text(text))
        }
        "message_stop" => Some(StreamChunk::Done),
        _ => None,
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(crate::protocol::STREAM_CHANNEL_CAPACITY);

        let mut system: Option<String> = request.system.clone();
        let mut messages: Vec<AnthropicMessage> = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                system = Some(msg.content.clone());
            } else {
                messages.push(AnthropicMessage {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                });
            }
        }

        let url = format!("{}/messages", self.base_url);
        info!("[Anthropic] stream model={}", request.model);
        if let Some(sys) = &system {
            debug!("[Anthropic] system: {}",
                sys.chars().take(60).collect::<String>());
        }
        for msg in &messages {
            debug!("[Anthropic] {} : {}",
                msg.role,
                msg.content.chars().take(60).collect::<String>());
        }

        // Build thinking config
        let thinking = request.thinking.as_ref().and_then(|t| {
            if t.enabled {
                Some(AnthropicThinking {
                    thinking_type: "enabled".to_string(),
                    budget_tokens: t.budget_tokens.unwrap_or(crate::cli::defaults::DEFAULT_THINKING_BUDGET_TOKENS_ANTHROPIC),
                })
            } else {
                None
            }
        });
        let thinking_enabled = thinking.is_some();

        let body = AnthropicRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens.unwrap_or(crate::cli::defaults::DEFAULT_MODEL_MAX_TOKENS),
            // When thinking is enabled, temperature must not be sent (Anthropic requirement)
            temperature: if thinking_enabled { None } else { request.temperature },
            top_p: request.top_p,
            thinking,
            stream: true,
            system,
        };

        let http_request = get_shared_client()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body);

        tokio::spawn(async move {
            send_and_stream(http_request, tx, "Anthropic", parse_anthropic_sse).await;
        });

        Ok(rx)
    }
}
