//! NPU (Neural Processing Unit) device implementation.
//!
//! The AMD NPU runs quantised FLM (Fast Language Model) models via Lemonade
//! Server.  Because the NPU is dedicated silicon — physically separate from the
//! GPU — it can service embedding, transcription, **and LLM inference** without
//! any GPU resource-contention logic.
//!
//! # Supported capabilities
//!
//! | Capability        | Default model              |
//! |-------------------|----------------------------|
//! | [`Embedding`]     | `embed-gemma-300m-FLM`     |
//! | [`Transcription`] | `whisper-v3-turbo-FLM`     |
//! | [`TextGeneration`]| `qwen3-8b-FLM`             |
//!
//! # Usage
//!
//! ```no_run
//! # use u_forge_core::hardware::npu::NpuDevice;
//! # async fn run() -> anyhow::Result<()> {
//! // All three capabilities on the NPU
//! let npu = NpuDevice::new(
//!     "http://localhost:13305/api/v1",
//!     None, // embed-gemma-300m-FLM
//!     None, // whisper-v3-turbo-FLM
//!     None, // qwen3-8b-FLM (pass Some("model-id") to override, or None to disable)
//! ).await?;
//!
//! let vec  = npu.embedding.as_ref().unwrap().embed("Hello, world!").await?;
//! let audio_bytes: Vec<u8> = std::fs::read("session.wav")?;
//! let text = npu.transcription
//!     .as_ref()
//!     .unwrap()
//!     .transcribe(audio_bytes, "session.wav")
//!     .await?;
//! if let Some(chat) = &npu.chat {
//!     let answer = chat.ask("What is a tarrasque?").await?;
//!     println!("{answer}");
//! }
//! # Ok(()) }
//! ```
//!
//! [`Embedding`]: crate::hardware::DeviceCapability::Embedding
//! [`Transcription`]: crate::hardware::DeviceCapability::Transcription
//! [`TextGeneration`]: crate::hardware::DeviceCapability::TextGeneration

use std::sync::Arc;

use anyhow::Result;

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use crate::config::ModelConfig;
use crate::lemonade::{LemonadeChatProvider, LemonadeModelRegistry, ModelLoadOptions};

use super::{DeviceCapability, DeviceWorker, HardwareBackend};

// ── Default model identifiers ─────────────────────────────────────────────────

/// Default NPU embedding model served by Lemonade (AMD FLM).
pub const DEFAULT_NPU_EMBEDDING_MODEL: &str = "embed-gemma-300m-FLM";

/// Default NPU speech-to-text model served by Lemonade (AMD FLM whisper).
pub const DEFAULT_NPU_STT_MODEL: &str = "whisper-v3-turbo-FLM";

/// Default NPU LLM model served by Lemonade (AMD FLM — lightweight for low latency).
pub const DEFAULT_NPU_LLM_MODEL: &str = "qwen3.5-9b-FLM";

// ── NpuDevice ─────────────────────────────────────────────────────────────────

/// Logical device representing the AMD NPU running Lemonade FLM models.
///
/// Holds an [`EmbeddingProvider`], an optional [`TranscriptionProvider`], and
/// an optional [`LemonadeChatProvider`], all routed to the NPU via Lemonade
/// Server.  Because the NPU is dedicated silicon it does **not** use a
/// [`GpuResourceManager`](crate::lemonade::GpuResourceManager) — requests are
/// sent directly and Lemonade handles NPU scheduling internally.
///
/// # Concurrency
///
/// Multiple embedding, transcription, and LLM calls may be in flight
/// simultaneously.  Lemonade Server serialises access to the physical NPU if
/// needed; from the Rust side, all providers are fully `Send + Sync` and safe
/// to share across tasks via `Arc`.
pub struct NpuDevice {
    pub name: String,
    pub backend: HardwareBackend,
    capabilities: Vec<DeviceCapability>,

    /// Embedding provider backed by the NPU.
    ///
    /// `None` when the device was constructed without an embedding model
    /// (e.g. [`NpuDevice::llm_only`] or [`NpuDevice::transcription_only`]).
    pub embedding: Option<Arc<dyn EmbeddingProvider>>,

    /// Transcription (STT) provider backed by the NPU.
    ///
    /// `None` when the device was constructed with
    /// [`NpuDevice::embedding_only`] or [`NpuDevice::llm_only`], or when the
    /// STT model is not available.
    pub transcription: Option<Arc<dyn TranscriptionProvider>>,

