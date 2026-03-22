//! inference_queue.rs — Unified capability-based inference queue.
//!
//! The [`InferenceQueue`] accepts embedding, transcription, and TTS jobs from
//! any number of callers and dispatches each job to whichever registered device
//! worker is both **capable** of handling it and **free** to accept new work.
//!
//! # Design
//!
//! Each capability type (Embedding, Transcription, TextToSpeech) has its own
//! shared work queue — a `Mutex<VecDeque<_>>` guarded by a `Notify` semaphore.
//! When the queue is built, one Tokio background task is spawned per
//! (device, capability) pair.  Multiple tasks listening on the same queue
//! implement work-stealing: whichever worker finishes first picks up the next
//! job.
//!
//! ```text
//!   caller                InferenceQueue           device workers (Tokio tasks)
//!   ──────                ──────────────           ────────────────────────────
//!
//!   embed(text)  ───────► embed_queue  ──────────► NpuDevice  (embed-gemma-300m-FLM)
//!                                      ──────────► llamacpp   (nomic-embed-text-v2-moe-GGUF)
//!                                      ──────────► llamacpp   (nomic-embed-text-v1-GGUF)
//!
//!   transcribe() ───────► transcribe_queue ──────► NpuDevice  (whisper-v3-turbo-FLM)
//!                                         ──────► GpuDevice  (Whisper-Large-v3-Turbo)
//!
//!   synthesize() ───────► synthesize_queue ──────► CpuDevice  (kokoro-v1)
//! ```
//!
//! The race is natural: both transcription workers receive a `notify_one()` when
//! a job is pushed, but only one can pop the job from the `VecDeque`.  The other
//! sees an empty queue and goes back to sleep.  If both workers are busy, the job
//! waits in the deque until one finishes.
//!
//! # Race-free wakeup
//!
//! The worker loop registers a [`Notified`](tokio::sync::futures::Notified) future
//! **before** checking the queue.  This prevents the classic lost-wakeup race:
//!
//! ```text
//! loop {
//!     let notified = queue.notify.notified();   // register first
//!     if let Some(job) = queue.try_pop() {       // then check
//!         drop(notified);
//!         process(job).await;
//!     } else {
//!         notified.await;                        // sleep if nothing found
//!     }
//! }
//! ```
//!
//! If a job is pushed between `notified()` and `try_pop()`, the deque will be
//! non-empty and the worker processes it immediately.  If the push happens after
//! `try_pop()` returns `None` but before `.await`, the stored `Notify` permit
//! ensures the worker wakes on the next poll.
//!
//! # Usage
//!
//! ```no_run
//! # use u_forge_ai::inference_queue::InferenceQueueBuilder;
//! # use u_forge_ai::hardware::npu::NpuDevice;
//! # use u_forge_ai::hardware::gpu::GpuDevice;
//! # use u_forge_ai::hardware::cpu::CpuDevice;
//! # async fn run() -> anyhow::Result<()> {
//! # let npu_device: NpuDevice = todo!();
//! # let gpu_device: GpuDevice = todo!();
//! # let cpu_device: CpuDevice = todo!();
//! # let wav_bytes: Vec<u8> = Vec::new();
//! let queue = InferenceQueueBuilder::new()
//!     .with_npu_device(npu_device)
//!     .with_gpu_device(gpu_device)
//!     .with_cpu_device(cpu_device)
//!     .build();
//!
//! // Embeddings go to the NPU
//! let vec = queue.embed("The kingdom fell at dawn.").await?;
//!
//! // Transcription goes to whichever of NPU / GPU is free first
//! let text = queue.transcribe(wav_bytes, "session.wav").await?;
//!
//! // TTS goes to the CPU
//! let audio = queue.synthesize("Welcome, adventurer!", None).await?;
//! # Ok(()) }
//! ```

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use tokio::sync::{oneshot, Notify};
use tracing::{debug, instrument, warn};

use crate::ai::embeddings::EmbeddingProvider;
use crate::hardware::cpu::CpuDevice;
use crate::hardware::gpu::GpuDevice;
use crate::hardware::npu::NpuDevice;
use crate::lemonade::{
    ChatCompletionResponse, ChatRequest, KokoroVoice, LemonadeChatProvider, LemonadeRerankProvider,
    LemonadeSttProvider, LemonadeTtsProvider, RerankDocument,
};
use crate::ai::transcription::TranscriptionProvider;

// ── Internal job types ────────────────────────────────────────────────────────

/// A single text embedding job.
struct EmbedJob {
    text: String,
    response: oneshot::Sender<Result<Vec<f32>>>,
}

/// A single audio transcription job.
struct TranscribeJob {
    audio_bytes: Vec<u8>,
    filename: String,
    response: oneshot::Sender<Result<String>>,
}

/// A single text-to-speech synthesis job.
struct SynthesizeJob {
    text: String,
    /// Explicit voice override; `None` uses the provider's default voice.
    voice: Option<KokoroVoice>,
    response: oneshot::Sender<Result<Vec<u8>>>,
}

/// A single LLM chat-completion job.
struct GenerateJob {
    request: ChatRequest,
    response: oneshot::Sender<Result<ChatCompletionResponse>>,
}

