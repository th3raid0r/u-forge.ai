//! Lemonade-backed transcription provider and manager.
//!
//! This module contains the [`LemonadeTranscriptionProvider`] implementation of
//! [`TranscriptionProvider`] and the [`TranscriptionManager`] convenience wrapper.
//! The trait definitions live in [`crate::ai::transcription`] and are
//! dependency-free; this module handles all Lemonade-specific HTTP logic.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::info;

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

// ── TranscriptionManager ──────────────────────────────────────────────────────

/// Owns a single [`TranscriptionProvider`] and hands out `Arc` references to it.
///
/// Construct via [`TranscriptionManager::try_new_auto`] for production use, or
/// [`TranscriptionManager::new_lemonade`] when the URL is known.
///
/// # Example
///
/// ```no_run
/// # use u_forge_core::ai::transcription::TranscriptionManager;
/// # async fn run() -> anyhow::Result<()> {
/// let mgr = TranscriptionManager::try_new_auto(None, None).await?;
/// let provider = mgr.get_provider();       // Arc<dyn TranscriptionProvider>
/// let wav = std::fs::read("session.wav")?;
/// let text = provider.transcribe(wav, "session.wav").await?;
/// # Ok(()) }
/// ```
pub struct TranscriptionManager {
    provider: Arc<dyn TranscriptionProvider>,
}

impl std::fmt::Debug for TranscriptionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptionManager")
            .field("model", &self.provider.model_name())
            .finish()
    }
}

impl TranscriptionManager {
    /// Create a manager backed directly by a Lemonade Server instance.
    ///
    /// Defaults to the NPU-optimised `whisper-v3-turbo-FLM` model when `model`
    /// is `None`.
    pub fn new_lemonade(base_url: &str, model: &str) -> Self {
        info!(base_url, model, "TranscriptionManager: using Lemonade Server");
        Self {
            provider: Arc::new(LemonadeTranscriptionProvider::new(base_url, model)),
        }
    }

    /// Auto-select a transcription provider from the environment.
    ///
    /// **Resolution order:**
    /// 1. `lemonade_url` argument (if `Some`)
    /// 2. `LEMONADE_URL` environment variable
    /// 3. Hard error — there is no silent local fallback for transcription
    ///
    /// `model` defaults to `"whisper-v3-turbo-FLM"` when `None`.
    pub async fn try_new_auto(lemonade_url: Option<&str>, model: Option<&str>) -> Result<Self> {
        let url = crate::lemonade::resolve_provider_url(lemonade_url, "LEMONADE_URL", false)
            .await
            .ok_or_else(|| {
                anyhow!(
                    "No Lemonade Server URL configured. Set the LEMONADE_URL environment \
                     variable or pass a URL explicitly:\n  \
                     export LEMONADE_URL=http://localhost:13305/api/v1"
                )
            })?;

        let whisper_model = model.unwrap_or("whisper-v3-turbo-FLM");
        Ok(Self::new_lemonade(&url, whisper_model))
    }

    /// Wrap an arbitrary [`TranscriptionProvider`] implementation.
    ///
    /// Useful in tests where a mock provider is preferred over a live server.
    pub fn from_provider(provider: Arc<dyn TranscriptionProvider>) -> Self {
        Self { provider }
    }

    /// Return a clone of the inner provider, suitable for passing to async tasks.
    pub fn get_provider(&self) -> Arc<dyn TranscriptionProvider> {
        self.provider.clone()
    }
}
