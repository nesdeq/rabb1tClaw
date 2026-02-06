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
    stream: bool,
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
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    text: Option<String>,
}

fn parse_anthropic_sse(data: &str) -> Option<StreamChunk> {
    let event: AnthropicStreamEvent = serde_json::from_str(data).ok()?;

    match event.event_type.as_str() {
        "content_block_delta" => {
            let text = event.delta?.text?;
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
        let (tx, rx) = mpsc::channel(100);

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

        let body = AnthropicRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
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
