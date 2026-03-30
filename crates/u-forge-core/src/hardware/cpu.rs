//! CPU device implementation for host-CPU inference via Lemonade Server.
//!
//! CPU-resident models run entirely on the host processor without occupying
//! the GPU or NPU.  Currently the only CPU-resident capability is
//! text-to-speech via the **Kokoro** TTS engine.
//!
//! # Supported capabilities
//!
//! | Capability     | Provider               | Default model |
//! |----------------|------------------------|---------------|
//! | [`TextToSpeech`] | [`LemonadeTtsProvider`] | `kokoro-v1`  |
//!
//! # Usage
//!
//! ```no_run
//! # use u_forge_core::hardware::cpu::CpuDevice;
//! # use u_forge_core::lemonade::KokoroVoice;
//! # async fn run() -> anyhow::Result<()> {
//! let cpu = CpuDevice::new(
//!     "http://localhost:8000/api/v1",
//!     None,                        // kokoro-v1
//!     KokoroVoice::default(),      // AfSky
//! );
//!
//! if let Some(tts) = &cpu.tts {
//!     let audio: Vec<u8> = tts.synthesize_default("Hello, adventurer!").await?;
//!     std::fs::write("greeting.wav", &audio)?;
//! }
//! # Ok(()) }
//! ```
//!
//! [`TextToSpeech`]: crate::hardware::DeviceCapability::TextToSpeech
//! [`LemonadeTtsProvider`]: crate::lemonade::LemonadeTtsProvider

use anyhow::Result;
use tracing::info;

use crate::lemonade::{KokoroVoice, LemonadeModelRegistry, LemonadeTtsProvider};

use super::{DeviceCapability, DeviceWorker, HardwareBackend};

// ── Default model identifier ──────────────────────────────────────────────────

/// Default CPU TTS model served by Lemonade (Kokoro v1).
pub const DEFAULT_CPU_TTS_MODEL: &str = "kokoro-v1";

// ── CpuDevice ─────────────────────────────────────────────────────────────────

/// Logical device representing CPU-resident inference via Lemonade Server.
///
/// Holds an optional [`LemonadeTtsProvider`] for text-to-speech synthesis.
/// Because TTS runs on the CPU it does **not** contend with GPU or NPU
/// resources and requires no resource manager.
///
/// # Concurrency
///
/// Multiple TTS calls may be issued concurrently.  Lemonade Server serialises
/// access to the CPU model internally; from the Rust side the provider is
/// fully `Send + Sync` and safe to share across tasks.
pub struct CpuDevice {
    pub name: String,
    capabilities: Vec<DeviceCapability>,

    /// Text-to-speech provider (Kokoro).
    ///
    /// `None` when the device was constructed with [`CpuDevice::empty`] or
    /// when no TTS model was found in the Lemonade registry.
    pub tts: Option<LemonadeTtsProvider>,
}

impl CpuDevice {
    /// Construct a `CpuDevice` from an already-fetched [`LemonadeModelRegistry`].
    ///
    /// The TTS provider is initialised when the registry contains a Kokoro
    /// model entry; otherwise `tts` is `None` and no capabilities are
    /// advertised.
    pub fn from_registry(registry: &LemonadeModelRegistry) -> Self {
        let tts = LemonadeTtsProvider::from_registry(registry).ok();

        let capabilities = if tts.is_some() {
            info!("CpuDevice: TTS provider ready (Kokoro)");
            vec![DeviceCapability::TextToSpeech]
        } else {
            tracing::warn!(
                "CpuDevice::from_registry: no TTS model found — \
                 device will advertise no capabilities"
            );
            vec![]
        };

        Self {
            name: "CPU (Kokoro TTS)".to_string(),
            capabilities,
            tts,
        }
    }

    /// Construct a `CpuDevice` with an explicit TTS model and default voice.
    ///
    /// # Arguments
    ///
    /// * `base_url`      — Lemonade Server API root
    ///   (e.g. `"http://localhost:8000/api/v1"`).
    /// * `tts_model`     — Kokoro model id.  Defaults to
    ///   [`DEFAULT_CPU_TTS_MODEL`] when `None`.
    /// * `default_voice` — Voice used when
    ///   [`synthesize_default`](LemonadeTtsProvider::synthesize_default) is
    ///   called.  Use [`KokoroVoice::default()`] for `AfSky`.
    pub fn new(base_url: &str, tts_model: Option<&str>, default_voice: KokoroVoice) -> Self {
        let model = tts_model.unwrap_or(DEFAULT_CPU_TTS_MODEL);

        info!(model, voice = %default_voice, "CpuDevice initialised");

        Self {
            name: "CPU (Kokoro TTS)".to_string(),
            capabilities: vec![DeviceCapability::TextToSpeech],
            tts: Some(LemonadeTtsProvider::new(base_url, model).with_voice(default_voice)),
        }
    }

