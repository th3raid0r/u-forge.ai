//! GPU device implementation for AMD ROCm (and future CUDA) via Lemonade Server.
//!
//! The GPU is a shared resource: STT (speech-to-text) and LLM (large language
//! model) inference both run on the same physical device.  Access is coordinated
//! by a [`GpuResourceManager`](crate::lemonade::GpuResourceManager) that enforces
//! the following policy:
//!
//! | Situation | Outcome |
//! |-----------|---------|
//! | STT while LLM active | **Immediate error** — STT is latency-sensitive |
//! | LLM while STT active | **Queue** — resumes when STT guard is dropped |
//! | LLM while LLM active | **Queue** — LLM requests are serialised |
//! | STT while idle | Proceeds immediately |
//! | LLM while idle | Proceeds immediately |
//!
//! Embedding via llamacpp **does not** need the `GpuResourceManager` lock —
//! Lemonade Server routes llamacpp embedding calls through a different execution
//! path from whispercpp STT.  The `embedding` field is therefore independent of
//! the `gpu` resource manager.
//!
//! # Supported capabilities
//!
//! | Capability        | Provider               | Model example               |
//! |-------------------|------------------------|-----------------------------|
//! | [`Transcription`] | [`LemonadeSttProvider`]  | `Whisper-Large-v3-Turbo`  |
//! | [`TextGeneration`]| [`LemonadeChatProvider`] | `GLM-4.7-Flash-GGUF`      |
//! | [`Embedding`]     | [`LemonadeProvider`]     | `embeddinggemma-300M-GGUF` |
//!
//! # Usage
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use u_forge_core::hardware::gpu::GpuDevice;
//! # use u_forge_core::lemonade::{GpuResourceManager, LemonadeModelRegistry};
//! # async fn run() -> anyhow::Result<()> {
//! let registry = LemonadeModelRegistry::fetch("http://localhost:13305/api/v1").await?;
//! let gpu = GpuResourceManager::new();
//! let device = GpuDevice::from_registry(&registry, gpu).await;
//!
//! // Transcribe audio
//! if let Some(stt) = &device.stt {
//!     let audio_bytes: Vec<u8> = std::fs::read("recording.wav")?;
//!     let result = stt.transcribe(audio_bytes, "recording.wav").await?;
//!     println!("{}", result.text);
//! }
//!
//! // Chat / LLM inference
//! if let Some(chat) = &device.chat {
//!     let answer = chat.ask("What is the capital of France?").await?;
//!     println!("{answer}");
//! }
//! # Ok(()) }
//! ```
//!
//! [`Transcription`]: crate::hardware::DeviceCapability::Transcription
//! [`TextGeneration`]: crate::hardware::DeviceCapability::TextGeneration
//! [`Embedding`]: crate::hardware::DeviceCapability::Embedding
//! [`LemonadeSttProvider`]: crate::lemonade::LemonadeSttProvider
//! [`LemonadeChatProvider`]: crate::lemonade::LemonadeChatProvider
//! [`LemonadeProvider`]: crate::ai::embeddings::LemonadeProvider

use std::sync::Arc;

use tracing::info;

use crate::ai::embeddings::{EmbeddingProvider, LemonadeProvider};
use crate::config::ModelConfig;
use crate::lemonade::{
    GpuResourceManager, LemonadeChatProvider, LemonadeModelRegistry, LemonadeSttProvider,
};

use crate::lemonade::ModelLoadOptions;

use super::{DeviceCapability, DeviceWorker, HardwareBackend};

// ── Shared helper ─────────────────────────────────────────────────────────────

