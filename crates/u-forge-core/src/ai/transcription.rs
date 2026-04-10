//! transcription.rs — Audio-to-text transcription providers for u-forge.ai.
//!
//! This module is the single home for all speech-to-text (STT) and
//! voice-to-text (VTT) concerns.  It was split out of `embeddings.rs` so that
//! embedding and transcription can evolve independently and be routed to
//! different hardware by the
//! [`InferenceQueue`](crate::inference_queue::InferenceQueue).
//!
//! # Providers
//!
//! | Type | Hardware | Notes |
//! |------|----------|-------|
//! | [`LemonadeTranscriptionProvider`] | NPU / GPU (via Lemonade) | Simple, no GPU contention logic |
//!
//! For GPU-managed STT (where the GPU is shared with LLM inference) use
//! [`LemonadeSttProvider`](crate::lemonade::LemonadeSttProvider) from
//! `lemonade.rs` together with its [`GpuResourceManager`](crate::lemonade::GpuResourceManager).
//! The [`InferenceQueue`] wires both together transparently.
//!
//! # Quick start
//!
//! ```no_run
//! # use u_forge_core::ai::transcription::TranscriptionManager;
//! # async fn example() -> anyhow::Result<()> {
//! // Auto: reads LEMONADE_URL env var, defaults to whisper-v3-turbo-FLM
//! let mgr = TranscriptionManager::try_new_auto(None, None).await?;
//! let wav  = std::fs::read("session.wav")?;
//! let text = mgr.get_provider().transcribe(wav, "session.wav").await?;
//! println!("{text}");
//! # Ok(()) }
//! ```

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info};

use crate::lemonade::LemonadeHttpClient;

// ── TranscriptionProvider trait ───────────────────────────────────────────────

/// Core trait for all audio-to-text transcription backends.
///
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks and placed behind an `Arc`.
///
/// # MIME inference
///
/// The `filename` parameter is used as a hint to determine the audio MIME type
/// for the multipart upload.  Recognised extensions:
///
/// | Extension | MIME type    |
/// |-----------|--------------|
/// | `.mp3`    | `audio/mpeg` |
/// | `.ogg`    | `audio/ogg`  |
/// | anything  | `audio/wav`  |
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    /// Transcribe raw audio bytes to text.
    ///
    /// `audio_bytes` — contents of a valid audio file (WAV, MP3, OGG, …).
    /// `filename`    — sent as the multipart filename hint (e.g. `"session.wav"`).
    ///
    /// Returns the transcribed text trimmed of leading/trailing whitespace.
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> Result<String>;

    /// The model name this provider is configured to use.
    fn model_name(&self) -> &str;
}

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
///   [`InferenceQueue`](crate::inference_queue::InferenceQueue)).
pub struct LemonadeTranscriptionProvider {
    client: LemonadeHttpClient,
    /// Whisper model identifier, e.g. `"whisper-v3-turbo-FLM"`.
    model: String,
}

impl LemonadeTranscriptionProvider {
    /// Create a new provider pointed at the given Lemonade Server.
    ///
    /// Construction is cheap and **synchronous** — no probe request is made.
    /// Errors are only surfaced when [`transcribe`] is called.
    ///
    /// [`transcribe`]: TranscriptionProvider::transcribe
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
        let start = std::time::Instant::now();

        let mime = mime_for_filename(filename);

