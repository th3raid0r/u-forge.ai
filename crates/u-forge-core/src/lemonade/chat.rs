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

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::client::{LemonadeConnection, LemonadeHttpClient};
use super::gpu_manager::GpuResourceManager;
use super::runtime::LemonadeRuntimeLease;

// ── Wire types ────────────────────────────────────────────────────────────────

/// Serialised body sent to `POST /chat/completions`.
#[derive(Serialize)]
struct LemonadeChatReq<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_completion_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    /// Lemonade's current wire name is `repeat_penalty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<&'a [String]>,
    /// Lemonade-specific field — absent when `None` (uses model default).
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    stream: bool,
}

/// Minimal streaming chunk shape — only the fields we need.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/thinking tokens — present when `enable_thinking: true` is set.
    /// Carried in `reasoning_content` per the Lemonade SSE wire format.
    #[serde(default)]
    reasoning_content: Option<String>,
}

// ── Public stream token type ──────────────────────────────────────────────────

/// A single token yielded by [`LemonadeChatProvider::complete_stream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamToken {
    /// Normal assistant response text.
    Content(String),
    /// Chain-of-thought reasoning token (only present when `enable_thinking` is active).
    Thinking(String),
    /// Terminal reason returned for a choice (for example `stop` or `length`).
    FinishReason(String),
    /// Usage accounting returned by the final streaming event.
    Usage(ChatUsage),
}

/// Transport-neutral events consumed above direct HTTP and Rig adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    ReasoningDelta(String),
    TextDelta(String),
    ToolCallStart {
        internal_id: String,
        name: String,
        args_display: String,
    },
    ToolResult {
        internal_id: String,
        content: String,
    },
    Usage(ChatUsage),
    Finished {
        reason: ChatTerminalReason,
        full_text: Option<String>,
    },
    FatalError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatTerminalReason {
    Provider(String),
    AgentComplete,
}

impl From<StreamToken> for ChatEvent {
    fn from(token: StreamToken) -> Self {
        match token {
            StreamToken::Content(text) => Self::TextDelta(text),
            StreamToken::Thinking(text) => Self::ReasoningDelta(text),
            StreamToken::FinishReason(reason) => Self::Finished {
                reason: ChatTerminalReason::Provider(reason),
                full_text: None,
            },
            StreamToken::Usage(usage) => Self::Usage(usage),
        }
    }
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
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
    /// Optional sampling controls shared by direct and agent chat adapters.
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub repetition_penalty: Option<f32>,
    pub seed: Option<u64>,
    pub stop: Option<Vec<String>>,
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
            top_p: None,
            top_k: None,
            min_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            repetition_penalty: None,
            seed: None,
            stop: None,
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

    /// Apply all configured sampling controls as one coherent request profile.
    pub fn with_sampling(mut self, sampling: &crate::config::ChatDeviceConfig) -> Self {
        self.temperature = sampling.temperature;
        self.top_p = sampling.top_p;
        self.top_k = sampling.top_k;
        self.min_p = sampling.min_p;
        self.frequency_penalty = sampling.frequency_penalty;
        self.presence_penalty = sampling.presence_penalty;
        self.repetition_penalty = sampling.repetition_penalty;
        self.seed = sampling.seed;
        self.stop.clone_from(&sampling.stop);
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
        let connection = Arc::new(LemonadeConnection::external(base_url).unwrap_or_else(|_| {
            LemonadeConnection::external("http://127.0.0.1:1/v1")
                .expect("static fallback Lemonade URL is valid")
        }));
        Self::from_connection(connection, model, gpu)
    }

