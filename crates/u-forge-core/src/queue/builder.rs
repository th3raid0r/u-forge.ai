//! [`InferenceQueueBuilder`] — register providers and spawn background tasks.

use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

use tracing::{debug, warn};

use crate::ai::embeddings::EmbeddingProvider;
use crate::config::AppConfig;
use crate::lemonade::provider_factory::{BuiltProvider, Capability, ProviderSlot};

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
/// Register providers with [`with_provider`] or [`with_providers`], then call
/// [`build`] to spawn the background Tokio tasks and get a queue handle.
///
/// Providers are created by [`ProviderFactory::build`] and carry a [`Capability`]
/// tag that routes them to the correct internal channel.
///
/// # Example
///
/// ```no_run
/// # use u_forge_core::queue::InferenceQueueBuilder;
/// # use u_forge_core::lemonade::{LemonadeServerCatalog, LemonadeRerankProvider};
/// # use u_forge_core::lemonade::provider_factory::{ProviderFactory, Capability};
/// # async fn run() -> anyhow::Result<()> {
/// # let built_providers = vec![];
/// let queue = InferenceQueueBuilder::new()
///     .with_providers(built_providers)
///     .build();
/// # Ok(()) }
/// ```
///
/// [`with_provider`]: InferenceQueueBuilder::with_provider
/// [`with_providers`]: InferenceQueueBuilder::with_providers
/// [`build`]: InferenceQueueBuilder::build
/// [`ProviderFactory::build`]: crate::lemonade::provider_factory::ProviderFactory::build
pub struct InferenceQueueBuilder {
    pub(super) providers: Vec<BuiltProvider>,
    config: AppConfig,
}

impl InferenceQueueBuilder {
    /// Create an empty builder with no providers registered.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            config: AppConfig::default(),
        }
    }

    /// Register a single provider.
    ///
    /// The provider's [`Capability`] tag determines which internal channel it
    /// is routed to.  `weight` is used only for [`Capability::Embedding`]
    /// workers — it controls dispatch priority in the
    /// [`WeightedEmbedDispatcher`].
    ///
    /// [`WeightedEmbedDispatcher`]: crate::queue::weighted::WeightedEmbedDispatcher
    pub fn with_provider(mut self, provider: BuiltProvider) -> Self {
        self.providers.push(provider);
        self
    }

    /// Register all providers from a `Vec`.
    ///
    /// Convenience form of calling [`with_provider`] in a loop.
    ///
    /// [`with_provider`]: InferenceQueueBuilder::with_provider
    pub fn with_providers(mut self, providers: Vec<BuiltProvider>) -> Self {
        self.providers.extend(providers);
        self
    }

    /// Override the application configuration used to control which backends
    /// are enabled, their dispatch weights, and model context limits.
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

        // ── Phase 1: register workers with the dispatcher (no spawning yet) ──
        //
        // Embed workers need an Arc<WeightedEmbedDispatcher> for work-stealing,
        // so we collect their specs here and spawn after wrapping the dispatcher.

        for built in self.providers {
            match (built.capability, built.provider) {
                (Capability::Embedding, ProviderSlot::Embedding(provider)) => {
                    let (queue, idle, ewma_us) =
                        embed_dispatcher.add_worker(built.weight, &built.name);
                    debug!(name = %built.name, weight = built.weight, "Registered embedding worker");
                    embed_specs.push(EmbedWorkerSpec {
                        queue,
                        idle,
                        ewma_us,
                        provider,
                        name: built.name,
                    });
                }
                (Capability::Transcription, ProviderSlot::Transcription(provider)) => {
                    let q = Arc::clone(&transcribe_queue);
                    let name = built.name;
                    transcription_workers += 1;
                    debug!(name = %name, "Spawning transcription worker");
                    tokio::spawn(async move {
                        run_transcribe_worker(q, provider, name).await;
                    });
                }
                (Capability::TextGeneration, ProviderSlot::Chat(chat)) => {
                    let q = Arc::clone(&generate_queue);
                    let name = built.name;
                    llm_workers += 1;
                    chat_providers_for_stream.push(chat.clone());
                    debug!(name = %name, model = %chat.model, "Spawning LLM worker");
                    tokio::spawn(async move {
                        run_llm_worker(q, chat, name).await;
                    });
                }
                (Capability::TextToSpeech, ProviderSlot::Tts(tts)) => {
                    let q = Arc::clone(&synthesize_queue);
                    let name = built.name;
                    tts_workers += 1;
                    debug!(name = %name, "Spawning TTS worker");
                    tokio::spawn(async move {
                        run_tts_worker(q, *tts, name).await;
                    });
                }
                (Capability::Reranking, ProviderSlot::Rerank(reranker)) => {
                    let q = Arc::clone(&rerank_queue);
                    let name = built.name;
                    reranking_workers += 1;
                    debug!(name = %name, "Spawning reranker worker");
                    tokio::spawn(async move {
                        run_rerank_worker(q, reranker, name).await;
                    });
                }
                (cap, _) => {
                    warn!(capability = ?cap, "Mismatched capability/provider slot — skipped");
                }
            }
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
            debug!(name = %spec.name, "Spawning embed worker");
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

        if embedding_workers == 0 {
            warn!("InferenceQueue built with no embedding workers");
        }
        if transcription_workers == 0 {
            warn!("InferenceQueue built with no transcription workers");
        }
        if tts_workers == 0 {
            warn!("InferenceQueue built with no TTS workers");
        }
        if llm_workers == 0 {
            warn!("InferenceQueue built with no LLM workers");
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
