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
//!     "http://localhost:8000/api/v1",
//!     None, // embed-gemma-300m-FLM
//!     None, // whisper-v3-turbo-FLM
//!     None, // qwen3-8b-FLM (pass Some("model-id") to override, or None to disable)
//! ).await?;
//!
//! let vec  = npu.embedding.embed("Hello, world!").await?;
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
use tracing::info;

use crate::ai::embeddings::{EmbeddingProvider, LemonadeProvider};
use crate::lemonade::{LemonadeChatProvider, LemonadeModelRegistry};
use crate::ai::transcription::{LemonadeTranscriptionProvider, TranscriptionProvider};

use super::{DeviceCapability, DeviceWorker, HardwareBackend};

// ── Default model identifiers ─────────────────────────────────────────────────

/// Default NPU embedding model served by Lemonade (AMD FLM).
pub const DEFAULT_NPU_EMBEDDING_MODEL: &str = "embed-gemma-300m-FLM";

/// Default NPU speech-to-text model served by Lemonade (AMD FLM whisper).
pub const DEFAULT_NPU_STT_MODEL: &str = "whisper-v3-turbo-FLM";

/// Default NPU LLM model served by Lemonade (AMD FLM — lightweight for low latency).
pub const DEFAULT_NPU_LLM_MODEL: &str = "qwen3-8b-FLM";

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
    /// Always present — construction fails if the embedding model is
    /// unreachable.
    pub embedding: Arc<dyn EmbeddingProvider>,

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
    /// * `base_url`        — Lemonade Server API root (e.g. `"http://localhost:8000/api/v1"`).
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
        let emb_model = embedding_model.unwrap_or(DEFAULT_NPU_EMBEDDING_MODEL);
        let stt_model_id = stt_model.unwrap_or(DEFAULT_NPU_STT_MODEL);

        let embedding = Arc::new(LemonadeProvider::new(base_url, emb_model).await?)
            as Arc<dyn EmbeddingProvider>;

        let transcription: Option<Arc<dyn TranscriptionProvider>> = Some(Arc::new(
            LemonadeTranscriptionProvider::new(base_url, stt_model_id),
        ));

        let mut capabilities = vec![DeviceCapability::Embedding, DeviceCapability::Transcription];

        let chat = llm_model.map(|m| {
            let model_id = if m.is_empty() {
                DEFAULT_NPU_LLM_MODEL
            } else {
                m
            };
            capabilities.push(DeviceCapability::TextGeneration);
            info!(model = model_id, "NpuDevice: LLM provider configured");
            LemonadeChatProvider::new_npu(base_url, model_id)
        });

        info!(
            embedding_model = emb_model,
            stt_model = stt_model_id,
            llm = chat
                .as_ref()
                .map(|c| c.model.as_str())
                .unwrap_or("disabled"),
            "NpuDevice initialised"
        );

        Ok(Self {
            name: "AMD NPU (FLM)".to_string(),
            backend: HardwareBackend::Npu,
            capabilities,
            embedding,
            transcription,
            chat,
        })
    }

    /// Construct an `NpuDevice` from an already-fetched [`LemonadeModelRegistry`].
    ///
    /// Capabilities are derived automatically from which FLM models the registry
    /// contains:
    /// - If a [`NpuEmbedding`](crate::lemonade::ModelRole::NpuEmbedding) model exists
    ///   → [`Embedding`](DeviceCapability::Embedding) is advertised.
    /// - If a [`NpuStt`](crate::lemonade::ModelRole::NpuStt) model exists
    ///   → [`Transcription`](DeviceCapability::Transcription) is advertised.
    /// - If a [`NpuLlm`](crate::lemonade::ModelRole::NpuLlm) model exists
    ///   → [`TextGeneration`](DeviceCapability::TextGeneration) is advertised.
    ///
    /// # Errors
    ///
    /// Returns an error if no embedding model is found **and** Lemonade Server
    /// cannot be probed for dimension information.  If the registry contains no
    /// embedding model at all, an error is returned immediately.
    pub async fn from_registry(registry: &LemonadeModelRegistry) -> Result<Self> {
        use crate::lemonade::ModelRole;

        let emb_entry = registry.npu_embedding_model().ok_or_else(|| {
            anyhow::anyhow!("No NPU embedding model found in the Lemonade registry")
        })?;

        let embedding = Arc::new(LemonadeProvider::new(&registry.base_url, &emb_entry.id).await?)
            as Arc<dyn EmbeddingProvider>;

        let mut capabilities = vec![DeviceCapability::Embedding];

        let transcription: Option<Arc<dyn TranscriptionProvider>> =
            registry.npu_stt_model().map(|m| {
                capabilities.push(DeviceCapability::Transcription);
                info!(model = %m.id, "NpuDevice: STT provider ready");
                Arc::new(LemonadeTranscriptionProvider::new(
                    &registry.base_url,
                    &m.id,
                )) as Arc<dyn TranscriptionProvider>
            });

        let chat = registry.npu_llm_model().map(|m| {
            capabilities.push(DeviceCapability::TextGeneration);
            info!(model = %m.id, "NpuDevice: LLM provider ready");
            LemonadeChatProvider::new_npu(&registry.base_url, &m.id)
        });

        if capabilities.is_empty() {
            tracing::warn!(
                "NpuDevice::from_registry: no capabilities found — \
                 device will advertise nothing"
            );
        }

        info!(
            embedding = %emb_entry.id,
            stt = transcription.is_some(),
            llm = chat.as_ref().map(|c| c.model.as_str()).unwrap_or("none"),
            "NpuDevice from registry"
        );

        Ok(Self {
            name: "AMD NPU (FLM)".to_string(),
            backend: HardwareBackend::Npu,
            capabilities,
            embedding,
            transcription,
            chat,
        })
    }

    /// Construct an `NpuDevice` with **embedding only** (no STT or LLM providers).
    ///
    /// Use this when the NPU whisper and LLM models are not available or not desired.
    ///
    /// # Arguments
    ///
    /// * `base_url` — Lemonade Server API root.
    /// * `model` — FLM embedding model id.  Defaults to
    ///   [`DEFAULT_NPU_EMBEDDING_MODEL`] when `None`.
    pub async fn embedding_only(base_url: &str, model: Option<&str>) -> Result<Self> {
        let emb_model = model.unwrap_or(DEFAULT_NPU_EMBEDDING_MODEL);

        let embedding = Arc::new(LemonadeProvider::new(base_url, emb_model).await?)
            as Arc<dyn EmbeddingProvider>;

        info!(model = emb_model, "NpuDevice initialised (embedding only)");

        Ok(Self {
            name: "AMD NPU Embedding".to_string(),
            backend: HardwareBackend::Npu,
            capabilities: vec![DeviceCapability::Embedding],
            embedding,
            transcription: None,
            chat: None,
        })
    }

    /// Construct an `NpuDevice` with **LLM only** (no embedding or STT providers).
    ///
    /// Because the LLM provider performs no probe request, this constructor is
    /// **synchronous**.
    ///
    /// # Arguments
    ///
    /// * `base_url` — Lemonade Server API root.
    /// * `model`    — FLM LLM model id.  Defaults to [`DEFAULT_NPU_LLM_MODEL`]
    ///   when `None`.
    pub fn llm_only(base_url: &str, model: Option<&str>) -> Self {
        let llm_model = model.unwrap_or(DEFAULT_NPU_LLM_MODEL);

        info!(model = llm_model, "NpuDevice initialised (LLM only)");

        Self {
            name: "AMD NPU LLM".to_string(),
            backend: HardwareBackend::Npu,
            capabilities: vec![DeviceCapability::TextGeneration],
            embedding: Arc::new(DisabledEmbeddingProvider),
            transcription: None,
            chat: Some(LemonadeChatProvider::new_npu(base_url, llm_model)),
        }
    }

    /// Construct an `NpuDevice` with **transcription only** (no embedding or LLM providers).
    ///
    /// Useful when embedding is handled by a different device and only the NPU
    /// whisper model is desired.
    ///
    /// Unlike [`NpuDevice::new`], this constructor is **synchronous** because
    /// the transcription provider performs no probe request.
    ///
    /// # Arguments
    ///
    /// * `base_url` — Lemonade Server API root.
    /// * `model` — FLM whisper model id.  Defaults to [`DEFAULT_NPU_STT_MODEL`]
    ///   when `None`.
    pub fn transcription_only(base_url: &str, model: Option<&str>) -> Self {
        let stt_model = model.unwrap_or(DEFAULT_NPU_STT_MODEL);

        // We need a dummy embedding provider to satisfy the field type.
        // Use a no-op placeholder so the type system is happy while keeping
        // construction cheap.
        let transcription: Arc<dyn TranscriptionProvider> =
            Arc::new(LemonadeTranscriptionProvider::new(base_url, stt_model));

        info!(
            model = stt_model,
            "NpuDevice initialised (transcription only)"
        );

        Self {
            name: "AMD NPU STT".to_string(),
            backend: HardwareBackend::Npu,
            capabilities: vec![DeviceCapability::Transcription],
            // Use a disconnected placeholder — will error if embed() is called.
            embedding: Arc::new(DisabledEmbeddingProvider),
            transcription: Some(transcription),
            chat: None,
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
            .field("embedding_dims", &self.embedding.dimensions().ok())
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

// ── DisabledEmbeddingProvider ─────────────────────────────────────────────────

/// Internal placeholder used when `NpuDevice` is built transcription-only.
///
/// All methods return an error — this provider must never be registered in a
/// queue's embedding pool.  Its sole purpose is to satisfy the `Arc<dyn
/// EmbeddingProvider>` field without allocating a real HTTP client.
struct DisabledEmbeddingProvider;

#[async_trait::async_trait]
impl crate::ai::embeddings::EmbeddingProvider for DisabledEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(anyhow::anyhow!(
            "DisabledEmbeddingProvider: this NpuDevice was built with transcription_only() \
             and does not support embedding"
        ))
    }

    async fn embed_batch(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        Err(anyhow::anyhow!(
            "DisabledEmbeddingProvider: this NpuDevice was built with transcription_only() \
             and does not support embedding"
        ))
    }

    fn dimensions(&self) -> Result<usize> {
        Err(anyhow::anyhow!("DisabledEmbeddingProvider: no dimensions"))
    }

    fn max_tokens(&self) -> Result<usize> {
        Err(anyhow::anyhow!("DisabledEmbeddingProvider: no max_tokens"))
    }

    fn provider_type(&self) -> crate::ai::embeddings::EmbeddingProviderType {
        crate::ai::embeddings::EmbeddingProviderType::Lemonade
    }

    fn model_info(&self) -> Option<crate::ai::embeddings::EmbeddingModelInfo> {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{DeviceCapability, DeviceWorker};

    use crate::test_helpers::lemonade_url;

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_transcription_only_capabilities() {
        let device = NpuDevice::transcription_only("http://localhost:8000/api/v1", None);
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
        let device = NpuDevice::transcription_only("http://localhost:8000/api/v1", None);
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
            NpuDevice::transcription_only("http://localhost:8000/api/v1", Some("my-whisper-FLM"));
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
        let device = NpuDevice::transcription_only("http://localhost:8000/api/v1", None);
        assert!(!device.name().is_empty(), "name should not be empty");
        assert_eq!(device.backend(), HardwareBackend::Npu);
    }

    #[test]
    fn test_disabled_embedding_provider_errors() {
        let provider = DisabledEmbeddingProvider;
        assert!(provider.dimensions().is_err());
        assert!(provider.max_tokens().is_err());
        assert!(provider.model_info().is_none());
    }

    #[tokio::test]
    async fn test_disabled_embedding_provider_embed_errors() {
        let provider = DisabledEmbeddingProvider;
        let result = provider.embed("hello").await;
        assert!(
            result.is_err(),
            "Expected error from DisabledEmbeddingProvider"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("transcription_only"),
            "Error should mention transcription_only: {msg}"
        );
    }

    #[test]
    fn test_summary_format() {
        let device = NpuDevice::transcription_only("http://localhost:8000/api/v1", None);
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
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };
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
        let dims = device.embedding.dimensions().unwrap();
        assert!(
            dims > 0,
            "Expected positive embedding dimensions, got {dims}"
        );
    }

    #[tokio::test]
    async fn test_embedding_only_construction() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };
        let device = NpuDevice::embedding_only(&url, None).await;
        assert!(
            device.is_ok(),
            "NpuDevice::embedding_only failed: {:?}",
            device.err()
        );
        let device = device.unwrap();
        assert!(device.supports(&DeviceCapability::Embedding));
        assert!(
            !device.supports(&DeviceCapability::Transcription),
            "embedding_only must NOT advertise Transcription"
        );
        assert!(device.transcription.is_none());
    }

    #[tokio::test]
    async fn test_embed_via_npu_device() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };
        let device = NpuDevice::embedding_only(&url, None)
            .await
            .expect("NpuDevice construction failed");
        let embedding = device.embedding.embed("The quick brown fox").await;
        assert!(embedding.is_ok(), "embed() failed: {:?}", embedding.err());
        let embedding = embedding.unwrap();
        assert!(!embedding.is_empty(), "Expected non-empty embedding vector");
        assert!(
            embedding.iter().all(|&x| x.is_finite()),
            "Embedding contains non-finite values"
        );
    }
}