    pub fn from_connection(
        connection: Arc<LemonadeConnection>,
        model: &str,
        gpu: Option<Arc<GpuResourceManager>>,
    ) -> Self {
        Self {
            client: LemonadeHttpClient::from_connection(connection),
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
            top_p: req.top_p,
            top_k: req.top_k,
            min_p: req.min_p,
            frequency_penalty: req.frequency_penalty,
            presence_penalty: req.presence_penalty,
            repeat_penalty: req.repetition_penalty,
            seed: req.seed,
            stop: req.stop.as_deref(),
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

    /// Complete while retaining an acquired runtime lease through the full
    /// non-streaming response.
    pub async fn complete_with_lease(
        &self,
        req: ChatRequest,
        _lease: LemonadeRuntimeLease,
    ) -> Result<ChatCompletionResponse> {
        self.complete(req).await
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
        self.complete_stream_inner(req, None)
    }

    /// Stream while retaining an acquired runtime lease until completion,
    /// cancellation, timeout, or protocol error.
    pub fn complete_stream_with_lease(
        &self,
        req: ChatRequest,
        lease: LemonadeRuntimeLease,
    ) -> mpsc::Receiver<Result<StreamToken>> {
        self.complete_stream_inner(req, Some(lease))
    }

    fn complete_stream_inner(
        &self,
        req: ChatRequest,
        lease: Option<LemonadeRuntimeLease>,
    ) -> mpsc::Receiver<Result<StreamToken>> {
        let (tx, rx) = mpsc::channel(64);
        let provider = self.clone();

        tokio::spawn(async move {
            let _runtime_lease = lease;
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
                top_p: req.top_p,
                top_k: req.top_k,
                min_p: req.min_p,
                frequency_penalty: req.frequency_penalty,
                presence_penalty: req.presence_penalty,
                repeat_penalty: req.repetition_penalty,
                seed: req.seed,
                stop: req.stop.as_deref(),
                enable_thinking: req.enable_thinking,
                stream: true,
            };

            let timeouts = provider.client.connection().timeouts();
            let first_token_deadline = tokio::time::Instant::now() + timeouts.first_token;
            let response = match tokio::time::timeout(
                timeouts.first_token,
                provider.client.post_stream("/chat/completions", &body),
            )
            .await
            .context("Timed out waiting for chat response headers")
            .and_then(|result| result.context("Stream init failed"))
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            let mut decoder = SseDecoder::default();
            let mut byte_stream = response.bytes_stream();
            let mut semantic_seen = false;

            loop {
                let deadline = if semantic_seen {
                    timeouts.stream_idle
                } else {
                    first_token_deadline.saturating_duration_since(tokio::time::Instant::now())
                };
                if deadline.is_zero() {
                    let _ = tx
                        .send(Err(anyhow!(
                            "Lemonade first token timeout after {:?}",
                            timeouts.first_token
                        )))
                        .await;
                    return;
                }
                let next = match tokio::time::timeout(deadline, byte_stream.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        let phase = if semantic_seen {
                            "stream idle"
                        } else {
                            "first token"
                        };
                        let _ = tx
                            .send(Err(anyhow!("Lemonade {phase} timeout after {deadline:?}")))
                            .await;
                        return;
                    }
                };
                let Some(chunk) = next else { break };
                let bytes = match chunk.context("Stream read error") {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };

                for event in decoder.push(&bytes) {
                    match decode_sse_event(&event) {
                        Ok(SseEvent::Done) => return,
                        Ok(SseEvent::Tokens(tokens)) => {
                            semantic_seen |= !tokens.is_empty();
                            for token in tokens {
                                if tx.send(Ok(token)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(error)).await;
                            return;
                        }
                    }
                }
            }
            if decoder.has_unfinished_event() {
                let _ = tx
                    .send(Err(anyhow!(
                        "Lemonade SSE stream ended with an incomplete event"
                    )))
                    .await;
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

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((start, length)) = event_boundary(&self.buffer) {
            events.push(self.buffer[..start].to_vec());
            self.buffer.drain(..start + length);
        }
        events
    }

    fn has_unfinished_event(&self) -> bool {
        self.buffer.iter().any(|byte| !byte.is_ascii_whitespace())
    }
}

fn event_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|i| (i, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

enum SseEvent {
    Done,
    Tokens(Vec<StreamToken>),
}

fn decode_sse_event(event: &[u8]) -> Result<SseEvent> {
    let event = std::str::from_utf8(event).context("Lemonade SSE event was not valid UTF-8")?;
    let mut data = Vec::new();
    let mut event_name = None;
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') || line.starts_with("id:") {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        } else if !line.is_empty() {
            return Err(anyhow!("Malformed Lemonade SSE field: {line}"));
        }
    }
    if data.is_empty() {
        return Ok(SseEvent::Tokens(Vec::new()));
    }
    let data = data.join("\n");
    if event_name == Some("error") {
        return Err(anyhow!("Lemonade SSE error event: {data}"));
    }
    if data == "[DONE]" {
        return Ok(SseEvent::Done);
    }
    let chunk: StreamChunk = serde_json::from_str(&data)
        .with_context(|| format!("Malformed Lemonade SSE JSON: {data}"))?;
    if let Some(error) = chunk.error {
        return Err(anyhow!("Lemonade streaming error: {error}"));
    }
    let mut tokens = Vec::new();
    for choice in chunk.choices {
        if let Some(thinking) = choice
            .delta
            .reasoning_content
            .filter(|value| !value.is_empty())
        {
            tokens.push(StreamToken::Thinking(thinking));
        }
        if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
            tokens.push(StreamToken::Content(content));
        }
        if let Some(reason) = choice.finish_reason {
            tokens.push(StreamToken::FinishReason(reason));
        }
    }
    if let Some(usage) = chunk.usage {
        tokens.push(StreamToken::Usage(usage));
    }
    Ok(SseEvent::Tokens(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lemonade::LemonadeTimeouts;
    use crate::test_helpers::{GPU_CPU_TEST_LLM_MODEL, require_integration_url};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock_stream_provider(
        writes: Vec<(std::time::Duration, &'static [u8])>,
        timeouts: LemonadeTimeouts,
        gpu: Option<Arc<GpuResourceManager>>,
    ) -> (LemonadeChatProvider, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            for (delay, bytes) in writes {
                tokio::time::sleep(delay).await;
                if socket.write_all(bytes).await.is_err() {
                    break;
                }
            }
        });
        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}/v1"),
                super::super::LemonadeOwnership::External,
                None,
                None,
                timeouts,
            )
            .unwrap(),
        );
        (
            LemonadeChatProvider::from_connection(connection, "test-model", gpu),
            server,
        )
    }

