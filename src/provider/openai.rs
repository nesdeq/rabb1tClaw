//! OpenAI provider implementation.

use super::{get_shared_client, send_and_stream, ChatRequest, LlmProvider, StreamChunk};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};

pub struct OpenAiProvider {
    base_url: String,
    api_key: String,
}

impl OpenAiProvider {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: Option<OpenAiDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

fn parse_openai_sse(data: &str) -> Option<StreamChunk> {
    if data == "[DONE]" {
        return Some(StreamChunk::Done);
    }

    let parsed: OpenAiStreamChunk = serde_json::from_str(data).ok()?;
    let content = parsed.choices.first()?.delta.as_ref()?.content.as_ref()?;

    if content.is_empty() {
        return None;
    }

    Some(StreamChunk::Text(content.clone()))
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(100);

        let mut messages: Vec<OpenAiMessage> = Vec::new();

        if let Some(system) = &request.system {
            messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: system.clone(),
            });
        }

        for msg in &request.messages {
            if msg.role != "system" {
                messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                });
            }
        }

        // gpt-5.x and o1/o3 models use max_completion_tokens instead of max_tokens
        let use_new_format = request.model.starts_with("gpt-5")
            || request.model.starts_with("o1")
            || request.model.starts_with("o3");

        let body = OpenAiRequest {
            model: request.model.clone(),
            messages,
            max_tokens: if use_new_format { None } else { request.max_tokens },
            max_completion_tokens: if use_new_format { request.max_tokens } else { None },
            temperature: request.temperature,
            stream: true,
        };

        let url = format!("{}/chat/completions", self.base_url);
        info!("[OpenAI] stream model={}", request.model);
        for msg in &body.messages {
            debug!("[OpenAI] {} : {}",
                msg.role,
                msg.content.chars().take(60).collect::<String>());
        }

        let http_request = get_shared_client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body);

        tokio::spawn(async move {
            send_and_stream(http_request, tx, "OpenAI", parse_openai_sse).await;
        });

        Ok(rx)
    }
}
