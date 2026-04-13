//! GPU-managed speech-to-text via Whisper on Lemonade Server.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_openai::{Client, config::OpenAIConfig};
use async_openai::types::{InputSource};
use async_openai::types::audio::{AudioInput, CreateTranscriptionRequestArgs};
use serde::{Deserialize, Serialize};

use async_trait::async_trait;

use crate::ai::transcription::TranscriptionProvider;

use super::client::make_lemonade_openai_client;
use super::gpu_manager::GpuResourceManager;
use super::registry::LemonadeModelRegistry;

/// Transcription result returned by the Whisper endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TranscriptionResult {
    /// The transcribed text.
    pub text: String,
}

/// Speech-to-text via `Whisper-Large-v3-Turbo` running on GPU.
///
/// Uses a shared [`GpuResourceManager`] to enforce the GPU sharing policy.
/// Calls to [`LemonadeSttProvider::transcribe`] will return an error immediately
/// if LLM inference is currently active — STT must never queue behind slow inference.
#[derive(Debug, Clone)]
pub struct LemonadeSttProvider {
    client: Client<OpenAIConfig>,
    /// The model id sent to the API (e.g. `"Whisper-Large-v3-Turbo"`).
    pub model: String,
    /// Shared GPU resource manager — also held by [`LemonadeChatProvider`](super::LemonadeChatProvider).
    pub gpu: Arc<GpuResourceManager>,
}

impl LemonadeSttProvider {
    /// Construct with an explicit base URL, model id, and GPU manager.
    pub fn new(base_url: &str, model: &str, gpu: Arc<GpuResourceManager>) -> Self {
        Self {
            client: make_lemonade_openai_client(base_url),
            model: model.to_string(),
            gpu,
        }
    }

    /// Construct using the STT model discovered in `registry`.
    pub fn from_registry(
        registry: &LemonadeModelRegistry,
        gpu: Arc<GpuResourceManager>,
    ) -> Result<Self> {
        let model = registry
            .stt_model()
            .ok_or_else(|| anyhow!("No STT model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id, gpu))
    }

    /// Transcribe `audio_data` to text.
    ///
    /// `audio_data` should be a valid audio file (WAV, MP3, OGG, FLAC, …).
    /// `filename` is the name hint sent to the server (e.g. `"recording.wav"`).
    ///
    /// # GPU Policy
    /// Returns `Err` immediately if LLM inference is currently occupying the GPU.
    /// The caller should surface this to the user as a "GPU busy" message and retry.
    pub async fn transcribe(
        &self,
        audio_data: Vec<u8>,
        filename: &str,
    ) -> Result<TranscriptionResult> {
        // Enforce GPU policy — STT is latency-sensitive, never queue.
        let _guard = self.gpu.begin_stt()?;

        let start = std::time::Instant::now();

        let audio_input = AudioInput {
            source: InputSource::VecU8 {
                filename: filename.to_string(),
                vec: audio_data,
            },
        };

        let req = CreateTranscriptionRequestArgs::default()
            .model(&self.model)
            .file(audio_input)
            .build()
            .context("Failed to build transcription request")?;

        // Use create_raw() because Lemonade's transcription response omits the
        // `usage` field that async-openai's typed `CreateTranscriptionResponseJson`
        // requires.  We parse only the `text` field we actually need.
        let raw = self
            .client
            .audio()
            .transcription()
            .create_raw(req)
            .await
            .context("STT HTTP request failed")?;

        let text = serde_json::from_slice::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v["text"].as_str().map(str::to_string))
            .ok_or_else(|| anyhow::anyhow!("STT response missing 'text' field"))?;

        tracing::debug!(
            model        = %self.model,
            text_len     = text.len(),
            duration_ms  = start.elapsed().as_millis(),
            "STT transcription complete"
        );

        Ok(TranscriptionResult { text })
        // _guard is dropped here → GPU released, queued LLM requests are woken.
    }
}

#[async_trait]
impl TranscriptionProvider for LemonadeSttProvider {
    /// Delegates to the inherent [`transcribe`](Self::transcribe) method and
    /// maps the result to a plain `String`.
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> anyhow::Result<String> {
        // Inherent `transcribe` has priority over this trait method.
        self.transcribe(audio_bytes, filename).await.map(|r| r.text)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