    fn downloaded_test_llm(
        catalog: &crate::lemonade::LemonadeServerCatalog,
    ) -> Option<&crate::lemonade::CatalogModel> {
        catalog
            .models
            .iter()
            .find(|model| model.downloaded && model.id == GPU_CPU_TEST_LLM_MODEL)
    }

    fn tokens(event: &[u8]) -> Vec<StreamToken> {
        match decode_sse_event(event).unwrap() {
            SseEvent::Tokens(tokens) => tokens,
            SseEvent::Done => panic!("expected tokens"),
        }
    }

    #[test]
    fn direct_chat_serializes_the_complete_sampling_profile() {
        let messages = vec![ChatMessage::user("hello")];
        let stop = vec!["END".to_string()];
        let body = LemonadeChatReq {
            model: "test-model",
            messages: &messages,
            max_completion_tokens: 321,
            temperature: 0.25,
            top_p: Some(0.8),
            top_k: Some(40),
            min_p: Some(0.05),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            repeat_penalty: Some(1.1),
            seed: Some(7),
            stop: Some(&stop),
            enable_thinking: Some(false),
            stream: true,
        };
        let value = serde_json::to_value(body).unwrap();
        assert_eq!(value["max_completion_tokens"], 321);
        assert!((value["top_p"].as_f64().unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(value["top_k"], 40);
        assert!((value["min_p"].as_f64().unwrap() - 0.05).abs() < 1e-6);
        assert!((value["frequency_penalty"].as_f64().unwrap() - 0.1).abs() < 1e-6);
        assert!((value["presence_penalty"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert!((value["repeat_penalty"].as_f64().unwrap() - 1.1).abs() < 1e-6);
        assert!(value.get("repetition_penalty").is_none());
        assert_eq!(value["seed"], 7);
        assert_eq!(value["stop"], serde_json::json!(["END"]));
        assert_eq!(value["enable_thinking"], false);
    }

    #[test]
    fn direct_chat_omits_unconfigured_optional_sampling_fields() {
        let messages = vec![ChatMessage::user("hello")];
        let body = LemonadeChatReq {
            model: "test-model",
            messages: &messages,
            max_completion_tokens: 10,
            temperature: 0.7,
            top_p: None,
            top_k: None,
            min_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            seed: None,
            stop: None,
            enable_thinking: None,
            stream: false,
        };
        let value = serde_json::to_value(body).unwrap();
        for field in [
            "top_p",
            "top_k",
            "min_p",
            "frequency_penalty",
            "presence_penalty",
            "repeat_penalty",
            "seed",
            "stop",
            "enable_thinking",
        ] {
            assert!(value.get(field).is_none(), "unexpected field {field}");
        }
    }

    #[test]
    fn sse_decoder_handles_arbitrary_fragmentation_and_multiple_events() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"why\"}}]}\r\n\r\n",
            "data: [DONE]\n\n"
        )
        .as_bytes();
        for split in 0..=wire.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&wire[..split]);
            events.extend(decoder.push(&wire[split..]));
            assert_eq!(events.len(), 3, "split at {split}");
            assert_eq!(tokens(&events[0]), vec![StreamToken::Content("one".into())]);
            assert_eq!(
                tokens(&events[1]),
                vec![StreamToken::Thinking("why".into())]
            );
            assert!(matches!(decode_sse_event(&events[2]), Ok(SseEvent::Done)));
            assert!(!decoder.has_unfinished_event());
        }
    }

    #[test]
    fn sse_surfaces_finish_reason_and_usage() {
        let event = br#"data: {"choices":[{"delta":{},"finish_reason":"length"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#;
        assert_eq!(
            tokens(event),
            vec![
                StreamToken::FinishReason("length".into()),
                StreamToken::Usage(ChatUsage {
                    prompt_tokens: 2,
                    completion_tokens: 3,
                    total_tokens: 5,
                }),
            ]
        );
    }

    #[test]
    fn sse_accepts_multiline_data_with_crlf_framing() {
        let event = b"event: message\r\ndata: {\"choices\":[{\"delta\":\r\ndata: {\"content\":\"joined\"}}]}\r\n";
        assert_eq!(tokens(event), vec![StreamToken::Content("joined".into())]);
    }

    #[tokio::test]
    async fn stream_enforces_first_semantic_token_and_idle_timeouts() {
        let mut timeouts = LemonadeTimeouts::default();
        timeouts.first_token = std::time::Duration::from_millis(40);
        timeouts.stream_idle = std::time::Duration::from_millis(40);
        let (provider, server) = mock_stream_provider(
            vec![
                (std::time::Duration::from_millis(10), b": keepalive\n\n"),
                (std::time::Duration::from_millis(80), b"data: [DONE]\n\n"),
            ],
            timeouts,
            None,
        )
        .await;
        let mut stream = provider.complete_stream(ChatRequest::new(vec![ChatMessage::user("x")]));
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), stream.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("first token timeout"));
        server.abort();

