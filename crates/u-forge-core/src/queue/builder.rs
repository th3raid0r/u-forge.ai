//! [`InferenceQueueBuilder`] — register providers and spawn background tasks.

use std::sync::{Arc, atomic::AtomicU64};

use tracing::{debug, warn};

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use crate::config::AppConfig;
use crate::lemonade::provider_factory::{BuiltProvider, Capability, ProviderSlot};
use crate::lemonade::{CoordinatedChatProvider, LemonadeTtsProvider, RerankProvider};

use super::dispatch::InferenceQueue;
use super::jobs::{
    EmbedJob, GenerateJob, GenerateStreamJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue,
};
use super::telemetry::QueueMetrics;
use super::weighted::WeightedEmbedDispatcher;
use super::workers::{
    run_embed_worker, run_llm_stream_worker, run_llm_worker, run_rerank_worker,
    run_transcribe_worker, run_tts_worker,
};

/// Collected information for a single embedding worker, deferred until the
/// dispatcher is fully built and wrapped in an `Arc`.
struct EmbedWorkerSpec {
    queue: Arc<WorkQueue<EmbedJob>>,
    ewma_us: Arc<AtomicU64>,
    provider: Arc<dyn EmbeddingProvider>,
    name: String,
}

struct QueueSet {
    transcribe: Arc<WorkQueue<TranscribeJob>>,
    synthesize: Arc<WorkQueue<SynthesizeJob>>,
    generate: Arc<WorkQueue<GenerateJob>>,
    generate_stream: Arc<WorkQueue<GenerateStreamJob>>,
    rerank: Arc<WorkQueue<RerankJob>>,
}

impl QueueSet {
    fn new() -> Self {
        Self {
            transcribe: Arc::new(WorkQueue::new()),
            synthesize: Arc::new(WorkQueue::new()),
            generate: Arc::new(WorkQueue::new()),
            generate_stream: Arc::new(WorkQueue::new()),
            rerank: Arc::new(WorkQueue::new()),
        }
    }
}

#[derive(Default)]
struct ProviderCounts {
    embedding: usize,
    transcription: usize,
    tts: usize,
    llm: usize,
    reranking: usize,
}

struct RegistrationState {
    embed_dispatcher: WeightedEmbedDispatcher,
    embed_specs: Vec<EmbedWorkerSpec>,
    embedding_models: Vec<String>,
    counts: ProviderCounts,
}

struct RegistrationOutput {
    embed_dispatcher: Arc<WeightedEmbedDispatcher>,
    embedding_space_fingerprint: Option<Arc<str>>,
    counts: ProviderCounts,
}

impl RegistrationState {
    fn new() -> Self {
        Self {
            embed_dispatcher: WeightedEmbedDispatcher::new(),
            embed_specs: Vec::new(),
            embedding_models: Vec::new(),
            counts: ProviderCounts::default(),
        }
    }

    fn register(&mut self, built: BuiltProvider, queues: &QueueSet, config: &AppConfig) {
        let BuiltProvider {
            name,
            capability,
            provider,
            weight,
        } = built;
        match (capability, provider) {
            (Capability::Embedding, ProviderSlot::Embedding(provider)) => {
                self.register_embedding(name, weight, provider)
            }
            (Capability::Transcription, ProviderSlot::Transcription(provider)) => {
                self.register_transcription(name, provider, queues)
            }
            (Capability::TextGeneration, ProviderSlot::Chat(provider)) => {
                self.register_llm(name, provider, queues, config)
            }
            (Capability::TextToSpeech, ProviderSlot::Tts(provider)) => {
                self.register_tts(name, provider, queues)
            }
            (Capability::Reranking, ProviderSlot::Rerank(provider)) => {
                self.register_reranker(name, provider, queues)
            }
            (capability, _) => {
                warn!(?capability, "Mismatched capability/provider slot — skipped");
            }
        }
    }

    fn register_embedding(
        &mut self,
        name: String,
        weight: u32,
        provider: Arc<dyn EmbeddingProvider>,
    ) {
        let model = provider
            .model_info()
            .map(|info| format!("{}@{}", info.name, info.dimensions))
            .unwrap_or_else(|| format!("{name}@unknown"));
        let (queue, ewma_us) = self.embed_dispatcher.add_worker(weight, &name);
        debug!(%name, weight, "Registered embedding worker");
        self.embedding_models.push(model);
        self.embed_specs.push(EmbedWorkerSpec {
            queue,
            ewma_us,
            provider,
            name,
        });
        self.counts.embedding += 1;
    }

