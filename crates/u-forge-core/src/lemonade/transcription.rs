//! Lemonade-backed transcription provider (no GPU lock).
//!
//! This module contains the [`LemonadeTranscriptionProvider`] implementation of
//! [`TranscriptionProvider`].  The trait definitions live in
//! [`crate::ai::transcription`] and are dependency-free; this module handles
//! all Lemonade-specific HTTP logic.
//!
//! For GPU-locked STT with resource contention management, see
//! [`LemonadeSttProvider`](super::stt::LemonadeSttProvider).

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::ai::transcription::{TranscriptionProvider, mime_for_filename};
use super::client::LemonadeHttpClient;

// ── LemonadeTranscriptionProvider ─────────────────────────────────────────────

/// Transcription provider backed by
/// [Lemonade Server](https://github.com/lemonade-sdk/lemonade).
///
/// Uses the OpenAI-compatible `POST /api/v1/audio/transcriptions` endpoint with
/// a `multipart/form-data` body.  The server must be running and the whisper
/// model must be pulled before use.
///
/// This provider is fully async — no Tokio threads are ever blocked.
///
/// Unlike [`LemonadeSttProvider`](crate::lemonade::LemonadeSttProvider), this
/// provider has **no** [`GpuResourceManager`](crate::lemonade::GpuResourceManager)
/// attached — it is intentionally simple and stateless.  Use it when:
///
/// * The model runs on the **NPU** (dedicated silicon, no GPU contention), or
/// * You are managing resource exclusion at a higher level (e.g. the
///   [`InferenceQueue`](crate::queue::InferenceQueue)).
pub struct LemonadeTranscriptionProvider {
    pub(crate) client: LemonadeHttpClient,
    /// Whisper model identifier, e.g. `"whisper-v3-turbo-FLM"`.
    model: String,
}

impl LemonadeTranscriptionProvider {
    /// Create a new provider pointed at the given Lemonade Server.
    ///
    /// Construction is cheap and **synchronous** — no probe request is made.
    /// Errors are only surfaced when [`transcribe`](TranscriptionProvider::transcribe)
    /// is called.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: LemonadeHttpClient::new(base_url),
            model: model.to_string(),
        }
    }
}

#[async_trait]
impl TranscriptionProvider for LemonadeTranscriptionProvider {
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> Result<String> {
        use tracing::debug;
        let start = std::time::Instant::now();

        let mime = mime_for_filename(filename);

        let part = reqwest::multipart::Part::bytes(audio_bytes)
            .file_name(filename.to_string())
            .mime_str(&mime)
            .map_err(|e| anyhow!("Invalid MIME type '{}': {}", mime, e))?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);

        let resp: serde_json::Value = self
            .client
            .post_multipart("/audio/transcriptions", form)
            .await
            .map_err(|e| anyhow!("Lemonade transcription request failed: {}", e))?;

        // Surface server-side errors as Rust errors.
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("Lemonade transcription error: {}", err));
        }

        let text = resp["text"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing 'text' field in transcription response: {}", resp))?
            .trim()
            .to_string();

        debug!(
            model    = %self.model,
            filename,
            text_len = text.len(),
            duration_ms = start.elapsed().as_millis(),
            "Transcription completed"
        );

        Ok(text)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