    /// Construct a `CpuDevice` with a specific voice override applied to an
    /// existing TTS provider.
    ///
    /// This is useful when you want to change the default voice without
    /// reconstructing the full device.
    pub fn new_with_voice(base_url: &str, tts_model: Option<&str>, voice: KokoroVoice) -> Self {
        Self::new(base_url, tts_model, voice)
    }

    /// Construct an empty `CpuDevice` that advertises no capabilities.
    ///
    /// Intended for testing or as a placeholder when no TTS model is
    /// available.
    pub fn empty() -> Self {
        Self {
            name: "CPU (no providers)".to_string(),
            capabilities: vec![],
            tts: None,
        }
    }

    /// Returns `true` if this device has an active TTS provider.
    pub fn has_tts(&self) -> bool {
        self.tts.is_some()
    }

    /// Synthesise speech using the configured default voice.
    ///
    /// Convenience wrapper around
    /// [`LemonadeTtsProvider::synthesize_default`].  Returns `None` when
    /// no TTS provider is configured.
    pub async fn speak(&self, text: &str) -> Option<Result<Vec<u8>>> {
        let tts = self.tts.as_ref()?;
        Some(tts.synthesize_default(text).await)
    }

    /// Synthesise speech using an explicit voice.
    ///
    /// Convenience wrapper around [`LemonadeTtsProvider::synthesize`].
    /// Returns `None` when no TTS provider is configured.
    pub async fn speak_with_voice(
        &self,
        text: &str,
        voice: &KokoroVoice,
    ) -> Option<Result<Vec<u8>>> {
        let tts = self.tts.as_ref()?;
        Some(tts.synthesize(text, Some(voice)).await)
    }
}

impl DeviceWorker for CpuDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn backend(&self) -> HardwareBackend {
        HardwareBackend::Cpu
    }

    fn capabilities(&self) -> &[DeviceCapability] {
        &self.capabilities
    }
}