    fn register_transcription(
        &mut self,
        name: String,
        provider: Arc<dyn TranscriptionProvider>,
        queues: &QueueSet,
    ) {
        let queue = Arc::clone(&queues.transcribe);
        debug!(%name, "Spawning transcription worker");
        tokio::spawn(async move { run_transcribe_worker(queue, provider, name).await });
        self.counts.transcription += 1;
    }

    fn register_llm(
        &mut self,
        name: String,
        provider: Box<CoordinatedChatProvider>,
        queues: &QueueSet,
        config: &AppConfig,
    ) {
        let mut provider = *provider;
        provider.profile.reasoning_control = config.chat.reasoning_control;
        let stream_provider = provider.clone();
        let stream_name = name.clone();
        let queue = Arc::clone(&queues.generate);
        let stream_queue = Arc::clone(&queues.generate_stream);
        debug!(%name, model = %provider.provider.model, "Spawning LLM worker");
        tokio::spawn(async move { run_llm_worker(queue, provider, name).await });
        tokio::spawn(async move {
            run_llm_stream_worker(stream_queue, stream_provider, stream_name).await;
        });
        self.counts.llm += 1;
    }

    fn register_tts(
        &mut self,
        name: String,
        provider: Box<LemonadeTtsProvider>,
        queues: &QueueSet,
    ) {
        let queue = Arc::clone(&queues.synthesize);
        debug!(%name, "Spawning TTS worker");
        tokio::spawn(async move { run_tts_worker(queue, *provider, name).await });
        self.counts.tts += 1;
    }

    fn register_reranker(
        &mut self,
        name: String,
        provider: Arc<dyn RerankProvider>,
        queues: &QueueSet,
    ) {
        let queue = Arc::clone(&queues.rerank);
        debug!(%name, "Spawning reranker worker");
        tokio::spawn(async move { run_rerank_worker(queue, provider, name).await });
        self.counts.reranking += 1;
    }

    fn finish(mut self) -> RegistrationOutput {
        self.embedding_models.sort();
        self.embedding_models.dedup();
        let embedding_space_fingerprint = if self.embedding_models.is_empty() {
            None
        } else {
            Some(Arc::<str>::from(self.embedding_models.join("|")))
        };
        let embed_dispatcher = Arc::new(self.embed_dispatcher);

        for spec in self.embed_specs {
            let dispatcher = Arc::clone(&embed_dispatcher);
            debug!(name = %spec.name, "Spawning embed worker");
            tokio::spawn(async move {
                run_embed_worker(
                    spec.queue,
                    spec.provider,
                    spec.name,
                    spec.ewma_us,
                    dispatcher,
                )
                .await;
            });
        }

        RegistrationOutput {
            embed_dispatcher,
            embedding_space_fingerprint,
            counts: self.counts,
        }
    }
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
        let queues = QueueSet::new();
        let metrics = Arc::new(QueueMetrics::default());
        let mut registration = RegistrationState::new();

        // Embedding registration is deliberately two-phase: all slots must be
        // present before the shared dispatcher can be wrapped and workers spawn.
        for provider in self.providers {
            registration.register(provider, &queues, &self.config);
        }
        let registration = registration.finish();
        let counts = registration.counts;

        if counts.embedding == 0 {
            warn!("InferenceQueue built with no embedding workers");
        }
        if counts.transcription == 0 {
            warn!("InferenceQueue built with no transcription workers");
        }
        if counts.tts == 0 {
            warn!("InferenceQueue built with no TTS workers");
        }
        if counts.llm == 0 {
            warn!("InferenceQueue built with no LLM workers");
        }

        InferenceQueue {
            embed_dispatcher: registration.embed_dispatcher,
            transcribe_queue: queues.transcribe,
            synthesize_queue: queues.synthesize,
            generate_queue: queues.generate,
            generate_stream_queue: queues.generate_stream,
            rerank_queue: queues.rerank,
            metrics,
            embedding_space_fingerprint: registration.embedding_space_fingerprint,
            embedding_workers: counts.embedding,
            transcription_workers: counts.transcription,
            tts_workers: counts.tts,
            llm_workers: counts.llm,
            reranking_workers: counts.reranking,
        }
    }
}

impl Default for InferenceQueueBuilder {
    fn default() -> Self {
        Self::new()
    }
}