/// Construct a llamacpp embedding provider from the registry's
/// `llamacpp_embedding_model`, optionally loading it first.
///
/// Returns `None` when no llamacpp embedding model is registered or the
/// provider fails to connect.  Pushes [`DeviceCapability::Embedding`] into
/// `capabilities` on success.
async fn init_llamacpp_embedding(
    registry: &LemonadeModelRegistry,
    load_opts: Option<&ModelLoadOptions>,
    capabilities: &mut Vec<DeviceCapability>,
    device_label: &str,
) -> Option<Arc<dyn EmbeddingProvider>> {
    let model_entry = registry.llamacpp_embedding_model()?;
    let model_id = model_entry.id.clone();
    let result = match load_opts {
        Some(opts) => LemonadeProvider::new_with_load(&registry.base_url, &model_id, opts).await,
        None => LemonadeProvider::new(&registry.base_url, &model_id).await,
    };
    match result {
        Ok(p) => {
            capabilities.push(DeviceCapability::Embedding);
            info!(model = %model_id, "{device_label}: embedding provider ready");
            Some(Arc::new(p) as Arc<dyn EmbeddingProvider>)
        }
        Err(e) => {
            tracing::warn!(model = %model_id, error = %e, "{device_label}: embedding model not reachable");
            None
        }
    }
}

// ── GpuDevice ─────────────────────────────────────────────────────────────────

/// Logical device representing a GPU running via ROCm + Lemonade Server.
///
/// Both [`LemonadeSttProvider`] and [`LemonadeChatProvider`] share the same
/// [`GpuResourceManager`] to enforce the GPU scheduling policy described in the
/// module-level documentation.
///
/// # Resource sharing
///
/// The `gpu` field is an `Arc<GpuResourceManager>` — it can (and should) be
/// cloned into multiple contexts.  If you are building a full
/// [`LemonadeStack`](crate::lemonade::LemonadeStack), pass the stack's GPU
/// manager here so that all components agree on the current workload state.
///
/// # Optional providers
///
/// Both `stt` and `chat` are `Option<…>`.  A model that is not downloaded or not
/// present in the Lemonade registry will simply be `None`; the device still
/// advertises only the capabilities it can actually service.
pub struct GpuDevice {
    pub name: String,
    pub backend: HardwareBackend,
    capabilities: Vec<DeviceCapability>,

    /// Shared GPU resource manager.
    ///
    /// Held here so callers can inspect the current GPU workload state or share
    /// the manager with other components (e.g. a `LemonadeStack`).
    pub gpu: Arc<GpuResourceManager>,

    /// GPU speech-to-text provider.
    ///
    /// `None` when no STT model was found in the registry or provided explicitly.
    pub stt: Option<LemonadeSttProvider>,

    /// GPU large-language-model chat provider.
    ///
    /// `None` when no LLM model was found in the registry or provided explicitly.
    pub chat: Option<LemonadeChatProvider>,

    /// GPU llamacpp embedding provider.
    ///
    /// Uses a llamacpp GGUF model (e.g. `embeddinggemma-300M-GGUF`) running on
    /// the GPU via ROCm/Vulkan.  Does **not** require the `GpuResourceManager`
    /// lock — Lemonade Server routes embedding calls separately from STT.
    ///
    /// `None` when no llamacpp embedding model was found in the registry or
    /// explicitly provided.
    pub embedding: Option<Arc<dyn EmbeddingProvider>>,
}