        let part = reqwest::multipart::Part::bytes(audio_bytes)
            .file_name(filename.to_string())
            .mime_str(mime)
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
/// Mirrors the pattern of
/// [`EmbeddingManager`](crate::embeddings::EmbeddingManager): construct once,
/// clone the `Arc<dyn TranscriptionProvider>` into as many async tasks as needed.
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
        info!(
            base_url,
            model, "TranscriptionManager: using Lemonade Server"
        );
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

// ── MIME helper (public for reuse in lemonade.rs / hardware modules) ──────────

/// Infer the audio MIME type from a filename extension.
///
/// | Extension | Returns      |
/// |-----------|--------------|
/// | `.mp3`    | `audio/mpeg` |
/// | `.ogg`    | `audio/ogg`  |
/// | `.flac`   | `audio/flac` |
/// | `.m4a`    | `audio/mp4`  |
/// | anything  | `audio/wav`  |
pub fn mime_for_filename(filename: &str) -> &'static str {
    let lower = filename.to_lowercase();
    if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".ogg") {
        "audio/ogg"
    } else if lower.ends_with(".flac") {
        "audio/flac"
    } else if lower.ends_with(".m4a") {
        "audio/mp4"
    } else {
        "audio/wav"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;

    // ── WAV helper ────────────────────────────────────────────────────────────

    /// Build a minimal valid PCM WAV file containing silence.
    ///
    /// Parameters: mono, 16-bit, 16 kHz.  `duration_secs` controls length.
    /// No external dependencies — pure byte construction following the RIFF spec.
    pub(crate) fn make_silence_wav(duration_secs: f32) -> Vec<u8> {
        let sample_rate: u32 = 16_000;
        let num_channels: u16 = 1;
        let bits_per_sample: u16 = 16;
        let num_samples = (sample_rate as f32 * duration_secs) as u32;
        let data_size = num_samples * (bits_per_sample as u32 / 8) * num_channels as u32;
        // RIFF chunk size = 4 (WAVE) + 8 (fmt hdr) + 16 (fmt body) + 8 (data hdr) + data
        let riff_size: u32 = 4 + 8 + 16 + 8 + data_size;

        let mut buf: Vec<u8> = Vec::with_capacity((8 + riff_size) as usize);

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt sub-chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size (PCM)
        buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
        buf.extend_from_slice(&num_channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate: u32 = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align: u16 = num_channels * bits_per_sample / 8;
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());

        // data sub-chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend(std::iter::repeat(0u8).take(data_size as usize));

        buf
    }

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[tokio::test]
    async fn test_transcription_manager_fails_with_no_url() {
        // try_new_auto(None, None) should fail when no server is discoverable
        // and LEMONADE_URL is unset.  Skip if a server IS reachable (the error
        // path wouldn't be exercised).
        if crate::test_helpers::integration_test_url().await.is_some() {
            eprintln!("SKIP: Lemonade Server is reachable — cannot test no-URL error path");
            return;
        }
        let result = TranscriptionManager::try_new_auto(None, None).await;
        assert!(
            result.is_err(),
            "Expected error when no URL is configured, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LEMONADE_URL"),
            "Error should mention LEMONADE_URL, got: {msg}"
        );
    }

    #[test]
    fn test_transcription_manager_debug_format() {
        let mgr = TranscriptionManager::new_lemonade(
            "http://localhost:13305/api/v1",
            "whisper-v3-turbo-FLM",
        );
        let s = format!("{:?}", mgr);
        assert!(
            s.contains("TranscriptionManager"),
            "missing struct name: {s}"
        );
        assert!(
            s.contains("whisper-v3-turbo-FLM"),
            "missing model name: {s}"
        );
    }

    #[test]
    fn test_lemonade_transcription_provider_model_name() {
        let p = LemonadeTranscriptionProvider::new(
            "http://localhost:13305/api/v1",
            "whisper-v3-turbo-FLM",
        );
        assert_eq!(p.model_name(), "whisper-v3-turbo-FLM");
    }

    #[test]
    fn test_provider_trims_trailing_slash_from_url() {
        let p = LemonadeTranscriptionProvider::new(
            "http://localhost:13305/api/v1/",
            "whisper-v3-turbo-FLM",
        );
        // base_url should not end in '/'
        assert!(
            !p.client.base_url.ends_with('/'),
            "base_url should not end with '/': {}",
            p.client.base_url
        );
    }

    #[test]
    fn test_make_silence_wav_valid_header() {
        let wav = make_silence_wav(0.1);
        // RIFF magic
        assert_eq!(&wav[0..4], b"RIFF", "Missing RIFF magic");
        // WAVE fourcc
        assert_eq!(&wav[8..12], b"WAVE", "Missing WAVE fourcc");
        // fmt sub-chunk
        assert_eq!(&wav[12..16], b"fmt ", "Missing fmt chunk");
        // Audio format = 1 (PCM)
        let audio_fmt = u16::from_le_bytes([wav[20], wav[21]]);
        assert_eq!(audio_fmt, 1, "Expected PCM format (1), got {audio_fmt}");
        // Sample rate = 16 000
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16_000, "Expected 16 kHz sample rate, got {sr}");
        // Channels = 1 (mono)
        let ch = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(ch, 1, "Expected mono (1), got {ch}");
    }

    #[test]
    fn test_make_silence_wav_size_scaling() {
        let wav_short = make_silence_wav(0.5);
        let wav_long = make_silence_wav(1.0);
        assert!(
            wav_long.len() > wav_short.len(),
            "Longer duration must produce a larger file"
        );
        // 1.0 s should be roughly double 0.5 s (within header overhead)
        let ratio = wav_long.len() as f64 / wav_short.len() as f64;
        assert!(
            ratio > 1.8 && ratio < 2.2,
            "Expected ~2× size ratio, got {ratio:.2}"
        );
    }

    #[test]
    fn test_mime_for_filename() {
        assert_eq!(mime_for_filename("track.mp3"), "audio/mpeg");
        assert_eq!(mime_for_filename("session.ogg"), "audio/ogg");
        assert_eq!(mime_for_filename("recording.flac"), "audio/flac");
        assert_eq!(mime_for_filename("voice.m4a"), "audio/mp4");
        assert_eq!(mime_for_filename("audio.wav"), "audio/wav");
        assert_eq!(mime_for_filename("unknown.xyz"), "audio/wav");
        // Case-insensitive
        assert_eq!(mime_for_filename("TRACK.MP3"), "audio/mpeg");
    }

    #[test]
    fn test_manager_from_provider() {
        struct Echo;
        #[async_trait::async_trait]
        impl TranscriptionProvider for Echo {
            async fn transcribe(&self, _: Vec<u8>, filename: &str) -> Result<String> {
                Ok(filename.to_string())
            }
            fn model_name(&self) -> &str {
                "echo"
            }
        }

        let mgr = TranscriptionManager::from_provider(Arc::new(Echo));
        assert_eq!(mgr.get_provider().model_name(), "echo");
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_lemonade_transcribe_silence_wav() {
        let url = require_integration_url!();
        let provider = LemonadeTranscriptionProvider::new(&url, "whisper-v3-turbo-FLM");

        // 1 second of silence — valid WAV, no speech content.
        let wav = make_silence_wav(1.0);
        let result = provider.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "transcribe() failed on silence WAV: {:?}",
            result.err()
        );
        // May be empty or contain hallucinated noise words — both are acceptable.
        let _text = result.unwrap();
    }

    #[tokio::test]
    async fn test_lemonade_transcribe_error_on_empty_body() {
        let url = require_integration_url!();
        let provider = LemonadeTranscriptionProvider::new(&url, "whisper-v3-turbo-FLM");

        // Sending an empty byte slice — the server should return an error.
        let result = provider.transcribe(vec![], "empty.wav").await;
        assert!(
            result.is_err(),
            "Expected error for empty audio, got Ok: {:?}",
            result.ok()
        );
    }
}
