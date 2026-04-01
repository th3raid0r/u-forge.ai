//! Chat / LLM provider via Lemonade Server.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::client::LemonadeHttpClient;
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
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
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
}

/// Chat / LLM via `GLM-4.7-Flash-GGUF` (or another configured GPU model).
///
/// Requests are **queued** if STT or another LLM is currently using the GPU.
/// See [`GpuResourceManager`] for the full policy description.
#[derive(Debug, Clone)]
pub struct LemonadeChatProvider {
    client: LemonadeHttpClient,
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
    ///
    /// Pass `Some(gpu)` for GPU-backed llamacpp models (ROCm / Vulkan) so that
    /// the GPU lock is acquired before each inference request.
    /// Pass `None` for NPU-backed FLM models — the NPU is dedicated silicon
    /// with no shared resource contention.
    pub fn new(base_url: &str, model: &str, gpu: Option<Arc<GpuResourceManager>>) -> Self {
        Self {
            client: LemonadeHttpClient::new(base_url),
            model: model.to_string(),
            gpu,
            default_max_tokens: 2048,
            default_temperature: 0.7,
        }
    }

    /// Construct for NPU use — no GPU resource manager needed.
    ///
    /// FLM models run on the AMD NPU, which is physically separate from the
    /// GPU.  No locking is required between NPU LLM and GPU STT/LLM requests.
    pub fn new_npu(base_url: &str, model: &str) -> Self {
        Self::new(base_url, model, None)
    }

    /// Construct using the GPU LLM model discovered in `registry`.
    ///
    /// Looks for [`ModelRole::GpuLlm`](super::ModelRole::GpuLlm) (llamacpp recipe) models only — FLM NPU
    /// LLMs are excluded.  Use [`from_registry_npu`](Self::from_registry_npu)
    /// for NPU LLMs.
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
    ///
    /// Looks for [`ModelRole::NpuLlm`](super::ModelRole::NpuLlm) models only.  No GPU resource manager needed.
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
    ///
    /// This is the primary entry point when you need fine-grained control.
    pub async fn complete(&self, req: ChatRequest) -> Result<ChatCompletionResponse> {
        // Acquire the GPU — suspends if STT or another LLM is active.
        // For NPU-backed providers (`gpu` is `None`), skip the lock entirely —
        // the NPU runs independently of the GPU with no shared resource.
        let _guard = if let Some(gpu) = &self.gpu {
            Some(gpu.begin_llm().await)
        } else {
            None
        };

        let start = std::time::Instant::now();
        let max_tokens = req.max_tokens.unwrap_or(self.default_max_tokens);
        let temperature = req.temperature.unwrap_or(self.default_temperature);

        let body = serde_json::json!({
            "model":       self.model,
            "messages":    req.messages,
            "max_tokens":  max_tokens,
            "temperature": temperature,
            "stream":      false,
        });

        let resp: ChatCompletionResponse = self
            .client
            .post_json("/chat/completions", &body)
            .await
            .context("Chat HTTP request failed")?;

        tracing::debug!(
            model         = %self.model,
            n_messages    = req.messages.len(),
            finish_reason = ?resp.choices.first().and_then(|c| c.finish_reason.as_deref()),
            total_tokens  = ?resp.usage.as_ref().map(|u| u.total_tokens),
            duration_ms   = start.elapsed().as_millis(),
            "Chat completion finished"
        );

        Ok(resp)
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
