//! Text-to-speech via kokoro-v1 running on CPU.

use anyhow::{Context, Result};
use async_openai::{Client, config::OpenAIConfig};
use async_openai::types::audio::{CreateSpeechRequestArgs, SpeechModel, Voice};
use serde::{Deserialize, Serialize};

use super::client::make_lemonade_openai_client;

/// Built-in voices supported by kokoro-v1.
///
/// Pass [`KokoroVoice::Custom`] to use any voice string the server accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum KokoroVoice {
    /// American English female (default, high quality).
    #[default]
    AfSky,
    /// American English female, warmer tone.
    AfHeart,
    /// American English male.
    AmAdam,
    /// British English male.
    BmGeorge,
    /// British English female.
    BfEmma,
    /// Arbitrary voice identifier forwarded verbatim to the API.
    Custom(String),
}

impl KokoroVoice {
    /// The voice identifier string expected by the Lemonade / kokoro API.
    pub fn as_str(&self) -> &str {
        match self {
            KokoroVoice::AfSky => "af_sky",
            KokoroVoice::AfHeart => "af_heart",
            KokoroVoice::AmAdam => "am_adam",
            KokoroVoice::BmGeorge => "bm_george",
            KokoroVoice::BfEmma => "bf_emma",
            KokoroVoice::Custom(v) => v.as_str(),
        }
    }

    fn to_oa_voice(&self) -> Voice {
        Voice::Other(self.as_str().to_string())
    }
}

impl std::fmt::Display for KokoroVoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Text-to-speech via kokoro-v1 running on CPU.
///
/// Calls `POST /api/v1/audio/speech` and returns the raw audio bytes.
/// The response content type is typically `audio/wav`, but inspect the
/// `Content-Type` header of the raw HTTP response if you need to be certain.
///
/// This provider does **not** interact with [`GpuResourceManager`](super::GpuResourceManager)
/// because kokoro runs entirely on the CPU.
#[derive(Debug, Clone)]
pub struct LemonadeTtsProvider {
    client: Client<OpenAIConfig>,
    /// The model id sent to the API (e.g. `"kokoro-v1"`).
    pub model: String,
    /// Voice used when none is specified at call time.
    pub default_voice: KokoroVoice,
}

impl LemonadeTtsProvider {
    /// Construct with an explicit base URL and model id.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: make_lemonade_openai_client(base_url),
            model: model.to_string(),
            default_voice: KokoroVoice::default(),
        }
    }

    /// Override the default voice.
    pub fn with_voice(mut self, voice: KokoroVoice) -> Self {
        self.default_voice = voice;
        self
    }

    /// Synthesize `text` into audio.
    ///
    /// `voice` overrides `self.default_voice` for this call only.
    /// Returns raw audio bytes (typically WAV).
    pub async fn synthesize(&self, text: &str, voice: Option<&KokoroVoice>) -> Result<Vec<u8>> {
        let effective_voice = voice.unwrap_or(&self.default_voice);
        let start = std::time::Instant::now();

        let req = CreateSpeechRequestArgs::default()
            .model(SpeechModel::Other(self.model.clone()))
            .input(text)
            .voice(effective_voice.to_oa_voice())
            .build()
            .context("Failed to build TTS request")?;

        let response = self
            .client
            .audio()
            .speech()
            .create(req)
            .await
            .context("TTS HTTP request failed")?;

        let bytes = response.bytes.to_vec();

        tracing::debug!(
            model        = %self.model,
            voice        = %effective_voice,
            input_chars  = text.len(),
            output_bytes = bytes.len(),
            duration_ms  = start.elapsed().as_millis(),
            "TTS synthesis complete"
        );

        Ok(bytes)
    }

    /// Synthesize using `self.default_voice`.
    pub async fn synthesize_default(&self, text: &str) -> Result<Vec<u8>> {
        let voice = self.default_voice.clone();
        self.synthesize(text, Some(&voice)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;

    #[test]
    fn test_kokoro_voice_as_str() {
        assert_eq!(KokoroVoice::AfSky.as_str(), "af_sky");
        assert_eq!(KokoroVoice::AfHeart.as_str(), "af_heart");
        assert_eq!(KokoroVoice::AmAdam.as_str(), "am_adam");
        assert_eq!(KokoroVoice::BmGeorge.as_str(), "bm_george");
        assert_eq!(KokoroVoice::BfEmma.as_str(), "bf_emma");
        assert_eq!(
            KokoroVoice::Custom("af_custom".into()).as_str(),
            "af_custom"
        );
    }

    #[test]
    fn test_kokoro_voice_default() {
        assert_eq!(KokoroVoice::default(), KokoroVoice::AfSky);
    }

    // ── Integration: TTS (requires Lemonade Server) ────────────────────────────

    #[tokio::test]
    async fn test_tts_returns_audio_bytes() {
        let url = require_integration_url!();
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url).await.unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let tts_sel = selector.select_tts().expect("No TTS model found in catalog");
        let tts = LemonadeTtsProvider::new(&url, &tts_sel.model_id);

        let audio = tts.synthesize_default("Hello, adventurer!").await.unwrap();
        assert!(!audio.is_empty(), "TTS should return non-empty audio bytes");
    }
}