        let (provider, server) = mock_stream_provider(
            vec![
                (
                    std::time::Duration::ZERO,
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                ),
                (std::time::Duration::from_millis(80), b"data: [DONE]\n\n"),
            ],
            timeouts,
            None,
        )
        .await;
        let mut stream = provider.complete_stream(ChatRequest::new(vec![ChatMessage::user("x")]));
        assert!(matches!(
            stream.recv().await.unwrap().unwrap(),
            StreamToken::Content(_)
        ));
        let error = stream.recv().await.unwrap().unwrap_err();
        assert!(error.to_string().contains("stream idle timeout"));
        server.abort();
    }

    #[tokio::test]
    async fn receiver_cancellation_releases_gpu_guard() {
        let mut writes = Vec::new();
        for _ in 0..20 {
            writes.push((
                std::time::Duration::from_millis(5),
                b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n" as &'static [u8],
            ));
        }
        let gpu = GpuResourceManager::new();
        let (provider, server) =
            mock_stream_provider(writes, LemonadeTimeouts::default(), Some(gpu.clone())).await;
        let mut stream = provider.complete_stream(ChatRequest::new(vec![ChatMessage::user("x")]));
        let _ = stream.recv().await;
        assert_eq!(gpu.current_workload(), super::super::GpuWorkload::LlmActive);
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while gpu.current_workload() != super::super::GpuWorkload::Idle {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.abort();
    }

    #[test]
    fn sse_rejects_malformed_json_protocol_and_server_errors() {
        assert!(decode_sse_event(b"data: {not-json}").is_err());
        assert!(decode_sse_event(b"unexpected: value").is_err());
        assert!(decode_sse_event(b"data: \xff").is_err());
        assert!(decode_sse_event(b"event: error\ndata: failed").is_err());
        let error = decode_sse_event(br#"data: {"error":{"message":"boom"}}"#)
            .err()
            .expect("server error must be surfaced");
        assert!(error.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn test_chat_request_returns_valid_response() {
        let url = require_integration_url!();
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url)
            .await
            .unwrap();
        let Some(llm) = downloaded_test_llm(&catalog) else {
            eprintln!("SKIP: {GPU_CPU_TEST_LLM_MODEL} is not downloaded");
            return;
        };
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::new(&url, &llm.id, Some(gpu));

        let request = ChatRequest::new(vec![ChatMessage::user("Reply with: pong")])
            .with_max_tokens(4)
            .with_temperature(0.0);
        let response = chat.complete(request).await.expect("Chat request failed");
        assert!(!response.id.is_empty(), "Chat response should have an id");
        assert!(
            !response.choices.is_empty(),
            "Chat response should contain at least one choice"
        );
    }

    #[tokio::test]
    async fn test_chat_request_with_overrides() {
        let url = require_integration_url!();
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url)
            .await
            .unwrap();
        let Some(llm) = downloaded_test_llm(&catalog) else {
            eprintln!("SKIP: {GPU_CPU_TEST_LLM_MODEL} is not downloaded");
            return;
        };
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::new(&url, &llm.id, Some(gpu));

        let req = ChatRequest::new(vec![ChatMessage::user("Count to three.")])
            .with_max_tokens(64)
            .with_temperature(0.1);

        let resp = chat.complete(req).await.unwrap();
        assert!(resp.first_content().is_some());
    }
}