impl GpuDevice {
    /// Construct a `GpuDevice` from an already-fetched [`LemonadeModelRegistry`].
    ///
    /// A fresh [`GpuResourceManager`] must be supplied by the caller so that it
    /// can be shared with other components that also access the GPU (e.g. a
    /// [`LemonadeStack`](crate::lemonade::LemonadeStack)).
    ///
    /// Capabilities are derived automatically from which models the registry
    /// contains:
    /// - If an STT model is found → [`Transcription`] is advertised.
    /// - If an LLM model is found → [`TextGeneration`] is advertised.
    /// - If a llamacpp embedding model is found → [`Embedding`] is advertised.
    ///
    /// [`Transcription`]: DeviceCapability::Transcription
    /// [`TextGeneration`]: DeviceCapability::TextGeneration
    /// [`Embedding`]: DeviceCapability::Embedding
    /// Like [`from_registry`](Self::from_registry) but loads the embedding model
    /// with parameters sourced from `config` (`u-forge.toml` `[models.load_params]`).
    ///
    /// Calls `POST /api/v1/load` with per-model `ctx_size`, `batch_size`, and
    /// `ubatch_size` before connecting, so the server uses the correct context
    /// window and batch tuning for the embedding workload.
    pub async fn from_registry_with_config(
        registry: &LemonadeModelRegistry,
        gpu: Arc<GpuResourceManager>,
        config: &ModelConfig,
    ) -> Self {
        let mut capabilities = Vec::new();

        let stt = LemonadeSttProvider::from_registry(registry, Arc::clone(&gpu))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::Transcription);
                info!(model = %p.model, "GpuDevice: STT provider ready");
            });

        let chat = LemonadeChatProvider::from_registry(registry, Some(Arc::clone(&gpu)))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::TextGeneration);
                info!(model = %p.model, "GpuDevice: Chat/LLM provider ready");
            });

        let load_opts_for_embed = registry
            .llamacpp_embedding_model()
            .map(|m| config.load_options_for(&m.id));
        let embedding = init_llamacpp_embedding(
            registry,
            load_opts_for_embed.as_ref(),
            &mut capabilities,
            "GpuDevice",
        )
        .await;

        if capabilities.is_empty() {
            tracing::warn!(
                "GpuDevice::from_registry_with_config: no STT, LLM, or embedding models \
                 found — device will advertise no capabilities"
            );
        }

        Self {
            name: "AMD GPU (ROCm)".to_string(),
            backend: HardwareBackend::GpuRocm,
            capabilities,
            gpu,
            stt,
            chat,
            embedding,
        }
    }

    pub async fn from_registry(
        registry: &LemonadeModelRegistry,
        gpu: Arc<GpuResourceManager>,
    ) -> Self {
        let mut capabilities = Vec::new();

        let stt = LemonadeSttProvider::from_registry(registry, Arc::clone(&gpu))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::Transcription);
                info!(model = %p.model, "GpuDevice: STT provider ready");
            });

        let chat = LemonadeChatProvider::from_registry(registry, Some(Arc::clone(&gpu)))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::TextGeneration);
                info!(model = %p.model, "GpuDevice: Chat/LLM provider ready");
            });

        // Embedding via llamacpp (does not need the GpuResourceManager lock).
        let embedding =
            init_llamacpp_embedding(registry, None, &mut capabilities, "GpuDevice").await;

        if capabilities.is_empty() {
            tracing::warn!(
                "GpuDevice::from_registry: no STT, LLM, or embedding models found — \
                 device will advertise no capabilities"
            );
        }

        Self {
            name: "AMD GPU (ROCm)".to_string(),
            backend: HardwareBackend::GpuRocm,
            capabilities,
            gpu,
            stt,
            chat,
            embedding,
        }
    }

    /// Construct a `GpuDevice` with explicit model ids.
    ///
    /// Pass `None` for either model to skip that provider.  At least one
    /// model should be `Some` for the device to advertise any capabilities.
    ///
    /// Embedding is not set by `new()`; use the async
    /// [`with_embedding`](Self::with_embedding) builder method to add it.
    ///
    /// # Arguments
    ///
    /// * `base_url`   — Lemonade Server API root (e.g. `"http://localhost:13305/api/v1"`).
    /// * `stt_model`  — STT model id (e.g. `"Whisper-Large-v3-Turbo"`).  `None`
    ///   to disable the STT provider.
    /// * `chat_model` — LLM model id (e.g. `"GLM-4.7-Flash-GGUF"`).  `None` to
    ///   disable the chat provider.
    /// * `gpu`        — Shared GPU resource manager.
    pub fn new(
        base_url: &str,
        stt_model: Option<&str>,
        chat_model: Option<&str>,
        gpu: Arc<GpuResourceManager>,
    ) -> Self {
        let mut capabilities = Vec::new();

        let stt = stt_model.map(|model| {
            capabilities.push(DeviceCapability::Transcription);
            info!(model, "GpuDevice: STT provider configured");
            LemonadeSttProvider::new(base_url, model, Arc::clone(&gpu))
        });

        let chat = chat_model.map(|model| {
            capabilities.push(DeviceCapability::TextGeneration);
            info!(model, "GpuDevice: Chat/LLM provider configured");
            LemonadeChatProvider::new(base_url, model, Some(Arc::clone(&gpu)))
        });

        Self {
            name: "AMD GPU (ROCm)".to_string(),
            backend: HardwareBackend::GpuRocm,
            capabilities,
            gpu,
            stt,
            chat,
            embedding: None,
        }
    }

    /// Add a llamacpp embedding provider to this device (async builder).
    ///
    /// Probes the model dimensions once; if the model is not reachable the
    /// device is returned unchanged (no capability added, no error propagated).
    ///
    /// Designed for builder-style chaining:
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use u_forge_core::hardware::gpu::GpuDevice;
    /// # use u_forge_core::lemonade::GpuResourceManager;
    /// # async fn run() {
    /// let gpu = GpuResourceManager::new();
    /// let device = GpuDevice::stt_only("http://localhost:13305/api/v1", "Whisper-Large-v3-Turbo", gpu)
    ///     .with_embedding("http://localhost:13305/api/v1", "user.ggml-org/embeddinggemma-300M-GGUF")
    ///     .await;
    /// # }
    /// ```
    pub async fn with_embedding(mut self, base_url: &str, model_id: &str) -> Self {
        match LemonadeProvider::new(base_url, model_id).await {
            Ok(p) => {
                self.capabilities.push(DeviceCapability::Embedding);
                info!(model = %model_id, "GpuDevice: embedding provider added");
                self.embedding = Some(Arc::new(p) as Arc<dyn EmbeddingProvider>);
            }
            Err(e) => {
                tracing::warn!(
                    model = %model_id,
                    error = %e,
                    "GpuDevice::with_embedding: model not reachable — skipping"
                );
            }
        }
        self
    }

    /// Construct a `GpuDevice` with **STT only** (no LLM provider).
    ///
    /// This is a convenience wrapper around [`GpuDevice::new`] for setups where
    /// LLM inference is handled elsewhere or not needed.
    pub fn stt_only(base_url: &str, stt_model: &str, gpu: Arc<GpuResourceManager>) -> Self {
        Self::new(base_url, Some(stt_model), None, gpu)
    }

    /// Construct a `GpuDevice` with **LLM only** (no STT provider).
    ///
    /// Useful when STT is routed to the NPU and only LLM inference should use
    /// the GPU.
    pub fn llm_only(base_url: &str, chat_model: &str, gpu: Arc<GpuResourceManager>) -> Self {
        Self::new(base_url, None, Some(chat_model), gpu)
    }

    /// Returns `true` if this device has an active STT provider.
    pub fn has_stt(&self) -> bool {
        self.stt.is_some()
    }

    /// Returns `true` if this device has an active LLM/chat provider.
    pub fn has_chat(&self) -> bool {
        self.chat.is_some()
    }

    /// Returns `true` if this device has an active llamacpp embedding provider.
    pub fn has_embedding(&self) -> bool {
        self.embedding.is_some()
    }

    /// Returns the current GPU workload state as a human-readable string.
    ///
    /// Delegates to [`GpuResourceManager::current_workload`].
    pub fn gpu_workload_summary(&self) -> String {
        self.gpu.current_workload().to_string()
    }
}

