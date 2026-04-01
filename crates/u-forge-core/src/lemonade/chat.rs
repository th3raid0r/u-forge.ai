//! Chat / LLM provider via Lemonade Server.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_openai::{Client, config::OpenAIConfig};
use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    CreateChatCompletionRequestArgs, CreateChatCompletionResponse,
};
use serde::{Deserialize, Serialize};

use super::client::make_lemonade_openai_client;
use super::gpu_manager::GpuResourceManager;
use super::registry::LemonadeModelRegistry;

/// A single message in a chat conversation, following the OpenAI `messages` format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Full response from `POST /api/v1/chat/completions`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<ChatUsage>,
}

/// A single completion choice.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage reported by the model.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl ChatCompletionResponse {
    /// Return the text content of the first choice, if any.
    pub fn first_content(&self) -> Option<&str> {
        self.choices.first().map(|c| c.message.content.as_str())
    }
}

/// Configuration for a single chat request, allowing per-call overrides.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    /// Overrides `LemonadeChatProvider::default_max_tokens`.
    pub max_tokens: Option<u32>,
    /// Overrides `LemonadeChatProvider::default_temperature`.
    pub temperature: Option<f32>,
    /// Overrides the model id set on the provider (e.g. `"GLM-4.7-Flash-GGUF"`).
    pub model: Option<String>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
            model: None,
        }
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

// ── Internal conversion helpers ───────────────────────────────────────────────

fn to_oa_message(msg: &ChatMessage) -> Result<ChatCompletionRequestMessage> {
    match msg.role.as_str() {
        "system" => Ok(ChatCompletionRequestSystemMessageArgs::default()
            .content(msg.content.clone())
            .build()
            .context("Failed to build system message")?
            .into()),
        "user" => Ok(ChatCompletionRequestUserMessageArgs::default()
            .content(msg.content.clone())
            .build()
            .context("Failed to build user message")?
            .into()),
        "assistant" => Ok(ChatCompletionRequestAssistantMessageArgs::default()
            .content(msg.content.clone())
            .build()
            .context("Failed to build assistant message")?
            .into()),
        role => Err(anyhow!("Unknown chat role: {role}")),
    }
}

fn from_oa_response(resp: CreateChatCompletionResponse) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: resp.id,
        choices: resp
            .choices
            .into_iter()
            .map(|c| ChatChoice {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: c.message.content.unwrap_or_default(),
                },
                finish_reason: c.finish_reason.map(|r| {
                    // FinishReason serialises to its lowercase OpenAI name via serde.
                    serde_json::to_value(&r)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| format!("{r:?}").to_lowercase())
                }),
            })
            .collect(),
        usage: resp.usage.map(|u| ChatUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }),
    }
}

// ── LemonadeChatProvider ──────────────────────────────────────────────────────

/// Chat / LLM via `GLM-4.7-Flash-GGUF` (or another configured GPU model).
///
/// Requests are **queued** if STT or another LLM is currently using the GPU.
/// See [`GpuResourceManager`] for the full policy description.
#[derive(Debug, Clone)]
pub struct LemonadeChatProvider {
    client: Client<OpenAIConfig>,
    /// The model id sent to the API (e.g. `"GLM-4.7-Flash-GGUF"`).
    pub model: String,
    /// Shared GPU resource manager — also held by [`LemonadeSttProvider`](super::LemonadeSttProvider).
    ///
    /// `None` when the provider targets the AMD NPU (FLM models), which runs on
    /// dedicated silicon with no GPU resource contention.  When `Some`, the GPU
    /// lock is acquired before every inference request via
    /// [`GpuResourceManager::begin_llm`].
    pub gpu: Option<Arc<GpuResourceManager>>,
    /// Default token limit used when no per-request override is given.
    pub default_max_tokens: u32,
    /// Default sampling temperature used when no per-request override is given.
    pub default_temperature: f32,
}

impl LemonadeChatProvider {
    /// Construct with an explicit base URL, model id, and optional GPU manager.
    pub fn new(base_url: &str, model: &str, gpu: Option<Arc<GpuResourceManager>>) -> Self {
        Self {
            client: make_lemonade_openai_client(base_url),
            model: model.to_string(),
            gpu,
            default_max_tokens: 2048,
            default_temperature: 0.7,
        }
    }

