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
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    /// vLLM extension: controls thinking for OSS reasoning models on DeepInfra etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatTemplateKwargs {
    enable_thinking: bool,
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
    /// OSS reasoning models (DeepSeek R1, Qwen QwQ, etc.) on vLLM-backed providers
    /// return chain-of-thought here instead of inline <think> tags.
    /// We intentionally parse and discard it — only `content` is forwarded.
    #[allow(dead_code)]
    reasoning_content: Option<String>,
}

fn parse_openai_sse(data: &str) -> Option<StreamChunk> {
    if data == "[DONE]" {
        return Some(StreamChunk::Done);
    }

    let parsed: OpenAiStreamChunk = serde_json::from_str(data).ok()?;
    let delta = parsed.choices.first()?.delta.as_ref()?;

    // Skip chunks that only carry reasoning_content (thinking phase)
    let content = delta.content.as_ref()?;
    if content.is_empty() {
        return None;
    }

    Some(StreamChunk::Text(content.clone()))
}

/// Check if a model is a reasoning model that requires max_completion_tokens
/// instead of max_tokens, and supports reasoning_effort.
/// Covers: o-series (o1, o3, o4-mini, …) and GPT-5.x (gpt-5, gpt-5.2, …)
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
        || m.starts_with("gpt-5")
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_stream(&self, request: ChatRequest) -> Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(crate::protocol::STREAM_CHANNEL_CAPACITY);

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

        let reasoning = is_reasoning_model(&request.model);

        // Reasoning models (o-series, gpt-5.x):
        //   - max_completion_tokens instead of max_tokens
        //   - no temperature (server decides)
        //   - reasoning_effort supported
        // Non-reasoning models:
        //   - max_tokens + temperature
        //   - no reasoning_effort
        // Map thinking config → chat_template_kwargs for vLLM-backed providers
        let chat_template_kwargs = request.thinking.as_ref().map(|t| {
            ChatTemplateKwargs { enable_thinking: t.enabled }
        });

        let body = OpenAiRequest {
            model: request.model.clone(),
            messages,
            max_tokens: if reasoning { None } else { request.max_tokens },
            max_completion_tokens: if reasoning { request.max_tokens } else { None },
            temperature: if reasoning { None } else { request.temperature },
            top_p: request.top_p,
            frequency_penalty: request.frequency_penalty,
            presence_penalty: request.presence_penalty,
            reasoning_effort: if reasoning { request.reasoning_effort.clone() } else { None },
            chat_template_kwargs,
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