/// A single document reranking job.
struct RerankJob {
    query: String,
    documents: Vec<String>,
    top_n: Option<usize>,
    response: oneshot::Sender<Result<Vec<RerankDocument>>>,
}

// ── MPMC work-queue primitive ─────────────────────────────────────────────────

/// A thread-safe multi-producer / multi-consumer work queue.
///
/// Built from a `parking_lot::Mutex<VecDeque<T>>` plus a `tokio::sync::Notify`
/// to wake sleeping workers when new jobs arrive.  No additional crates needed.
struct WorkQueue<T> {
    queue: Mutex<VecDeque<T>>,
    notify: Notify,
}

impl<T> WorkQueue<T> {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        }
    }

    /// Push a job and wake **one** waiting worker.
    fn push(&self, job: T) {
        self.queue.lock().push_back(job);
        self.notify.notify_one();
    }

    /// Try to pop the next job without blocking.
    fn try_pop(&self) -> Option<T> {
        self.queue.lock().pop_front()
    }

    /// Current number of pending jobs (for monitoring / metrics).
    fn pending(&self) -> usize {
        self.queue.lock().len()
    }
}

// ── Public queue state exposed via QueueStats ─────────────────────────────────

/// Snapshot of the queue's current pending job counts.
#[derive(Debug, Clone)]
pub struct QueueStats {
    /// Jobs waiting to be picked up by an embedding worker.
    pub pending_embeddings: usize,
    /// Jobs waiting to be picked up by a transcription worker.
    pub pending_transcriptions: usize,
    /// Jobs waiting to be picked up by a TTS worker.
    pub pending_syntheses: usize,
    /// Jobs waiting to be picked up by an LLM worker.
    pub pending_generations: usize,
    /// Jobs waiting to be picked up by a reranking worker.
    pub pending_rerankings: usize,
}

// ── InferenceQueue ────────────────────────────────────────────────────────────

/// Shared, capability-based work queue for all AI inference tasks.
///
/// Construct via [`InferenceQueueBuilder`] — register your device workers there
/// and call [`build`](InferenceQueueBuilder::build) to spawn the background
/// Tokio tasks and obtain a queue handle.
///
/// The handle is `Clone` and cheap to clone (`Arc` internals) — hand copies to
/// as many callers as needed.
#[derive(Clone)]
pub struct InferenceQueue {
    embed_queue: Arc<WorkQueue<EmbedJob>>,
    transcribe_queue: Arc<WorkQueue<TranscribeJob>>,
    synthesize_queue: Arc<WorkQueue<SynthesizeJob>>,
    generate_queue: Arc<WorkQueue<GenerateJob>>,
    rerank_queue: Arc<WorkQueue<RerankJob>>,

    // Capability presence flags — avoids dynamic trait-object dispatch for
    // the fast "no device registered" error path.
    has_embedding: bool,
    has_transcription: bool,
    has_tts: bool,
    has_text_generation: bool,
    has_reranking: bool,

    // Worker counts per capability (informational).
    embedding_workers: usize,
    transcription_workers: usize,
    tts_workers: usize,
    llm_workers: usize,
    reranking_workers: usize,
}

impl InferenceQueue {
    // ── Public API ────────────────────────────────────────────────────────────

    /// Submit a text embedding request and await the result.
    ///
    /// Blocks the calling task until a capable device picks up the job and
    /// returns the embedding vector.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No embedding-capable device is registered.
    /// - The worker task was dropped before completing the job (internal error).
    /// - The underlying embedding provider returned an error.
    #[instrument(skip(self, text), fields(text_len))]
    pub async fn embed(&self, text: impl Into<String>) -> Result<Vec<f32>> {
        if !self.has_embedding {
            return Err(anyhow!(
                "InferenceQueue: no embedding-capable device is registered. \
                 Add an NpuDevice with NpuDevice::new() or NpuDevice::embedding_only() \
                 to the builder before calling embed()."
            ));
        }

        let text = text.into();
        tracing::Span::current().record("text_len", text.len());

        let (tx, rx) = oneshot::channel();
        self.embed_queue.push(EmbedJob { text, response: tx });

        rx.await
            .map_err(|_| anyhow!("InferenceQueue: embedding worker dropped the response channel"))?
    }

