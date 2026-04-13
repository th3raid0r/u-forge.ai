//! [`InferenceQueueBuilder`] — register device workers and spawn background tasks.

use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};


use tracing::{debug, warn};

use crate::ai::embeddings::EmbeddingProvider;
use crate::config::AppConfig;
use crate::hardware::cpu::CpuDevice;
use crate::hardware::gpu::GpuDevice;
use crate::hardware::npu::NpuDevice;
use crate::lemonade::LemonadeRerankProvider;

use super::dispatch::InferenceQueue;
use super::jobs::{EmbedJob, GenerateJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue};
use super::weighted::WeightedEmbedDispatcher;
use super::workers::{
    run_embed_worker, run_llm_worker, run_rerank_worker, run_transcribe_worker, run_tts_worker,
};

/// Collected information for a single embedding worker, deferred until the
/// dispatcher is fully built and wrapped in an `Arc`.
struct EmbedWorkerSpec {
    queue: Arc<WorkQueue<EmbedJob>>,
    idle: Arc<AtomicBool>,
    ewma_us: Arc<AtomicU64>,
    provider: Arc<dyn EmbeddingProvider>,
    name: String,
}

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
    /// Each entry becomes its own `run_embed_worker` Tokio task with the given weight.
    extra_embedding_providers: Vec<(Arc<dyn EmbeddingProvider>, String, u32)>,
    /// Application configuration controlling which backends to use and their weights.
    config: AppConfig,
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
            config: AppConfig::default(),
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
    /// The GPU can service [`Transcription`], [`TextGeneration`], and/or
    /// [`Embedding`] jobs depending on which providers were loaded.
    ///
    /// [`Transcription`]: crate::hardware::DeviceCapability::Transcription
    /// [`TextGeneration`]: crate::hardware::DeviceCapability::TextGeneration
    /// [`Embedding`]: crate::hardware::DeviceCapability::Embedding
    pub fn with_gpu_device(mut self, device: GpuDevice) -> Self {
        self.gpu_devices.push(device);
        self
    }

    /// Register a CPU device.
    ///
    /// The CPU services [`TextToSpeech`] and optionally [`Embedding`] jobs.
    ///
    /// [`TextToSpeech`]: crate::hardware::DeviceCapability::TextToSpeech
    /// [`Embedding`]: crate::hardware::DeviceCapability::Embedding
    pub fn with_cpu_device(mut self, device: CpuDevice) -> Self {
        self.cpu_devices.push(device);
        self
    }

    /// Register a standalone embedding provider as an additional worker.
    ///
    /// `weight` controls dispatch priority relative to device-based workers.
    /// Use `config.embedding.cpu_weight` (default 10) for CPU-tier providers.
    ///
    /// `name` is used only for tracing/logging.
    ///
    /// The provider's output dimensions **must** match every other embedding
    /// provider registered on this queue — callers are responsible for probing
    /// dimensions via [`EmbeddingProvider::dimensions`] before registering.
    pub fn with_embedding_provider(
        mut self,
        provider: Arc<dyn EmbeddingProvider>,
        name: impl Into<String>,
    ) -> Self {
        let weight = self.config.embedding.cpu_weight;
        self.extra_embedding_providers.push((provider, name.into(), weight));
        self
    }

    /// Register a standalone embedding provider with an explicit dispatch weight.
    pub fn with_embedding_provider_weighted(
        mut self,
        provider: Arc<dyn EmbeddingProvider>,
        name: impl Into<String>,
        weight: u32,
    ) -> Self {
        self.extra_embedding_providers.push((provider, name.into(), weight));
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

    /// Override the application configuration used to control which backends are
    /// enabled, their dispatch weights, and model context limits.
    ///
    /// Defaults to [`AppConfig::default()`] (all backends enabled, standard
    /// weights) if this method is not called.
    pub fn with_config(mut self, config: AppConfig) -> Self {
        self.config = config;
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
        let mut embed_dispatcher = WeightedEmbedDispatcher::new();
        let transcribe_queue = Arc::new(WorkQueue::<TranscribeJob>::new());
        let synthesize_queue = Arc::new(WorkQueue::<SynthesizeJob>::new());
        let generate_queue = Arc::new(WorkQueue::<GenerateJob>::new());
        let rerank_queue = Arc::new(WorkQueue::<RerankJob>::new());

        let mut embed_specs: Vec<EmbedWorkerSpec> = Vec::new();
        let mut chat_providers_for_stream: Vec<crate::lemonade::LemonadeChatProvider> = Vec::new();
        let mut transcription_workers: usize = 0;
        let mut tts_workers: usize = 0;
        let mut llm_workers: usize = 0;
        let mut reranking_workers: usize = 0;

        let cfg = &self.config.embedding;

        // ── Phase 1: register workers with the dispatcher (no spawning yet) ──
        //
        // Embed workers need an Arc<WeightedEmbedDispatcher> for work-stealing,
        // so we collect their specs here and spawn after wrapping the dispatcher.

        // ── NPU workers ──────────────────────────────────────────────────────
        for device in self.npu_devices {
            let device = Arc::new(device) as Arc<NpuDevice>;

            if cfg.npu_enabled && device.has_embedding() {
                let provider = Arc::clone(device.embedding.as_ref().expect("has_embedding() true but embedding is None"));
                let name = device.name.clone();
                let (queue, idle, ewma_us) = embed_dispatcher.add_worker(cfg.npu_weight, &name);
                debug!(device = %name, weight = cfg.npu_weight, "Registered NPU embedding worker");
                embed_specs.push(EmbedWorkerSpec { queue, idle, ewma_us, provider, name });
            }

            if let Some(provider) = device.transcription.clone() {
                let q = Arc::clone(&transcribe_queue);
                let name = device.name.clone();
                transcription_workers += 1;
                debug!(device = %name, "Spawning NPU transcription worker");
                tokio::spawn(async move {
                    run_transcribe_worker(q, provider, name).await;
                });
            }

            if let Some(chat) = device.chat.clone() {
                let q = Arc::clone(&generate_queue);
                let name = device.name.clone();
                llm_workers += 1;
                chat_providers_for_stream.push(chat.clone());
                debug!(device = %name, model = %chat.model, "Spawning NPU LLM worker");
                tokio::spawn(async move {
                    run_llm_worker(q, chat, name).await;
                });
            }
        }

        // ── GPU workers ──────────────────────────────────────────────────────
        for device in self.gpu_devices {
            if cfg.gpu_enabled {
                if let Some(provider) = device.embedding {
                    let name = format!("{} (embed)", device.name);
                    let (queue, idle, ewma_us) =
                        embed_dispatcher.add_worker(cfg.gpu_weight, &name);
                    debug!(device = %name, weight = cfg.gpu_weight, "Registered GPU embedding worker");
                    embed_specs.push(EmbedWorkerSpec { queue, idle, ewma_us, provider, name });
                }
            }

            if let Some(stt) = device.stt {
                let q = Arc::clone(&transcribe_queue);
                let name = device.name.clone();
                transcription_workers += 1;
                debug!(device = %name, "Spawning GPU STT transcription worker");
                tokio::spawn(async move {
                    run_transcribe_worker(q, stt, name).await;
                });
            }

            if let Some(chat) = device.chat {
                let q = Arc::clone(&generate_queue);
                let name = device.name.clone();
                llm_workers += 1;
                chat_providers_for_stream.push(chat.clone());
                debug!(device = %name, model = %chat.model, "Spawning GPU LLM worker");
                tokio::spawn(async move {
                    run_llm_worker(q, chat, name).await;
                });
            }
        }

        // ── CPU workers ──────────────────────────────────────────────────────
        for device in self.cpu_devices {
            if cfg.cpu_enabled {
                if let Some(provider) = device.embedding {
                    let name = format!("{} (embed)", device.name);
                    let (queue, idle, ewma_us) =
                        embed_dispatcher.add_worker(cfg.cpu_weight, &name);
                    debug!(device = %name, weight = cfg.cpu_weight, "Registered CPU embedding worker");
                    embed_specs.push(EmbedWorkerSpec { queue, idle, ewma_us, provider, name });
                }
            }

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
        for (provider, name, weight) in self.extra_embedding_providers {
            let (queue, idle, ewma_us) = embed_dispatcher.add_worker(weight, &name);
            debug!(device = %name, weight, "Registered standalone embedding worker");
            embed_specs.push(EmbedWorkerSpec { queue, idle, ewma_us, provider, name });
        }

        // ── Phase 2: wrap dispatcher in Arc, then spawn embed workers ─────────
        //
        // Workers need Arc<WeightedEmbedDispatcher> to call steal_from_busiest
        // and to sleep on global_notify, so we wrap only after all workers are
        // registered.
        let embed_dispatcher = Arc::new(embed_dispatcher);
        let embedding_workers = embed_specs.len();

        for spec in embed_specs {
            let dispatcher = Arc::clone(&embed_dispatcher);
            debug!(device = %spec.name, "Spawning embed worker");
            tokio::spawn(async move {
                run_embed_worker(
                    spec.queue,
                    spec.provider,
                    spec.name,
                    spec.idle,
                    spec.ewma_us,
                    dispatcher,
                )
                .await;
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

        if embedding_workers == 0 {
            warn!("InferenceQueue built with no embedding-capable devices");
        }
        if transcription_workers == 0 {
            warn!("InferenceQueue built with no transcription-capable devices");
        }
        if tts_workers == 0 {
            warn!("InferenceQueue built with no TTS-capable devices");
        }
        if llm_workers == 0 {
            warn!("InferenceQueue built with no LLM-capable devices");
        }

        InferenceQueue {
            embed_dispatcher,
            transcribe_queue,
            synthesize_queue,
            generate_queue,
            rerank_queue,
            chat_providers: Arc::new(chat_providers_for_stream),
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
