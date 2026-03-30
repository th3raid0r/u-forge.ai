//! [`InferenceQueueBuilder`] — register device workers and spawn background tasks.

use std::sync::Arc;

use tracing::{debug, warn};

use crate::ai::embeddings::EmbeddingProvider;
use crate::hardware::cpu::CpuDevice;
use crate::hardware::gpu::GpuDevice;
use crate::hardware::npu::NpuDevice;
use crate::lemonade::LemonadeRerankProvider;

use super::dispatch::InferenceQueue;
use super::jobs::{EmbedJob, GenerateJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue};
use super::workers::{
    run_embed_worker, run_gpu_stt_worker, run_llm_worker, run_rerank_worker, run_transcribe_worker,
    run_tts_worker,
};

// ── InferenceQueueBuilder ─────────────────────────────────────────────────────

/// Builder for [`InferenceQueue`].
///
/// Register one or more device workers, then call [`build`] to spawn the
/// background Tokio tasks and get a queue handle.
///
/// # Example
///
/// ```no_run
/// # use u_forge_core::queue::InferenceQueueBuilder;
/// # async fn run() -> anyhow::Result<()> {
/// # let npu = todo!();
/// # let gpu = todo!();
/// # let cpu = todo!();
/// let queue = InferenceQueueBuilder::new()
///     .with_npu_device(npu)
///     .with_gpu_device(gpu)
///     .with_cpu_device(cpu)
///     .build();
/// # Ok(()) }
/// ```
///
/// [`build`]: InferenceQueueBuilder::build
pub struct InferenceQueueBuilder {
    pub(super) npu_devices: Vec<NpuDevice>,
    pub(super) gpu_devices: Vec<GpuDevice>,
    pub(super) cpu_devices: Vec<CpuDevice>,
    rerankers: Vec<LemonadeRerankProvider>,
    /// Standalone embedding providers registered directly (e.g. llamacpp ROCm/CPU).
    /// Each entry becomes its own `run_embed_worker` Tokio task competing on the
    /// shared `embed_queue` — natural work-stealing across all workers.
    extra_embedding_providers: Vec<(Arc<dyn EmbeddingProvider>, String)>,
}

impl InferenceQueueBuilder {
    /// Create an empty builder with no devices registered.
    pub fn new() -> Self {
        Self {
            npu_devices: Vec::new(),
            gpu_devices: Vec::new(),
            cpu_devices: Vec::new(),
            rerankers: Vec::new(),
            extra_embedding_providers: Vec::new(),
        }
    }

    /// Register an NPU device.
    ///
    /// The NPU can service both [`Embedding`] and [`Transcription`] jobs
    /// depending on how the device was constructed.
    ///
    /// [`Embedding`]: crate::hardware::DeviceCapability::Embedding
    /// [`Transcription`]: crate::hardware::DeviceCapability::Transcription
    pub fn with_npu_device(mut self, device: NpuDevice) -> Self {
        self.npu_devices.push(device);
        self
    }

    /// Register a GPU device.
    ///
    /// The GPU can service [`Transcription`] and/or [`TextGeneration`] jobs
    /// depending on which providers were loaded.
    ///
    /// [`Transcription`]: crate::hardware::DeviceCapability::Transcription
    /// [`TextGeneration`]: crate::hardware::DeviceCapability::TextGeneration
    pub fn with_gpu_device(mut self, device: GpuDevice) -> Self {
        self.gpu_devices.push(device);
        self
    }

    /// Register a CPU device.
    ///
    /// The CPU services [`TextToSpeech`] jobs via Kokoro TTS.
    ///
    /// [`TextToSpeech`]: crate::hardware::DeviceCapability::TextToSpeech
    pub fn with_cpu_device(mut self, device: CpuDevice) -> Self {
        self.cpu_devices.push(device);
        self
    }

    /// Register a standalone embedding provider as an additional worker.
    ///
    /// Each call adds one more Tokio task that competes on the shared
    /// `embed_queue`.  Use this to add llamacpp ROCm or CPU embedding models
    /// alongside the NPU worker so that bulk embedding jobs are distributed
    /// across all available devices simultaneously.
    ///
    /// `name` is used only for tracing/logging and can be any descriptive string
    /// (e.g. `"llamacpp(user.ggml-org/embeddinggemma-300M-GGUF)"`).
    ///
    /// The provider's output dimensions **must** match every other embedding
    /// provider registered on this queue — callers are responsible for probing
    /// dimensions via [`EmbeddingProvider::dimensions`] before registering.
    pub fn with_embedding_provider(
        mut self,
        provider: Arc<dyn EmbeddingProvider>,
        name: impl Into<String>,
    ) -> Self {
        self.extra_embedding_providers.push((provider, name.into()));
        self
    }

    /// Register a reranker provider.
    ///
    /// Rerankers service [`Reranking`] jobs via
    /// `POST /api/v1/reranking` on Lemonade Server.
    ///
    /// [`Reranking`]: crate::hardware::DeviceCapability::Reranking
    pub fn with_reranker(mut self, reranker: LemonadeRerankProvider) -> Self {
        self.rerankers.push(reranker);
        self
    }

