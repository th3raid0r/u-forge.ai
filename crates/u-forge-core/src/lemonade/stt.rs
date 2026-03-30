//! GPU-managed speech-to-text via Whisper on Lemonade Server.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::client::LemonadeHttpClient;
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
    client: LemonadeHttpClient,
    /// The model id sent to the API (e.g. `"Whisper-Large-v3-Turbo"`).
    pub model: String,
    /// Shared GPU resource manager — also held by [`LemonadeChatProvider`](super::LemonadeChatProvider).
    pub gpu: Arc<GpuResourceManager>,
}

impl LemonadeSttProvider {
    /// Construct with an explicit base URL, model id, and GPU manager.
    pub fn new(base_url: &str, model: &str, gpu: Arc<GpuResourceManager>) -> Self {
        Self {
            client: LemonadeHttpClient::new(base_url),
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

        let audio_part = reqwest::multipart::Part::bytes(audio_data)
            .file_name(filename.to_string())
            .mime_str("audio/wav")
            .context("Failed to set audio MIME type")?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", audio_part);

        let result: TranscriptionResult = self
            .client
            .post_multipart("/audio/transcriptions", form)
            .await
            .context("STT HTTP request failed")?;

        tracing::debug!(
            model        = %self.model,
            text_len     = result.text.len(),
            duration_ms  = start.elapsed().as_millis(),
            "STT transcription complete"
        );

        Ok(result)
        // _guard is dropped here → GPU released, queued LLM requests are woken.
    }
}