    /// Submit a batch of texts for embedding.
    ///
    /// Each text is submitted as a separate job; calls are made concurrently
    /// and the results are collected in input order.  This leverages all
    /// available embedding workers simultaneously.
    ///
    /// For small batches this is more efficient than a single
    /// [`embed_batch`](crate::embeddings::EmbeddingProvider::embed_batch) call
    /// because it can parallelise across multiple NPU contexts.
    pub async fn embed_many(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if !self.has_embedding {
            return Err(anyhow!(
                "InferenceQueue: no embedding-capable device is registered"
            ));
        }

        // Fire all jobs concurrently and collect results in input order.
        // We use a JoinSet paired with an index so we can reassemble in order.
        let mut set: tokio::task::JoinSet<(usize, Result<Vec<f32>>)> = tokio::task::JoinSet::new();

        for (i, text) in texts.into_iter().enumerate() {
            let q = self.clone();
            set.spawn(async move { (i, q.embed(text).await) });
        }

        let mut results: Vec<Option<Vec<f32>>> = Vec::new();
        while let Some(join_result) = set.join_next().await {
            let (i, embed_result) = join_result
                .map_err(|e| anyhow!("InferenceQueue: embed_many task panicked: {e}"))?;
            let vec = embed_result?;
            // Grow the results vec if needed.
            if i >= results.len() {
                results.resize_with(i + 1, || None);
            }
            results[i] = Some(vec);
        }

        // Collect, turning any missing slots into errors (should never happen).
        results
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                v.ok_or_else(|| anyhow!("InferenceQueue: embed_many missing result for index {i}"))
            })
            .collect()
    }

    /// Submit an audio transcription request and await the result.
    ///
    /// The job is dispatched to whichever transcription-capable device
    /// (NPU whisper or GPU ROCm whisper) becomes free first.
    ///
    /// # Arguments
    ///
    /// * `audio_bytes` — Contents of a valid audio file (WAV, MP3, OGG, …).
    /// * `filename`    — Filename hint used to infer the MIME type
    ///   (e.g. `"session.wav"`).  See [`mime_for_filename`](crate::transcription::mime_for_filename).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No transcription-capable device is registered.
    /// - The worker task was dropped before completing the job.
    /// - The underlying provider returned an error (bad audio, server error, …).
    #[instrument(skip(self, audio_bytes), fields(filename, audio_bytes_len))]
    pub async fn transcribe(
        &self,
        audio_bytes: Vec<u8>,
        filename: impl Into<String>,
    ) -> Result<String> {
        if !self.has_transcription {
            return Err(anyhow!(
                "InferenceQueue: no transcription-capable device is registered. \
                 Add an NpuDevice::new() or GpuDevice::stt_only() to the builder \
                 before calling transcribe()."
            ));
        }

        let filename = filename.into();
        tracing::Span::current().record("filename", &filename);
        tracing::Span::current().record("audio_bytes_len", audio_bytes.len());

        let (tx, rx) = oneshot::channel();
        self.transcribe_queue.push(TranscribeJob {
            audio_bytes,
            filename,
            response: tx,
        });

        rx.await.map_err(|_| {
            anyhow!("InferenceQueue: transcription worker dropped the response channel")
        })?
    }

    /// Submit a text-to-speech synthesis request and await the audio bytes.
    ///
    /// # Arguments
    ///
    /// * `text`  — Text to synthesise.
    /// * `voice` — Optional voice override.  Passes `None` to use the
    ///   provider's configured default voice.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No TTS-capable device is registered.
    /// - The worker task was dropped before completing the job.
    /// - The underlying TTS provider returned an error.
    #[instrument(skip(self, text), fields(text_len, voice))]
    pub async fn synthesize(
        &self,
        text: impl Into<String>,
        voice: Option<KokoroVoice>,
    ) -> Result<Vec<u8>> {
        if !self.has_tts {
            return Err(anyhow!(
                "InferenceQueue: no TTS-capable device is registered. \
                 Add a CpuDevice::new() to the builder before calling synthesize()."
            ));
        }

        let text = text.into();
        if let Some(ref v) = voice {
            tracing::Span::current().record("voice", v.as_str());
        }
        tracing::Span::current().record("text_len", text.len());

        let (tx, rx) = oneshot::channel();
        self.synthesize_queue.push(SynthesizeJob {
            text,
            voice,
            response: tx,
        });

        rx.await
            .map_err(|_| anyhow!("InferenceQueue: TTS worker dropped the response channel"))?
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    /// Submit a chat-completion / LLM generation request and await the result.
    ///
    /// The job is dispatched to the first available LLM-capable device (GPU
    /// llamacpp or NPU FLM), whichever becomes free first.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No LLM-capable device is registered.
    /// - The worker task was dropped before completing the job.
    /// - The underlying chat provider returned an error.
    #[instrument(skip(self, request), fields(model, n_messages))]
    pub async fn generate(&self, request: ChatRequest) -> Result<ChatCompletionResponse> {
        if !self.has_text_generation {
            return Err(anyhow!(
                "InferenceQueue: no LLM-capable device is registered. \
                 Add a GpuDevice with a chat model or an NpuDevice with an LLM model \
                 to the builder before calling generate()."
            ));
        }

        tracing::Span::current().record("n_messages", request.messages.len());

        let (tx, rx) = oneshot::channel();
        self.generate_queue.push(GenerateJob {
            request,
            response: tx,
        });

        rx.await
            .map_err(|_| anyhow!("InferenceQueue: LLM worker dropped the response channel"))?
    }

    /// Convenience wrapper: submit a single-turn user prompt and return the
    /// assistant's reply text.
    pub async fn ask(&self, prompt: impl Into<String>) -> Result<String> {
        use crate::lemonade::ChatMessage;
        let req = ChatRequest::new(vec![ChatMessage::user(prompt.into())]);
        let resp = self.generate(req).await?;
        resp.first_content()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("InferenceQueue: LLM response contained no choices"))
    }

    /// Submit a document reranking request and await the ranked results.
    ///
    /// # Arguments
    ///
    /// * `query`     — The search query or reference text.
    /// * `documents` — Candidate documents to score and rank.
    /// * `top_n`     — If `Some(n)`, only the top-n results are returned.
    ///
    /// Results are returned sorted by descending relevance score.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No reranking-capable device is registered.
    /// - The worker task was dropped before completing the job.
    /// - The underlying reranker returned an error.
    #[instrument(skip(self, query, documents), fields(n_docs, top_n))]
    pub async fn rerank(
        &self,
        query: impl Into<String>,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>> {
        if !self.has_reranking {
            return Err(anyhow!(
                "InferenceQueue: no reranking-capable device is registered. \
                 Ensure a reranker model is available in the Lemonade registry \
                 and add it via the builder before calling rerank()."
            ));
        }

        let query = query.into();
        tracing::Span::current().record("n_docs", documents.len());
        if let Some(n) = top_n {
            tracing::Span::current().record("top_n", n);
        }

        let (tx, rx) = oneshot::channel();
        self.rerank_queue.push(RerankJob {
            query,
            documents,
            top_n,
            response: tx,
        });

        rx.await
            .map_err(|_| anyhow!("InferenceQueue: reranking worker dropped the response channel"))?
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    /// Returns the current number of pending jobs for each capability type.
    pub fn stats(&self) -> QueueStats {
        QueueStats {
            pending_embeddings: self.embed_queue.pending(),
            pending_transcriptions: self.transcribe_queue.pending(),
            pending_syntheses: self.synthesize_queue.pending(),
            pending_generations: self.generate_queue.pending(),
            pending_rerankings: self.rerank_queue.pending(),
        }
    }

    /// Whether any embedding-capable worker is registered.
    pub fn has_embedding(&self) -> bool {
        self.has_embedding
    }

    /// Whether any transcription-capable worker is registered.
    pub fn has_transcription(&self) -> bool {
        self.has_transcription
    }

    /// Whether any TTS-capable worker is registered.
    pub fn has_tts(&self) -> bool {
        self.has_tts
    }

    /// Whether any LLM-capable worker is registered.
    pub fn has_text_generation(&self) -> bool {
        self.has_text_generation
    }

    /// Whether any reranking-capable worker is registered.
    pub fn has_reranking(&self) -> bool {
        self.has_reranking
    }

    /// Number of background worker tasks registered for embedding.
    pub fn embedding_worker_count(&self) -> usize {
        self.embedding_workers
    }

    /// Number of background worker tasks registered for transcription.
    pub fn transcription_worker_count(&self) -> usize {
        self.transcription_workers
    }

    /// Number of background worker tasks registered for TTS.
    pub fn tts_worker_count(&self) -> usize {
        self.tts_workers
    }

    /// Number of background worker tasks registered for LLM generation.
    pub fn llm_worker_count(&self) -> usize {
        self.llm_workers
    }

    /// Number of background worker tasks registered for reranking.
    pub fn reranking_worker_count(&self) -> usize {
        self.reranking_workers
    }
}

impl std::fmt::Debug for InferenceQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceQueue")
            .field("has_embedding", &self.has_embedding)
            .field("has_transcription", &self.has_transcription)
            .field("has_tts", &self.has_tts)
            .field("has_text_generation", &self.has_text_generation)
            .field("has_reranking", &self.has_reranking)
            .field("embedding_workers", &self.embedding_workers)
            .field("transcription_workers", &self.transcription_workers)
            .field("tts_workers", &self.tts_workers)
            .field("llm_workers", &self.llm_workers)
            .field("reranking_workers", &self.reranking_workers)
            .finish()
    }
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
/// # use u_forge_ai::inference_queue::InferenceQueueBuilder;
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
    npu_devices: Vec<NpuDevice>,
    gpu_devices: Vec<GpuDevice>,
    cpu_devices: Vec<CpuDevice>,
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
    /// (e.g. `"llamacpp(nomic-embed-text-v1-GGUF)/ROCm"`).
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

