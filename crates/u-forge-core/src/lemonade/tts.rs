//! Text-to-speech via kokoro-v1 running on CPU.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::client::LemonadeHttpClient;
use super::registry::LemonadeModelRegistry;

/// Built-in voices supported by kokoro-v1.
///
/// Pass [`KokoroVoice::Custom`] to use any voice string the server accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KokoroVoice {
    /// American English female (default, high quality).
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
}

impl Default for KokoroVoice {
    fn default() -> Self {
        KokoroVoice::AfSky
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
    client: LemonadeHttpClient,
    /// The model id sent to the API (e.g. `"kokoro-v1"`).
    pub model: String,
    /// Voice used when none is specified at call time.
    pub default_voice: KokoroVoice,
}

impl LemonadeTtsProvider {
    /// Construct with an explicit base URL and model id.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: LemonadeHttpClient::new(base_url),
            model: model.to_string(),
            default_voice: KokoroVoice::default(),
        }
    }

    /// Construct using the TTS model discovered in `registry`.
    pub fn from_registry(registry: &LemonadeModelRegistry) -> Result<Self> {
        let model = registry
            .tts_model()
            .ok_or_else(|| anyhow!("No TTS model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id))
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
        let voice_str = voice.unwrap_or(&self.default_voice).as_str();
        let start = std::time::Instant::now();

        let body = serde_json::json!({
            "model": self.model,
            "input":  text,
            "voice":  voice_str,
        });

        let bytes = self
            .client
            .post_bytes("/audio/speech", &body)
            .await
            .context("TTS HTTP request failed")?;

        tracing::debug!(
            model    = %self.model,
            voice    = %voice_str,
            input_chars = text.len(),
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
    use crate::test_helpers::lemonade_url;

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

    // ── Integration: TTS (requires LEMONADE_URL) ──────────────────────────────

    #[tokio::test]
    async fn test_tts_returns_audio_bytes() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_tts_returns_audio_bytes — Lemonade Server not available");
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        let audio = tts.synthesize_default("Hello, adventurer!").await.unwrap();
        assert!(!audio.is_empty(), "TTS should return non-empty audio bytes");
        println!("TTS returned {} bytes of audio", audio.len());
    }

    #[tokio::test]
    async fn test_tts_multiple_voices() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        for voice in &[
            KokoroVoice::AfSky,
            KokoroVoice::AfHeart,
            KokoroVoice::AmAdam,
            KokoroVoice::BmGeorge,
        ] {
            let audio = tts
                .synthesize("The dungeon awaits.", Some(voice))
                .await
                .unwrap();
            assert!(
                !audio.is_empty(),
                "Voice {:?} should produce audio bytes",
                voice
            );
        }
    }

    #[tokio::test]
    async fn test_tts_long_text() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        let text = "Deep beneath the ancient mountain, \
                    where shadows cling to every stone, \
                    the adventurers discovered a chamber unlike any they had seen before. \
                    Runes of glowing amber lined the walls, pulsing with a rhythm like a \
                    heartbeat, and at the centre stood a pedestal bearing a single obsidian key.";

        let audio = tts.synthesize_default(text).await.unwrap();
        assert!(!audio.is_empty(), "Long-form TTS should return audio bytes");
    }
}