    /// Construct for NPU use — no GPU resource manager needed.
    pub fn new_npu(base_url: &str, model: &str) -> Self {
        Self::new(base_url, model, None)
    }

    /// Construct using the GPU LLM model discovered in `registry`.
    pub fn from_registry(
        registry: &LemonadeModelRegistry,
        gpu: Option<Arc<GpuResourceManager>>,
    ) -> Result<Self> {
        let model = registry
            .llm_model()
            .ok_or_else(|| anyhow!("No GPU LLM model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id, gpu))
    }

    /// Construct using the NPU FLM LLM model discovered in `registry`.
    pub fn from_registry_npu(registry: &LemonadeModelRegistry) -> Result<Self> {
        let model = registry
            .npu_llm_model()
            .ok_or_else(|| anyhow!("No NPU FLM LLM model found in the Lemonade registry"))?;
        Ok(Self::new_npu(&registry.base_url, &model.id))
    }

    /// Override the default max tokens ceiling.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.default_max_tokens = n;
        self
    }

    /// Override the default sampling temperature.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.default_temperature = t;
        self
    }

    /// Send a full `ChatRequest`, queuing if the GPU is busy.
    pub async fn complete(&self, req: ChatRequest) -> Result<ChatCompletionResponse> {
        let _guard = if let Some(gpu) = &self.gpu {
            Some(gpu.begin_llm().await)
        } else {
            None
        };

        let start = std::time::Instant::now();
        let max_tokens = req.max_tokens.unwrap_or(self.default_max_tokens);
        let temperature = req.temperature.unwrap_or(self.default_temperature);
        let model = req.model.as_deref().unwrap_or(&self.model);

        let messages: Result<Vec<_>> = req.messages.iter().map(to_oa_message).collect();
        let messages = messages.context("Failed to convert chat messages")?;

        let oa_req = CreateChatCompletionRequestArgs::default()
            .model(model)
            .messages(messages)
            .max_completion_tokens(max_tokens)
            .temperature(temperature)
            .build()
            .context("Failed to build chat completion request")?;

        let oa_resp = self
            .client
            .chat()
            .create(oa_req)
            .await
            .context("Chat HTTP request failed")?;

        tracing::debug!(
            model         = %model,
            n_messages    = req.messages.len(),
            finish_reason = ?oa_resp.choices.first().and_then(|c| c.finish_reason.as_ref()),
            total_tokens  = ?oa_resp.usage.as_ref().map(|u| u.total_tokens),
            duration_ms   = start.elapsed().as_millis(),
            "Chat completion finished"
        );

        Ok(from_oa_response(oa_resp))
        // _guard dropped here → GPU released.
    }

    /// Send a list of messages with provider defaults, queuing if GPU is busy.
    pub async fn chat(&self, messages: Vec<ChatMessage>) -> Result<ChatCompletionResponse> {
        self.complete(ChatRequest::new(messages)).await
    }

    /// Convenience: single user-turn prompt. Returns the assistant's text.
    pub async fn ask(&self, prompt: &str) -> Result<String> {
        let resp = self.chat(vec![ChatMessage::user(prompt)]).await?;
        resp.first_content()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Chat response contained no choices"))
    }

    /// Convenience: system prompt + single user turn. Returns the assistant's text.
    pub async fn ask_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let resp = self
            .chat(vec![ChatMessage::system(system), ChatMessage::user(prompt)])
            .await?;
        resp.first_content()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Chat response contained no choices"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;

    #[tokio::test]
    async fn test_chat_ask_returns_response() {
        let url = require_integration_url!();
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let response = chat
            .ask("Respond with exactly one word: pong")
            .await
            .unwrap();
        assert!(
            !response.is_empty(),
            "Chat should return a non-empty response"
        );
    }

    #[tokio::test]
    async fn test_chat_request_with_overrides() {
        let url = require_integration_url!();
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let req = ChatRequest::new(vec![ChatMessage::user("Count to three.")])
            .with_max_tokens(64)
            .with_temperature(0.1);

        let resp = chat.complete(req).await.unwrap();
        assert!(resp.first_content().is_some());
    }
}