    /// LLM / chat-completion provider backed by the NPU.
    ///
    /// `None` when the device was constructed without an LLM model
    /// (e.g. [`NpuDevice::embedding_only`] or [`NpuDevice::transcription_only`]).
    /// No [`GpuResourceManager`](crate::lemonade::GpuResourceManager) is needed —
    /// the NPU runs independently of the GPU.
    pub chat: Option<LemonadeChatProvider>,
}

impl NpuDevice {
    /// Construct an `NpuDevice` with embedding, transcription, and optionally LLM.
    ///
    /// # Arguments
    ///
    /// * `base_url`        — Lemonade Server API root (e.g. `"http://localhost:13305/api/v1"`).
    /// * `embedding_model` — FLM embedding model id.  Defaults to
    ///   [`DEFAULT_NPU_EMBEDDING_MODEL`] when `None`.
    /// * `stt_model`       — FLM whisper model id.  Defaults to
    ///   [`DEFAULT_NPU_STT_MODEL`] when `None`.
    /// * `llm_model`       — FLM LLM model id.  Defaults to
    ///   [`DEFAULT_NPU_LLM_MODEL`] when `Some("")` is passed; pass `None`
    ///   to disable the LLM provider entirely.
    ///
    /// # Errors
    ///
    /// Returns an error if Lemonade Server is unreachable or the embedding model
    /// cannot be probed (a single dummy request is sent to discover the vector
    /// dimensionality).  STT and LLM providers are constructed cheaply with no
    /// probe request and do not contribute to construction errors.
    pub async fn new(
        base_url: &str,
        embedding_model: Option<&str>,
        stt_model: Option<&str>,
        llm_model: Option<&str>,
    ) -> Result<Self> {
        Self::new_with_load(base_url, embedding_model, stt_model, llm_model, None).await
    }

    /// Like [`new`](Self::new) but explicitly loads the embedding model first.
    pub async fn new_with_load(
        base_url: &str,
        embedding_model: Option<&str>,
        stt_model: Option<&str>,
        llm_model: Option<&str>,
        load_opts: Option<&ModelLoadOptions>,
    ) -> Result<Self> {
        crate::lemonade::device_factory::npu_from_url_with_load(
            base_url, embedding_model, stt_model, llm_model, load_opts,
        )
        .await
    }

    /// Construct from an already-fetched registry.
    pub async fn from_registry(registry: &LemonadeModelRegistry) -> Result<Self> {
        crate::lemonade::device_factory::npu_from_registry_with_load(registry, None).await
    }

    /// Like [`from_registry`](Self::from_registry) with per-model load params.
    pub async fn from_registry_with_config(
        registry: &LemonadeModelRegistry,
        config: &ModelConfig,
    ) -> Result<Self> {
        crate::lemonade::device_factory::npu_from_registry_with_config(registry, config).await
    }

    /// Like [`from_registry`](Self::from_registry) with explicit load options.
    pub async fn from_registry_with_load(
        registry: &LemonadeModelRegistry,
        load_opts: Option<&ModelLoadOptions>,
    ) -> Result<Self> {
        crate::lemonade::device_factory::npu_from_registry_with_load(registry, load_opts).await
    }

    /// Construct with **embedding only** (no STT or LLM).
    pub async fn embedding_only(
        base_url: &str,
        model: Option<&str>,
        load_opts: Option<&ModelLoadOptions>,
    ) -> Result<Self> {
        crate::lemonade::device_factory::npu_embedding_only(base_url, model, load_opts).await
    }

    /// Construct with **LLM only** (no embedding or STT).
    pub fn llm_only(base_url: &str, model: Option<&str>) -> Self {
        crate::lemonade::device_factory::npu_llm_only(base_url, model)
    }

    /// Construct with **transcription only** (no embedding or LLM).
    pub fn transcription_only(base_url: &str, model: Option<&str>) -> Self {
        crate::lemonade::device_factory::npu_transcription_only(base_url, model)
    }

    /// Low-level constructor: assemble an `NpuDevice` from already-resolved
    /// providers.  Used by [`crate::lemonade::device_factory`].
    pub(crate) fn from_parts(
        name: String,
        capabilities: Vec<DeviceCapability>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        transcription: Option<Arc<dyn TranscriptionProvider>>,
        chat: Option<LemonadeChatProvider>,
    ) -> Self {
        Self {
            name,
            backend: HardwareBackend::Npu,
            capabilities,
            embedding,
            transcription,
            chat,
        }
    }

    /// Whether this device has an active embedding provider.
    pub fn has_embedding(&self) -> bool {
        self.capabilities.contains(&DeviceCapability::Embedding)
    }