// ── Tests for with_embedding_provider ────────────────────────────────────────
// (covered by the existing test_embed_* tests via build_mock_queue, which uses
// with_npu_device; the extra_embedding_providers path is exercised by the
// integration path in cli_demo and by the builder field defaulting to empty.)

// ── LLM worker ────────────────────────────────────────────────────────────────

/// LLM generation worker — services both GPU llamacpp and NPU FLM chat providers.
///
/// When `provider.gpu` is `Some`, the [`GpuResourceManager`] inside the provider
/// handles GPU locking automatically inside `complete()`.  When `None` (NPU), the
/// call goes directly to the FLM model with no locking.
async fn run_llm_worker(
    queue: Arc<WorkQueue<GenerateJob>>,
    provider: LemonadeChatProvider,
    device_name: String,
) {
    loop {
        let notified = queue.notify.notified();

        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();
            let n_messages = job.request.messages.len();
            let result = provider.complete(job.request).await;
            debug!(
                device = %device_name,
                n_messages,
                ok = result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "LLM generation job complete"
            );
            let _ = job.response.send(result);
        } else {
            notified.await;
        }
    }
}

// ── Reranker worker ───────────────────────────────────────────────────────────

async fn run_rerank_worker(
    queue: Arc<WorkQueue<RerankJob>>,
    provider: LemonadeRerankProvider,
    device_name: String,
) {
    loop {
        let notified = queue.notify.notified();

        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();
            let n_docs = job.documents.len();
            let result = provider.rerank(&job.query, job.documents, job.top_n).await;
            debug!(
                device = %device_name,
                n_docs,
                top_n = ?job.top_n,
                ok = result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "Rerank job complete"
            );
            let _ = job.response.send(result);
        } else {
            notified.await;
        }
    }
}