impl DeviceWorker for GpuDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn backend(&self) -> HardwareBackend {
        self.backend.clone()
    }

    fn capabilities(&self) -> &[DeviceCapability] {
        &self.capabilities
    }
}

impl std::fmt::Debug for GpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDevice")
            .field("name", &self.name)
            .field("backend", &self.backend)
            .field("capabilities", &self.capabilities)
            .field("stt_model", &self.stt.as_ref().map(|p| p.model.as_str()))
            .field("chat_model", &self.chat.as_ref().map(|p| p.model.as_str()))
            .field("has_embedding", &self.embedding.is_some())
            .finish()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{DeviceCapability, DeviceWorker, HardwareBackend};

    use crate::test_helpers::require_integration_url;

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_new_no_models_has_no_capabilities() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new("http://localhost:13305/api/v1", None, None, gpu);
        assert!(
            device.capabilities().is_empty(),
            "No models → no capabilities"
        );
        assert!(!device.has_stt());
        assert!(!device.has_chat());
    }

    #[test]
    fn test_new_stt_only_capabilities() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::stt_only(
            "http://localhost:13305/api/v1",
            "Whisper-Large-v3-Turbo",
            gpu,
        );
        assert!(
            device.supports(&DeviceCapability::Transcription),
            "STT-only must advertise Transcription"
        );
        assert!(
            !device.supports(&DeviceCapability::TextGeneration),
            "STT-only must NOT advertise TextGeneration"
        );
        assert!(device.has_stt());
        assert!(!device.has_chat());
    }

    #[test]
    fn test_new_llm_only_capabilities() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::llm_only("http://localhost:13305/api/v1", "GLM-4.7-Flash-GGUF", gpu);
        assert!(
            device.supports(&DeviceCapability::TextGeneration),
            "LLM-only must advertise TextGeneration"
        );
        assert!(
            !device.supports(&DeviceCapability::Transcription),
            "LLM-only must NOT advertise Transcription"
        );
        assert!(!device.has_stt());
        assert!(device.has_chat());
    }

    #[test]
    fn test_new_both_models_has_both_capabilities() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new(
            "http://localhost:13305/api/v1",
            Some("Whisper-Large-v3-Turbo"),
            Some("GLM-4.7-Flash-GGUF"),
            gpu,
        );
        assert!(device.supports(&DeviceCapability::Transcription));
        assert!(device.supports(&DeviceCapability::TextGeneration));
        assert!(!device.supports(&DeviceCapability::Embedding));
        assert!(!device.supports(&DeviceCapability::TextToSpeech));
        assert!(device.has_stt());
        assert!(device.has_chat());
    }

    #[test]
    fn test_device_worker_backend_is_rocm() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::llm_only("http://localhost:13305/api/v1", "model", gpu);
        assert_eq!(
            device.backend(),
            HardwareBackend::GpuRocm,
            "GPU device backend should be GpuRocm"
        );
    }

    #[test]
    fn test_device_worker_name_is_nonempty() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::llm_only("http://localhost:13305/api/v1", "model", gpu);
        assert!(!device.name().is_empty(), "Device name should not be empty");
    }

    #[test]
    fn test_shared_gpu_manager_same_arc() {
        // Both stt and chat should share the exact same GpuResourceManager.
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new(
            "http://localhost:13305/api/v1",
            Some("Whisper-Large-v3-Turbo"),
            Some("GLM-4.7-Flash-GGUF"),
            Arc::clone(&gpu),
        );

        // The Arc in the device must point to the same allocation as `gpu`.
        assert!(
            Arc::ptr_eq(&device.gpu, &gpu),
            "GpuDevice.gpu must be the same Arc as the one passed at construction"
        );

        // The STT and chat providers must also share it.
        if let Some(stt) = &device.stt {
            assert!(
                Arc::ptr_eq(&stt.gpu, &gpu),
                "STT provider's GpuResourceManager must be shared with GpuDevice.gpu"
            );
        }
        if let Some(chat) = &device.chat {
            assert!(
                chat.gpu
                    .as_ref()
                    .map(|g| Arc::ptr_eq(g, &gpu))
                    .unwrap_or(false),
                "Chat provider's GpuResourceManager must be shared with GpuDevice.gpu"
            );
        }
    }

    #[test]
    fn test_gpu_workload_summary_is_idle_initially() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new("http://localhost:13305/api/v1", None, None, gpu);
        let summary = device.gpu_workload_summary();
        // GpuWorkload::Idle displays as "Idle"
        assert!(
            summary.to_lowercase().contains("idle"),
            "Initial workload should be Idle, got: {summary}"
        );
    }

    #[test]
    fn test_debug_format_includes_key_fields() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new(
            "http://localhost:13305/api/v1",
            Some("Whisper-Large-v3-Turbo"),
            Some("GLM-4.7-Flash-GGUF"),
            gpu,
        );
        let debug = format!("{:?}", device);
        assert!(
            debug.contains("GpuDevice"),
            "Debug must include struct name"
        );
        assert!(
            debug.contains("Whisper-Large-v3-Turbo"),
            "Debug must include STT model"
        );
        assert!(
            debug.contains("GLM-4.7-Flash-GGUF"),
            "Debug must include chat model"
        );
    }

    #[test]
    fn test_summary_contains_backend_and_capabilities() {
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new(
            "http://localhost:13305/api/v1",
            Some("Whisper-Large-v3-Turbo"),
            Some("GLM-4.7-Flash-GGUF"),
            gpu,
        );
        let s = device.summary();
        assert!(s.contains("ROCm"), "summary should mention ROCm: {s}");
        assert!(
            s.contains("Transcription"),
            "summary should mention Transcription: {s}"
        );
        assert!(
            s.contains("TextGeneration"),
            "summary should mention TextGeneration: {s}"
        );
    }

    #[tokio::test]
    async fn test_stt_blocked_when_llm_active() {
        // Verify the GPU resource manager policy: STT must fail immediately
        // while LLM inference holds the GPU.  No server needed — pure in-process.
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::new(
            "http://localhost:13305/api/v1",
            Some("Whisper-Large-v3-Turbo"),
            Some("GLM-4.7-Flash-GGUF"),
            Arc::clone(&gpu),
        );

        // Simulate LLM holding the GPU (gpu is already an Arc).
        let _llm_guard = gpu.begin_llm().await;

        // STT should be rejected immediately (not queue).
        let stt_result = gpu.begin_stt();
        assert!(
            stt_result.is_err(),
            "STT must fail immediately when LLM is active"
        );

        // GpuDevice reflects the same state.
        let workload = device.gpu_workload_summary();
        assert!(
            workload.to_lowercase().contains("llm"),
            "Workload summary should show LLM active: {workload}"
        );
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_from_registry_stt_transcribes() {
        let url = require_integration_url!();

        let registry = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let device = GpuDevice::from_registry(&registry, gpu).await;

        let Some(stt) = &device.stt else {
            eprintln!("Skipping: no STT model in registry");
            return;
        };

        // Build 1 s of silence WAV inline (avoids depending on transcription module).
        let wav = {
            let sample_rate: u32 = 16_000;
            let num_channels: u16 = 1;
            let bits_per_sample: u16 = 16;
            let num_samples: u32 = sample_rate;
            let data_size = num_samples * (bits_per_sample as u32 / 8) * num_channels as u32;
            let riff_size: u32 = 4 + 8 + 16 + 8 + data_size;
            let mut buf = Vec::with_capacity((8 + riff_size) as usize);
            buf.extend_from_slice(b"RIFF");
            buf.extend_from_slice(&riff_size.to_le_bytes());
            buf.extend_from_slice(b"WAVE");
            buf.extend_from_slice(b"fmt ");
            buf.extend_from_slice(&16u32.to_le_bytes());
            buf.extend_from_slice(&1u16.to_le_bytes());
            buf.extend_from_slice(&num_channels.to_le_bytes());
            buf.extend_from_slice(&sample_rate.to_le_bytes());
            let byte_rate = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
            buf.extend_from_slice(&byte_rate.to_le_bytes());
            let block_align = num_channels * bits_per_sample / 8;
            buf.extend_from_slice(&block_align.to_le_bytes());
            buf.extend_from_slice(&bits_per_sample.to_le_bytes());
            buf.extend_from_slice(b"data");
            buf.extend_from_slice(&data_size.to_le_bytes());
            buf.extend(std::iter::repeat(0u8).take(data_size as usize));
            buf
        };

        let result = stt.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "GPU STT transcription failed: {:?}",
            result.err()
        );
    }
}