    /// Whether this device has an active transcription provider.
    pub fn has_transcription(&self) -> bool {
        self.capabilities.contains(&DeviceCapability::Transcription)
    }

    /// Whether this device has an active LLM/chat provider.
    pub fn has_chat(&self) -> bool {
        self.chat.is_some()
    }
}

impl DeviceWorker for NpuDevice {
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

impl std::fmt::Debug for NpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NpuDevice")
            .field("name", &self.name)
            .field("backend", &self.backend)
            .field("capabilities", &self.capabilities)
            .field("embedding_dims", &self.embedding.as_ref().and_then(|p| p.dimensions().ok()))
            .field(
                "stt_model",
                &self
                    .transcription
                    .as_ref()
                    .map(|p| p.model_name().to_string()),
            )
            .field("chat_model", &self.chat.as_ref().map(|c| c.model.as_str()))
            .finish()
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{DeviceCapability, DeviceWorker};

    use crate::test_helpers::require_integration_url;

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_transcription_only_capabilities() {
        let device = NpuDevice::transcription_only("http://localhost:13305/api/v1", None);
        assert!(
            device.supports(&DeviceCapability::Transcription),
            "transcription_only must support Transcription"
        );
        assert!(
            !device.supports(&DeviceCapability::Embedding),
            "transcription_only must NOT support Embedding"
        );
        assert!(device.has_transcription());
        assert!(!device.has_embedding());
    }

    #[test]
    fn test_transcription_only_uses_default_model() {
        let device = NpuDevice::transcription_only("http://localhost:13305/api/v1", None);
        let model = device
            .transcription
            .as_ref()
            .unwrap()
            .model_name()
            .to_string();
        assert_eq!(
            model, DEFAULT_NPU_STT_MODEL,
            "Expected default STT model, got {model}"
        );
    }

    #[test]
    fn test_transcription_only_custom_model() {
        let device =
            NpuDevice::transcription_only("http://localhost:13305/api/v1", Some("my-whisper-FLM"));
        let model = device
            .transcription
            .as_ref()
            .unwrap()
            .model_name()
            .to_string();
        assert_eq!(model, "my-whisper-FLM");
    }

    #[test]
    fn test_device_worker_name_and_backend() {
        let device = NpuDevice::transcription_only("http://localhost:13305/api/v1", None);
        assert!(!device.name().is_empty(), "name should not be empty");
        assert_eq!(device.backend(), HardwareBackend::Npu);
    }

    #[test]
    fn test_transcription_only_has_no_embedding() {
        let device = NpuDevice::transcription_only("http://localhost:13305/api/v1", None);
        assert!(
            device.embedding.is_none(),
            "transcription_only must have no embedding provider"
        );
    }

    #[test]
    fn test_summary_format() {
        let device = NpuDevice::transcription_only("http://localhost:13305/api/v1", None);
        let summary = device.summary();
        assert!(
            summary.contains("NPU"),
            "summary should mention NPU: {summary}"
        );
        assert!(
            summary.contains("Transcription"),
            "summary should mention Transcription: {summary}"
        );
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_new_both_capabilities() {
        let url = require_integration_url!();
        let device = NpuDevice::new(&url, None, None, None).await;
        assert!(device.is_ok(), "NpuDevice::new failed: {:?}", device.err());
        let device = device.unwrap();
        assert!(device.supports(&DeviceCapability::Embedding));
        assert!(device.supports(&DeviceCapability::Transcription));
        assert!(
            !device.supports(&DeviceCapability::TextGeneration),
            "new(llm_model=None) must NOT advertise TextGeneration"
        );
        assert!(device.has_embedding());
        assert!(device.has_transcription());
        assert!(!device.has_chat());
        // Embedding dimensions should be non-zero
        let dims = device.embedding.as_ref().unwrap().dimensions().unwrap();
        assert!(
            dims > 0,
            "Expected positive embedding dimensions, got {dims}"
        );
    }

    #[tokio::test]
    async fn test_embed_via_npu_device() {
        let url = require_integration_url!();
        let device = NpuDevice::embedding_only(&url, None, None)
            .await
            .expect("NpuDevice construction failed");
        let embedding = device.embedding.as_ref().unwrap().embed("The quick brown fox").await;
        assert!(embedding.is_ok(), "embed() failed: {:?}", embedding.err());
        let embedding = embedding.unwrap();
        assert!(!embedding.is_empty(), "Expected non-empty embedding vector");
        assert!(
            embedding.iter().all(|&x| x.is_finite()),
            "Embedding contains non-finite values"
        );
    }
}