// ── Worker loop implementations ───────────────────────────────────────────────
//
// Each function is a long-running async loop.  Workers run until the queue Arc
// is dropped (i.e. the InferenceQueue is dropped), at which point the channel
// senders stored in each job will be dropped, causing the oneshot receivers to
// return errors.
//
// Loop invariant (race-free wakeup):
//
//   1. Create `notified` future — this registers a permit listener BEFORE the
//      deque is checked.
//   2. Try to pop a job.
//   3a. If a job is found: drop the `notified` future (permit stays registered
//       for the next iteration if one was stored), process the job.
//   3b. If no job: `.await` the `notified` future.  This will wake immediately
//       if `notify_one()` was called between steps 1 and 3b.
//
// This is the canonical race-free pattern from the Tokio `Notify` documentation.

/// Maximum number of attempts for a single embed job before the error is
/// returned to the caller.  Retries guard against transient server hiccups
/// (e.g. a Lemonade instance that is momentarily swapping a model in/out).
const EMBED_MAX_ATTEMPTS: u32 = 3;

/// Base delay before the first retry.  Doubles on each subsequent attempt
/// (100 ms → 200 ms) so three attempts add at most ~300 ms of backoff.
const EMBED_RETRY_BASE_MS: u64 = 100;

async fn run_embed_worker(
    queue: Arc<WorkQueue<EmbedJob>>,
    provider: Arc<dyn EmbeddingProvider>,
    device_name: String,
) {
    loop {
        // Step 1: register before checking
        let notified = queue.notify.notified();

        // Step 2: try to pop
        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();

            // Retry loop — attempt up to EMBED_MAX_ATTEMPTS times before
            // giving up and forwarding the final error to the caller.
            let mut last_err: Option<anyhow::Error> = None;
            let mut result: Option<Vec<f32>> = None;

            for attempt in 1..=EMBED_MAX_ATTEMPTS {
                match provider.embed(&job.text).await {
                    Ok(vec) => {
                        result = Some(vec);
                        break;
                    }
                    Err(e) => {
                        let delay_ms = EMBED_RETRY_BASE_MS * (1 << (attempt - 1));
                        debug!(
                            device = %device_name,
                            attempt,
                            EMBED_MAX_ATTEMPTS,
                            delay_ms,
                            error = %e,
                            "Embed attempt failed — retrying"
                        );
                        last_err = Some(e);
                        if attempt < EMBED_MAX_ATTEMPTS {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        }
                    }
                }
            }

            let final_result = result.ok_or_else(|| {
                last_err.unwrap_or_else(|| anyhow::anyhow!("embed failed with no error detail"))
            });

            debug!(
                device = %device_name,
                text_len = job.text.len(),
                ok = final_result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "Embed job complete"
            );

            // Ignore send errors — caller may have timed out and dropped the receiver.
            let _ = job.response.send(final_result);
        } else {
            // Step 3b: nothing in queue, sleep until notified
            notified.await;
        }
    }
}

async fn run_transcribe_worker(
    queue: Arc<WorkQueue<TranscribeJob>>,
    provider: Arc<dyn TranscriptionProvider>,
    device_name: String,
) {
    loop {
        let notified = queue.notify.notified();

        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();
            let result = provider.transcribe(job.audio_bytes, &job.filename).await;
            debug!(
                device = %device_name,
                filename = %job.filename,
                ok = result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "Transcription job complete"
            );
            let _ = job.response.send(result);
        } else {
            notified.await;
        }
    }
}

/// Transcription worker backed by the GPU-managed [`LemonadeSttProvider`].
///
/// This is a separate function (rather than reusing `run_transcribe_worker`)
/// because `LemonadeSttProvider` returns [`TranscriptionResult`](crate::lemonade::TranscriptionResult)
/// (a struct) rather than `String`, so the result must be mapped before
/// sending on the oneshot channel.
async fn run_gpu_stt_worker(
    queue: Arc<WorkQueue<TranscribeJob>>,
    stt: LemonadeSttProvider,
    device_name: String,
) {
    loop {
        let notified = queue.notify.notified();

        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();
            let result = stt
                .transcribe(job.audio_bytes, &job.filename)
                .await
                .map(|r| r.text);
            debug!(
                device = %device_name,
                filename = %job.filename,
                ok = result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "GPU STT job complete"
            );
            let _ = job.response.send(result);
        } else {
            notified.await;
        }
    }
}