    /// Spawn background worker Tokio tasks and return an [`InferenceQueue`]
    /// handle.
    ///
    /// # Panics
    ///
    /// Must be called from within a Tokio runtime (required by
    /// `tokio::spawn`).  Will panic if called outside an async context.
    pub fn build(self) -> InferenceQueue {
        let embed_queue = Arc::new(WorkQueue::<EmbedJob>::new());
        let transcribe_queue = Arc::new(WorkQueue::<TranscribeJob>::new());
        let synthesize_queue = Arc::new(WorkQueue::<SynthesizeJob>::new());
        let generate_queue = Arc::new(WorkQueue::<GenerateJob>::new());
        let rerank_queue = Arc::new(WorkQueue::<RerankJob>::new());

        let mut embedding_workers: usize = 0;
        let mut transcription_workers: usize = 0;
        let mut tts_workers: usize = 0;
        let mut llm_workers: usize = 0;
        let mut reranking_workers: usize = 0;

        // ── NPU workers ──────────────────────────────────────────────────────
        for device in self.npu_devices {
            let device = Arc::new(device) as Arc<NpuDevice>;

            // Embedding worker
            if device.has_embedding() {
                let q = Arc::clone(&embed_queue);
                let provider = Arc::clone(&device.embedding);
                let name = device.name.clone();
                embedding_workers += 1;
                debug!(device = %name, "Spawning NPU embedding worker");
                tokio::spawn(async move {
                    run_embed_worker(q, provider, name).await;
                });
            }

            // Transcription worker (NPU FLM whisper)
            if let Some(provider) = device.transcription.clone() {
                let q = Arc::clone(&transcribe_queue);
                let name = device.name.clone();
                transcription_workers += 1;
                debug!(device = %name, "Spawning NPU transcription worker");
                tokio::spawn(async move {
                    run_transcribe_worker(q, provider, name).await;
                });
            }

            // LLM worker (NPU FLM chat)
            if let Some(chat) = device.chat.clone() {
                let q = Arc::clone(&generate_queue);
                let name = device.name.clone();
                llm_workers += 1;
                debug!(device = %name, model = %chat.model, "Spawning NPU LLM worker");
                tokio::spawn(async move {
                    run_llm_worker(q, chat, name).await;
                });
            }
        }

        // ── GPU workers ──────────────────────────────────────────────────────
        for device in self.gpu_devices {
            // GPU STT (whispercpp via Vulkan/ROCm) — shares the transcription
            // queue with the NPU whisper worker; the first free device wins.
            if let Some(stt) = device.stt {
                let q = Arc::clone(&transcribe_queue);
                let name = device.name.clone();
                transcription_workers += 1;
                debug!(device = %name, "Spawning GPU STT transcription worker");
                tokio::spawn(async move {
                    run_gpu_stt_worker(q, stt, name).await;
                });
            }

            // GPU LLM (llamacpp via ROCm/Vulkan) — shares the generate queue
            // with the NPU LLM worker; the first free device wins.
            if let Some(chat) = device.chat {
                let q = Arc::clone(&generate_queue);
                let name = device.name.clone();
                llm_workers += 1;
                debug!(device = %name, model = %chat.model, "Spawning GPU LLM worker");
                tokio::spawn(async move {
                    run_llm_worker(q, chat, name).await;
                });
            }
        }

        // ── CPU workers ──────────────────────────────────────────────────────
        for device in self.cpu_devices {
            if let Some(tts) = device.tts {
                let q = Arc::clone(&synthesize_queue);
                let name = device.name.clone();
                tts_workers += 1;
                debug!(device = %name, "Spawning CPU TTS worker");
                tokio::spawn(async move {
                    run_tts_worker(q, tts, name).await;
                });
            }
        }

        // ── Extra standalone embedding workers ───────────────────────────────
        for (provider, name) in self.extra_embedding_providers {
            let q = Arc::clone(&embed_queue);
            embedding_workers += 1;
            debug!(device = %name, "Spawning standalone embedding worker");
            tokio::spawn(async move {
                run_embed_worker(q, provider, name).await;
            });
        }

        // ── Reranker workers ─────────────────────────────────────────────────
        for reranker in self.rerankers {
            let q = Arc::clone(&rerank_queue);
            let name = format!("Reranker({})", reranker.model);
            reranking_workers += 1;
            debug!(model = %reranker.model, "Spawning reranker worker");
            tokio::spawn(async move {
                run_rerank_worker(q, reranker, name).await;
            });
        }

        let has_embedding = embedding_workers > 0;
        let has_transcription = transcription_workers > 0;
        let has_tts = tts_workers > 0;
        let has_text_generation = llm_workers > 0;
        let has_reranking = reranking_workers > 0;

        if !has_embedding {
            warn!("InferenceQueue built with no embedding-capable devices");
        }
        if !has_transcription {
            warn!("InferenceQueue built with no transcription-capable devices");
        }
        if !has_tts {
            warn!("InferenceQueue built with no TTS-capable devices");
        }
        if !has_text_generation {
            warn!("InferenceQueue built with no LLM-capable devices");
        }

        InferenceQueue {
            embed_queue,
            transcribe_queue,
            synthesize_queue,
            generate_queue,
            rerank_queue,
            has_embedding,
            has_transcription,
            has_tts,
            has_text_generation,
            has_reranking,
            embedding_workers,
            transcription_workers,
            tts_workers,
            llm_workers,
            reranking_workers,
        }
    }
}

impl Default for InferenceQueueBuilder {
    fn default() -> Self {
        Self::new()
    }
}