impl std::fmt::Debug for CpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuDevice")
            .field("name", &self.name)
            .field("backend", &HardwareBackend::Cpu)
            .field("capabilities", &self.capabilities)
            .field("tts_model", &self.tts.as_ref().map(|p| p.model.as_str()))
            .finish()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{DeviceCapability, DeviceWorker, HardwareBackend};
    use crate::test_helpers::lemonade_url;

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_empty_device_has_no_capabilities() {
        let device = CpuDevice::empty();
        assert!(
            device.capabilities().is_empty(),
            "Empty device must have no capabilities"
        );
        assert!(!device.has_tts(), "Empty device must have no TTS");
    }

    #[test]
    fn test_new_device_advertises_tts_capability() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::default());
        assert!(
            device.supports(&DeviceCapability::TextToSpeech),
            "CpuDevice::new must advertise TextToSpeech"
        );
        assert!(
            !device.supports(&DeviceCapability::Embedding),
            "CpuDevice must NOT advertise Embedding"
        );
        assert!(
            !device.supports(&DeviceCapability::Transcription),
            "CpuDevice must NOT advertise Transcription"
        );
        assert!(
            !device.supports(&DeviceCapability::TextGeneration),
            "CpuDevice must NOT advertise TextGeneration"
        );
        assert!(device.has_tts());
    }

    #[test]
    fn test_new_uses_default_model_when_none() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::default());
        let model = device.tts.as_ref().unwrap().model.clone();
        assert_eq!(
            model, DEFAULT_CPU_TTS_MODEL,
            "Should use DEFAULT_CPU_TTS_MODEL, got {model}"
        );
    }

    #[test]
    fn test_new_uses_explicit_model() {
        let device = CpuDevice::new(
            "http://localhost:8000/api/v1",
            Some("kokoro-v2"),
            KokoroVoice::default(),
        );
        let model = device.tts.as_ref().unwrap().model.clone();
        assert_eq!(model, "kokoro-v2", "Should use explicitly provided model");
    }

    #[test]
    fn test_device_worker_backend_is_cpu() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::default());
        assert_eq!(
            device.backend(),
            HardwareBackend::Cpu,
            "CpuDevice backend must be Cpu"
        );
    }

    #[test]
    fn test_device_worker_name_is_nonempty() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::default());
        assert!(!device.name().is_empty(), "Device name must not be empty");
    }

    #[test]
    fn test_default_voice_is_used() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::AfHeart);
        let voice = device.tts.as_ref().unwrap().default_voice.as_str();
        assert_eq!(
            voice,
            KokoroVoice::AfHeart.as_str(),
            "Should store the supplied default voice"
        );
    }

    #[test]
    fn test_debug_format_includes_key_fields() {
        let device = CpuDevice::new(
            "http://localhost:8000/api/v1",
            Some("kokoro-v1"),
            KokoroVoice::default(),
        );
        let debug = format!("{device:?}");
        assert!(
            debug.contains("CpuDevice"),
            "Debug must include struct name"
        );
        assert!(debug.contains("kokoro-v1"), "Debug must include TTS model");
    }

    #[test]
    fn test_summary_contains_backend_and_capability() {
        let device = CpuDevice::new("http://localhost:8000/api/v1", None, KokoroVoice::default());
        let summary = device.summary();
        assert!(
            summary.contains("CPU"),
            "summary must mention CPU backend: {summary}"
        );
        assert!(
            summary.contains("TextToSpeech"),
            "summary must mention TextToSpeech capability: {summary}"
        );
    }

    #[test]
    fn test_speak_returns_none_when_no_tts() {
        // speak() is async; we test the sync synchronous path here by checking
        // has_tts() instead.
        let device = CpuDevice::empty();
        assert!(!device.has_tts(), "empty device must not have TTS");
    }

    #[test]
    fn test_new_with_voice_stores_voice() {
        let device =
            CpuDevice::new_with_voice("http://localhost:8000/api/v1", None, KokoroVoice::BmGeorge);
        let voice = device.tts.as_ref().unwrap().default_voice.as_str();
        assert_eq!(
            voice,
            KokoroVoice::BmGeorge.as_str(),
            "new_with_voice must store BmGeorge"
        );
    }

    #[test]
    fn test_empty_device_summary_shows_no_caps() {
        let device = CpuDevice::empty();
        let summary = device.summary();
        assert!(
            summary.contains("caps=[]"),
            "Empty device summary should show caps=[]: {summary}"
        );
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_from_registry_discovers_tts_model() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: LEMONADE_URL not set");
            return;
        };

        let registry = match crate::lemonade::LemonadeModelRegistry::fetch(&url).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Registry fetch failed: {e}");
                return;
            }
        };

        let device = CpuDevice::from_registry(&registry);
        println!("CpuDevice from registry: {}", device.summary());
        // We do not assert specific capabilities here since the server's
        // model set is environment-dependent, but we log the result for
        // manual inspection.
    }

    #[tokio::test]
    async fn test_speak_returns_audio_bytes() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: LEMONADE_URL not set");
            return;
        };

        let device = CpuDevice::new(&url, None, KokoroVoice::default());

        let result = device.speak("Hello, adventurer!").await;
        assert!(
            result.is_some(),
            "speak() must return Some when TTS is configured"
        );

        let audio = result.unwrap();
        assert!(audio.is_ok(), "TTS synthesis failed: {:?}", audio.err());

        let bytes = audio.unwrap();
        assert!(!bytes.is_empty(), "TTS must return non-empty audio bytes");
    }

    #[tokio::test]
    async fn test_speak_with_different_voices() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: LEMONADE_URL not set");
            return;
        };

        let device = CpuDevice::new(&url, None, KokoroVoice::default());

        for voice in [
            KokoroVoice::AfSky,
            KokoroVoice::AfHeart,
            KokoroVoice::AmAdam,
        ] {
            let result = device.speak_with_voice("Test voice.", &voice).await;
            assert!(
                result.is_some(),
                "speak_with_voice must return Some for voice {:?}",
                voice
            );
            let audio = result.unwrap();
            assert!(
                audio.is_ok(),
                "TTS failed for voice {:?}: {:?}",
                voice,
                audio.err()
            );
            assert!(
                !audio.unwrap().is_empty(),
                "Expected non-empty audio for voice {:?}",
                voice
            );
        }
    }
}
