//! Chat / LLM provider via Lemonade Server.
//!
//! Lemonade's `/chat/completions` endpoint is *almost* OpenAI-compatible, but
//! deviates at the thinking/reasoning parameter: OpenAI uses a `reasoning` object
//! or `reasoning_effort` string, while Lemonade uses a flat `enable_thinking: bool`
//! field in the request body.  Because `async-openai`'s typed builder cannot inject
//! arbitrary fields, this module hand-rolls both the request struct and the SSE
//! stream parser using `reqwest` directly (via [`LemonadeHttpClient::post_stream`]).
//!
//! All other Lemonade endpoints (embeddings, TTS, STT) remain on `async-openai`.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::client::LemonadeHttpClient;
use super::gpu_manager::GpuResourceManager;

// ── Wire types ────────────────────────────────────────────────────────────────

/// Serialised body sent to `POST /chat/completions`.
#[derive(Serialize)]
struct LemonadeChatReq<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_completion_tokens: u32,
    temperature: f32,
    /// Lemonade-specific field — absent when `None` (uses model default).
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    stream: bool,
}

/// Minimal streaming chunk shape — only the fields we need.
#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    /// Reasoning/thinking tokens — present when `enable_thinking: true` is set.
    /// Carried in `reasoning_content` per the Lemonade SSE wire format.
    reasoning_content: Option<String>,
}

// ── Public stream token type ──────────────────────────────────────────────────

/// A single token yielded by [`LemonadeChatProvider::complete_stream`].
#[derive(Debug, Clone)]
pub enum StreamToken {
    /// Normal assistant response text.
    Content(String),
    /// Chain-of-thought reasoning token (only present when `enable_thinking` is active).
    Thinking(String),
}

// ── Public types ──────────────────────────────────────────────────────────────

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
    /// When `Some(true)`, sends `enable_thinking: true` in the request body, activating
    /// Lemonade's chain-of-thought reasoning.  `None` omits the field (model default).
    pub enable_thinking: Option<bool>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
            model: None,
            enable_thinking: None,
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

    /// Enable or disable Lemonade's chain-of-thought reasoning for this request.
    ///
    /// Serialises to `enable_thinking: bool` in the request body — Lemonade's
    /// deviation from the OpenAI `reasoning`/`reasoning_effort` convention.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.enable_thinking = Some(enabled);
        self
    }
}

// ── LemonadeChatProvider ──────────────────────────────────────────────────────

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
    pub fn new_npu(base_url: &str, model: &str) -> Self {
        Self::new(base_url, model, None)
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

        let body = LemonadeChatReq {
            model,
            messages: &req.messages,
            max_completion_tokens: max_tokens,
            temperature,
            enable_thinking: req.enable_thinking,
            stream: false,
        };

        let resp: ChatCompletionResponse = self
            .client
            .post_json("/chat/completions", &body)
            .await
            .context("Chat HTTP request failed")?;

        tracing::debug!(
            model         = %model,
            n_messages    = req.messages.len(),
            finish_reason = ?resp.choices.first().and_then(|c| c.finish_reason.as_ref()),
            total_tokens  = ?resp.usage.as_ref().map(|u| u.total_tokens),
            duration_ms   = start.elapsed().as_millis(),
            "Chat completion finished"
        );

        Ok(resp)
        // _guard dropped here → GPU released.
    }

    /// Send a streaming `ChatRequest`; returns an mpsc receiver that yields
    /// [`StreamToken`]s as the model generates them.
    ///
    /// Spawns an internal Tokio task that holds the GPU lock and drives the
    /// SSE stream.  The task exits (and the lock is released) when the stream
    /// is exhausted, the receiver is dropped, or the first error occurs.
    ///
    /// # SSE parsing
    ///
    /// Lemonade follows the standard OpenAI SSE wire format:
    /// `data: {…json…}\n\n` lines until `data: [DONE]\n\n`.
    /// Non-`data:` lines (`event:`, `id:`, blank) are silently skipped.
    ///
    /// When `enable_thinking` is active, the model may emit `reasoning_content`
    /// deltas alongside (or before) normal `content` deltas; these are surfaced
    /// as [`StreamToken::Thinking`] items.
    pub fn complete_stream(&self, req: ChatRequest) -> mpsc::Receiver<Result<StreamToken>> {
        let (tx, rx) = mpsc::channel(64);
        let provider = self.clone();

        tokio::spawn(async move {
            let _guard = if let Some(gpu) = &provider.gpu {
                Some(gpu.begin_llm().await)
            } else {
                None
            };

            let max_tokens = req.max_tokens.unwrap_or(provider.default_max_tokens);
            let temperature = req.temperature.unwrap_or(provider.default_temperature);
            let model = req.model.as_deref().unwrap_or(&provider.model).to_string();

            let body = LemonadeChatReq {
                model: &model,
                messages: &req.messages,
                max_completion_tokens: max_tokens,
                temperature,
                enable_thinking: req.enable_thinking,
                stream: true,
            };

            let response = match provider
                .client
                .post_stream("/chat/completions", &body)
                .await
                .context("Stream init failed")
            {
                Ok(r) => r,
                Err(e) => { let _ = tx.send(Err(e)).await; return; }
            };

            // Drive the SSE byte stream, accumulating into a line buffer.
            let mut line_buf = String::new();
            let mut byte_stream = response.bytes_stream();

            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk.context("Stream read error") {
                    Ok(b) => b,
                    Err(e) => { let _ = tx.send(Err(e)).await; return; }
                };

                line_buf.push_str(&String::from_utf8_lossy(&bytes));

                // Process all complete lines in the buffer.
                loop {
                    let Some(nl) = line_buf.find('\n') else { break };
                    let line = line_buf[..nl].trim_end_matches('\r').to_string();
                    line_buf.drain(..=nl);

                    if line.is_empty() {
                        continue;
                    }

                    let Some(data) = line.strip_prefix("data: ") else {
                        continue; // skip event:, id:, comment lines
                    };

                    if data == "[DONE]" {
                        return;
                    }

                    if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                        for choice in chunk.choices {
                            if let Some(thinking) = choice.delta.reasoning_content {
                                if !thinking.is_empty()
                                    && tx.send(Ok(StreamToken::Thinking(thinking))).await.is_err()
                                {
                                    return;
                                }
                            }
                            if let Some(content) = choice.delta.content {
                                if !content.is_empty()
                                    && tx.send(Ok(StreamToken::Content(content))).await.is_err()
                                {
                                    return; // receiver dropped
                                }
                            }
                        }
                    }
                }
            }
            // _guard dropped here → GPU released.
        });

        rx
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
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url).await.unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let llm = selector.select_llm_models().into_iter().next()
            .expect("No LLM model found in catalog");
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::new(&url, &llm.model_id, Some(gpu));

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
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url).await.unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let llm = selector.select_llm_models().into_iter().next()
            .expect("No LLM model found in catalog");
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::new(&url, &llm.model_id, Some(gpu));

        let req = ChatRequest::new(vec![ChatMessage::user("Count to three.")])
            .with_max_tokens(64)
            .with_temperature(0.1);

        let resp = chat.complete(req).await.unwrap();
        assert!(resp.first_content().is_some());
    }
}