async fn run_tts_worker(
    queue: Arc<WorkQueue<SynthesizeJob>>,
    tts: LemonadeTtsProvider,
    device_name: String,
) {
    loop {
        let notified = queue.notify.notified();

        if let Some(job) = queue.try_pop() {
            drop(notified);
            let start = std::time::Instant::now();
            let result = match &job.voice {
                Some(voice) => tts.synthesize(&job.text, Some(&voice)).await,
                None => tts.synthesize_default(&job.text).await,
            };
            debug!(
                device = %device_name,
                text_len = job.text.len(),
                voice = job.voice.as_ref().map(|v| v.as_str()).unwrap_or("default"),
                ok = result.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "TTS job complete"
            );
            let _ = job.response.send(result);
        } else {
            notified.await;
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProviderType};
    use crate::hardware::cpu::CpuDevice;
    use crate::hardware::npu::NpuDevice;

    use crate::test_helpers::lemonade_url;

    /// Build a minimal valid mono 16-bit 16 kHz PCM WAV file of silence.
    /// Inline copy so tests don't depend on the private `transcription::tests` module.
    fn make_test_silence_wav(duration_secs: f32) -> Vec<u8> {
        let sample_rate: u32 = 16_000;
        let num_channels: u16 = 1;
        let bits_per_sample: u16 = 16;
        let num_samples = (sample_rate as f32 * duration_secs) as u32;
        let data_size = num_samples * (bits_per_sample as u32 / 8) * num_channels as u32;
        let riff_size: u32 = 4 + 8 + 16 + 8 + data_size;
        let mut buf: Vec<u8> = Vec::with_capacity((8 + riff_size) as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&riff_size.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&num_channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate: u32 = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align: u16 = num_channels * bits_per_sample / 8;
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_size.to_le_bytes());
        buf.extend(std::iter::repeat(0u8).take(data_size as usize));
        buf
    }

    // ── Mock embedding provider ───────────────────────────────────────────────

    const MOCK_DIMS: usize = 8;

    struct MockEmbeddingProvider;

    #[async_trait::async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok((0..MOCK_DIMS)
                .map(|i| (text.len() as f32 + i as f32) / 1000.0)
                .collect())
        }

        async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            let mut out = Vec::new();
            for t in &texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }

        fn dimensions(&self) -> Result<usize> {
            Ok(MOCK_DIMS)
        }

        fn max_tokens(&self) -> Result<usize> {
            Ok(512)
        }

        fn provider_type(&self) -> EmbeddingProviderType {
            EmbeddingProviderType::Lemonade
        }

        fn model_info(&self) -> Option<EmbeddingModelInfo> {
            None
        }
    }

    // ── Mock transcription provider ───────────────────────────────────────────

    struct MockTranscriptionProvider {
        response: String,
    }

    #[async_trait::async_trait]
    impl TranscriptionProvider for MockTranscriptionProvider {
        async fn transcribe(&self, _audio_bytes: Vec<u8>, _filename: &str) -> Result<String> {
            Ok(self.response.clone())
        }

        fn model_name(&self) -> &str {
            "mock-whisper"
        }
    }

    // ── Helper: build a queue wired to mock providers ─────────────────────────

    fn build_mock_queue() -> InferenceQueue {
        // We bypass the builder's device constructors and create the queue
        // internals directly so we can inject mock providers without a server.
        let embed_queue = Arc::new(WorkQueue::<EmbedJob>::new());
        let transcribe_queue = Arc::new(WorkQueue::<TranscribeJob>::new());
        let synthesize_queue = Arc::new(WorkQueue::<SynthesizeJob>::new());

        // Spawn a mock embedding worker
        {
            let q = Arc::clone(&embed_queue);
            let provider: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbeddingProvider);
            tokio::spawn(async move {
                run_embed_worker(q, provider, "mock-npu".to_string()).await;
            });
        }

        // Spawn two mock transcription workers (simulates NPU + GPU competition)
        for label in ["mock-npu-stt", "mock-gpu-stt"] {
            let q = Arc::clone(&transcribe_queue);
            let provider: Arc<dyn TranscriptionProvider> = Arc::new(MockTranscriptionProvider {
                response: format!("[transcribed by {label}]"),
            });
            let name = label.to_string();
            tokio::spawn(async move {
                run_transcribe_worker(q, provider, name).await;
            });
        }

        InferenceQueue {
            embed_queue,
            transcribe_queue,
            synthesize_queue,
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: true,
            has_transcription: true,
            has_tts: false,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 1,
            transcription_workers: 2,
            tts_workers: 0,
            llm_workers: 0,
            reranking_workers: 0,
        }
    }

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_builder_default_has_no_capabilities() {
        // We cannot call build() outside a runtime, but we can inspect the
        // builder state directly.
        let builder = InferenceQueueBuilder::new();
        assert!(
            builder.npu_devices.is_empty(),
            "New builder should have no NPU devices"
        );
        assert!(
            builder.gpu_devices.is_empty(),
            "New builder should have no GPU devices"
        );
        assert!(
            builder.cpu_devices.is_empty(),
            "New builder should have no CPU devices"
        );
    }

    #[tokio::test]
    async fn test_embed_returns_vector() {
        let queue = build_mock_queue();
        let vec = queue.embed("Hello, world!").await;
        assert!(vec.is_ok(), "embed() failed: {:?}", vec.err());
        let vec = vec.unwrap();
        assert_eq!(
            vec.len(),
            MOCK_DIMS,
            "Expected {MOCK_DIMS} dimensions, got {}",
            vec.len()
        );
    }

    #[tokio::test]
    async fn test_embed_is_deterministic() {
        let queue = build_mock_queue();
        let v1 = queue.embed("same text").await.unwrap();
        let v2 = queue.embed("same text").await.unwrap();
        assert_eq!(v1, v2, "Same input must produce the same embedding");
    }

    #[tokio::test]
    async fn test_embed_many_returns_all_results() {
        let queue = build_mock_queue();
        let texts = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let results = queue.embed_many(texts.clone()).await;
        assert!(results.is_ok(), "embed_many failed: {:?}", results.err());
        let results = results.unwrap();
        assert_eq!(
            results.len(),
            texts.len(),
            "embed_many must return one vector per input"
        );
        for v in &results {
            assert_eq!(v.len(), MOCK_DIMS);
        }
    }

    #[tokio::test]
    async fn test_transcribe_returns_string() {
        let queue = build_mock_queue();
        let wav = vec![0u8; 64]; // dummy audio
        let result = queue.transcribe(wav, "test.wav").await;
        assert!(result.is_ok(), "transcribe() failed: {:?}", result.err());
        let text = result.unwrap();
        assert!(
            !text.is_empty(),
            "Expected non-empty transcription, got empty string"
        );
        // One of the two mock workers should have handled it
        assert!(
            text.contains("[transcribed by"),
            "Expected mock transcription text, got: {text}"
        );
    }

    #[tokio::test]
    async fn test_transcribe_multiple_concurrent_jobs_all_complete() {
        let queue = build_mock_queue();

        // Fire 10 concurrent transcription jobs and check all complete.
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let q = queue.clone();
                tokio::spawn(async move { q.transcribe(vec![0u8; 8], format!("f{i}.wav")).await })
            })
            .collect();

        for h in handles {
            let result = h.await.expect("task panicked");
            assert!(
                result.is_ok(),
                "Concurrent transcription job failed: {:?}",
                result.err()
            );
        }
    }

    #[tokio::test]
    async fn test_synthesize_errors_when_no_tts_device() {
        let queue = build_mock_queue(); // no TTS workers
        let result = queue.synthesize("Hello!", None).await;
        assert!(
            result.is_err(),
            "Expected error when no TTS device registered"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("TTS"),
            "Error message should mention TTS: {msg}"
        );
    }

    #[tokio::test]
    async fn test_embed_errors_when_no_embedding_device() {
        // Build a queue with no embedding workers
        let q = InferenceQueue {
            embed_queue: Arc::new(WorkQueue::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: false,
            has_transcription: false,
            has_tts: false,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 0,
            transcription_workers: 0,
            tts_workers: 0,
            llm_workers: 0,
            reranking_workers: 0,
        };
        let result = q.embed("test").await;
        assert!(result.is_err(), "Expected error with no embedding device");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("embedding"),
            "Error should mention embedding: {msg}"
        );
    }

    #[tokio::test]
    async fn test_transcribe_errors_when_no_transcription_device() {
        let q = InferenceQueue {
            embed_queue: Arc::new(WorkQueue::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: false,
            has_transcription: false,
            has_tts: false,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 0,
            transcription_workers: 0,
            tts_workers: 0,
            llm_workers: 0,
            reranking_workers: 0,
        };
        let result = q.transcribe(vec![], "test.wav").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("transcription"),
            "Error should mention transcription: {msg}"
        );
    }

    #[tokio::test]
    async fn test_stats_reflect_pending_jobs() {
        // Use a very slow mock provider so jobs pile up in the queue.
        struct SlowProvider;
        #[async_trait::async_trait]
        impl EmbeddingProvider for SlowProvider {
            async fn embed(&self, text: &str) -> Result<Vec<f32>> {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(vec![0.0; MOCK_DIMS])
            }
            async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
                Ok(vec![vec![0.0; MOCK_DIMS]; texts.len()])
            }
            fn dimensions(&self) -> Result<usize> {
                Ok(MOCK_DIMS)
            }
            fn max_tokens(&self) -> Result<usize> {
                Ok(512)
            }
            fn provider_type(&self) -> EmbeddingProviderType {
                EmbeddingProviderType::Lemonade
            }
            fn model_info(&self) -> Option<EmbeddingModelInfo> {
                None
            }
        }

        let embed_queue = Arc::new(WorkQueue::<EmbedJob>::new());
        {
            let q = Arc::clone(&embed_queue);
            let provider: Arc<dyn EmbeddingProvider> = Arc::new(SlowProvider);
            tokio::spawn(async move {
                run_embed_worker(q, provider, "slow-npu".to_string()).await;
            });
        }

        let queue = InferenceQueue {
            embed_queue,
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: true,
            has_transcription: false,
            has_tts: false,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 1,
            transcription_workers: 0,
            tts_workers: 0,
            llm_workers: 0,
            reranking_workers: 0,
        };

        // Push several jobs quickly.
        let futures: Vec<_> = (0..5).map(|i| queue.embed(format!("text {i}"))).collect();

        // At least 0 pending (the worker may have grabbed the first one already).
        let stats = queue.stats();
        assert!(
            stats.pending_embeddings <= 5,
            "Pending should be <= 5, got {}",
            stats.pending_embeddings
        );

        // Wait for all to finish.
        let mut all_ok = true;
        for f in futures {
            if f.await.is_err() {
                all_ok = false;
            }
        }
        assert!(all_ok, "All embed jobs should succeed");

        // Queue should be drained now.
        let stats = queue.stats();
        assert_eq!(
            stats.pending_embeddings, 0,
            "Queue should be empty after all jobs complete"
        );
    }

    #[test]
    fn test_queue_debug_format() {
        let q = InferenceQueue {
            embed_queue: Arc::new(WorkQueue::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: true,
            has_transcription: true,
            has_tts: false,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 1,
            transcription_workers: 2,
            tts_workers: 0,
            llm_workers: 0,
            reranking_workers: 0,
        };
        let debug = format!("{q:?}");
        assert!(
            debug.contains("InferenceQueue"),
            "Debug must include struct name"
        );
        assert!(
            debug.contains("has_embedding: true"),
            "Debug must reflect embedding flag"
        );
        assert!(
            debug.contains("transcription_workers: 2"),
            "Debug must show worker counts"
        );
    }

    #[test]
    fn test_worker_count_accessors() {
        let q = InferenceQueue {
            embed_queue: Arc::new(WorkQueue::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: true,
            has_transcription: true,
            has_tts: true,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 1,
            transcription_workers: 2,
            tts_workers: 1,
            llm_workers: 0,
            reranking_workers: 0,
        };
        assert_eq!(q.embedding_worker_count(), 1);
        assert_eq!(q.transcription_worker_count(), 2);
        assert_eq!(q.tts_worker_count(), 1);
    }

    #[test]
    fn test_capability_flags() {
        let q = InferenceQueue {
            embed_queue: Arc::new(WorkQueue::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            has_embedding: true,
            has_transcription: false,
            has_tts: true,
            has_text_generation: false,
            has_reranking: false,
            embedding_workers: 1,
            transcription_workers: 0,
            tts_workers: 1,
            llm_workers: 0,
            reranking_workers: 0,
        };
        assert!(q.has_embedding());
        assert!(!q.has_transcription());
        assert!(q.has_tts());
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_queue_embed_via_npu() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };

        let npu = match NpuDevice::embedding_only(&url, None).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping: NpuDevice construction failed: {e}");
                return;
            }
        };

        let queue = InferenceQueueBuilder::new().with_npu_device(npu).build();

        let vec = queue
            .embed("The foundation stood for a thousand years")
            .await;
        assert!(vec.is_ok(), "embed() via NPU failed: {:?}", vec.err());
        let vec = vec.unwrap();
        assert!(!vec.is_empty(), "Expected non-empty embedding");
        assert!(
            vec.iter().all(|&x| x.is_finite()),
            "Embedding contains non-finite values"
        );
    }

    #[tokio::test]
    async fn test_queue_transcribe_via_npu() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };

        let npu = match NpuDevice::new(&url, None, None, None).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping: NpuDevice construction failed: {e}");
                return;
            }
        };

        let queue = InferenceQueueBuilder::new().with_npu_device(npu).build();

        // 1 second of silence
        // 1 second of silence — valid WAV, no speech content.
        let wav = make_test_silence_wav(1.0);
        let result = queue.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "transcribe() via NPU failed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_queue_two_transcription_workers_compete() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable and LEMONADE_URL not set");
            return;
        };

        // Register the same NPU whisper provider twice to simulate two workers
        // competing.  In a real setup these would be NPU + GPU.
        let npu1 = match NpuDevice::new(&url, None, None, None).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping: NpuDevice 1 construction failed: {e}");
                return;
            }
        };
        let npu2 = match NpuDevice::new(&url, None, None, None).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Skipping: NpuDevice 2 construction failed: {e}");
                return;
            }
        };

        let queue = InferenceQueueBuilder::new()
            .with_npu_device(npu1)
            .with_npu_device(npu2)
            .build();

        assert_eq!(
            queue.transcription_worker_count(),
            2,
            "Expected 2 transcription workers"
        );

        let wav = make_test_silence_wav(0.5);

        // Fire two concurrent jobs — ideally each worker picks up one.
        let (r1, r2) = tokio::join!(
            queue.transcribe(wav.clone(), "a.wav"),
            queue.transcribe(wav.clone(), "b.wav"),
        );
        assert!(r1.is_ok(), "Job 1 failed: {:?}", r1.err());
        assert!(r2.is_ok(), "Job 2 failed: {:?}", r2.err());
    }
}
