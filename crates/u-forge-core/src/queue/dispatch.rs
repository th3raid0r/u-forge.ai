//! [`InferenceQueue`] struct, its public API, and its [`QueueStats`] snapshot type.

use std::sync::Arc;

use anyhow::anyhow;
use tokio::sync::{mpsc, oneshot};
use tracing::instrument;

use crate::lemonade::{
    ChatCompletionResponse, ChatRequest, KokoroVoice, RerankDocument, StreamToken,
};

use super::jobs::{
    EmbedJob, GenerateJob, GenerateStreamJob, JobContext, RerankJob, SynthesizeJob, TranscribeJob,
    WorkQueue,
};
use super::lifecycle::{
    CancellationToken, InferenceError, InferenceJob, InferenceResult, JobCompletion,
    StreamingInferenceJob, submit_one_shot,
};
use super::telemetry::{QueueCounters, QueueMetrics};
use super::weighted::WeightedEmbedDispatcher;

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
    /// Streaming jobs waiting to be picked up by an LLM worker.
    pub pending_generation_streams: usize,
    /// Jobs waiting to be picked up by a reranking worker.
    pub pending_rerankings: usize,
    /// Race-safe lifecycle totals and bounded latency summaries.
    pub counters: QueueCounters,
}

// ── InferenceQueue ────────────────────────────────────────────────────────────

/// Shared, capability-based work queue for all AI inference tasks.
///
/// Construct via [`InferenceQueueBuilder`] — register your device workers there
/// and call [`build`](super::builder::InferenceQueueBuilder::build) to spawn the
/// background Tokio tasks and obtain a queue handle.
///
/// The handle is `Clone` and cheap to clone (`Arc` internals) — hand copies to
/// as many callers as needed.
#[derive(Clone)]
pub struct InferenceQueue {
    pub(super) embed_dispatcher: Arc<WeightedEmbedDispatcher>,
    pub(super) transcribe_queue: Arc<WorkQueue<TranscribeJob>>,
    pub(super) synthesize_queue: Arc<WorkQueue<SynthesizeJob>>,
    pub(super) generate_queue: Arc<WorkQueue<GenerateJob>>,
    pub(super) generate_stream_queue: Arc<WorkQueue<GenerateStreamJob>>,
    pub(super) rerank_queue: Arc<WorkQueue<RerankJob>>,
    pub(super) metrics: Arc<QueueMetrics>,

    /// Stable identity of all embedding providers eligible for this lane.
    pub(super) embedding_space_fingerprint: Option<Arc<str>>,

    // Worker counts per capability — presence is derived as `count > 0`.
    pub(super) embedding_workers: usize,
    pub(super) transcription_workers: usize,
    pub(super) tts_workers: usize,
    pub(super) llm_workers: usize,
    pub(super) reranking_workers: usize,
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
    #[instrument(
        skip(self, text),
        fields(text_len, pending_jobs, selected_worker_id, duration_us)
    )]
    pub async fn embed(&self, text: impl Into<String>) -> InferenceResult<Vec<f32>> {
        self.submit_embed(text).await
    }

    /// Submit an embedding job with a new cancellation token.
    pub fn submit_embed(&self, text: impl Into<String>) -> InferenceJob<Vec<f32>> {
        self.submit_embed_with_cancellation(text, CancellationToken::new())
    }

    /// Submit an embedding job governed by an existing parent token.
    pub fn submit_embed_with_cancellation(
        &self,
        text: impl Into<String>,
        cancellation: CancellationToken,
    ) -> InferenceJob<Vec<f32>> {
        let span = tracing::Span::current();
        let t0 = std::time::Instant::now();
        let job = submit_one_shot(
            self.embedding_workers > 0,
            "embedding",
            cancellation,
            Arc::clone(&self.metrics),
            |context, response| {
                let text = text.into();
                span.record("text_len", text.len());
                span.record("pending_jobs", self.embed_dispatcher.pending());
                EmbedJob {
                    context,
                    text,
                    response,
                }
            },
            |job| {
                let worker_id = self.embed_dispatcher.submit(job);
                span.record("selected_worker_id", worker_id);
            },
        );
        span.record("duration_us", t0.elapsed().as_micros() as u64);
        job
    }

    /// Stable provider-set identity used to prevent mixing vector spaces.
    pub fn embedding_space_fingerprint(&self) -> Option<&str> {
        self.embedding_space_fingerprint.as_deref()
    }

    /// Submit a batch of texts for embedding.
    ///
    /// Submissions are pipelined with a concurrency cap of `embedding_workers * 2`
    /// so bulk imports don't materialise every pending future at once.  Results are
    /// returned in input order.
    pub async fn embed_many(&self, texts: Vec<String>) -> InferenceResult<Vec<Vec<f32>>> {
        self.embed_many_with_cancellation(texts, CancellationToken::new())
            .await
    }

    /// Embed a bounded fan-out under one parent cancellation token.
    pub async fn embed_many_with_cancellation(
        &self,
        texts: Vec<String>,
        cancellation: CancellationToken,
    ) -> InferenceResult<Vec<Vec<f32>>> {
        if self.embedding_workers == 0 {
            return Err(InferenceError::CapabilityUnavailable {
                capability: "embedding",
            });
        }

        use futures::{StreamExt, TryStreamExt};
        let concurrency = (self.embedding_workers * 2).max(4);
        futures::stream::iter(texts)
            .map(|text| {
                let q = self.clone();
                let cancellation = cancellation.clone();
                async move { q.submit_embed_with_cancellation(text, cancellation).await }
            })
            .buffered(concurrency)
            .try_collect()
            .await
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
    ///   (e.g. `"session.wav"`).  See [`mime_for_filename`](crate::ai::transcription::mime_for_filename).
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
    ) -> InferenceResult<String> {
        self.submit_transcribe(audio_bytes, filename).await
    }

    pub fn submit_transcribe(
        &self,
        audio_bytes: Vec<u8>,
        filename: impl Into<String>,
    ) -> InferenceJob<String> {
        self.submit_transcribe_with_cancellation(audio_bytes, filename, CancellationToken::new())
    }

    pub fn submit_transcribe_with_cancellation(
        &self,
        audio_bytes: Vec<u8>,
        filename: impl Into<String>,
        cancellation: CancellationToken,
    ) -> InferenceJob<String> {
        submit_one_shot(
            self.transcription_workers > 0,
            "transcription",
            cancellation,
            Arc::clone(&self.metrics),
            |context, response| {
                let filename = filename.into();
                tracing::Span::current().record("filename", &filename);
                tracing::Span::current().record("audio_bytes_len", audio_bytes.len());
                TranscribeJob {
                    context,
                    audio_bytes,
                    filename,
                    response,
                }
            },
            |job| self.transcribe_queue.push(job),
        )
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
    ) -> InferenceResult<Vec<u8>> {
        self.submit_synthesize(text, voice).await
    }

    pub fn submit_synthesize(
        &self,
        text: impl Into<String>,
        voice: Option<KokoroVoice>,
    ) -> InferenceJob<Vec<u8>> {
        self.submit_synthesize_with_cancellation(text, voice, CancellationToken::new())
    }

    pub fn submit_synthesize_with_cancellation(
        &self,
        text: impl Into<String>,
        voice: Option<KokoroVoice>,
        cancellation: CancellationToken,
    ) -> InferenceJob<Vec<u8>> {
        submit_one_shot(
            self.tts_workers > 0,
            "TTS",
            cancellation,
            Arc::clone(&self.metrics),
            |context, response| {
                let text = text.into();
                if let Some(ref voice) = voice {
                    tracing::Span::current().record("voice", voice.as_str());
                }
                tracing::Span::current().record("text_len", text.len());
                SynthesizeJob {
                    context,
                    text,
                    voice,
                    response,
                }
            },
            |job| self.synthesize_queue.push(job),
        )
    }

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
    pub async fn generate(&self, request: ChatRequest) -> InferenceResult<ChatCompletionResponse> {
        self.submit_generate(request).await
    }

    pub fn submit_generate(&self, request: ChatRequest) -> InferenceJob<ChatCompletionResponse> {
        self.submit_generate_with_cancellation(request, CancellationToken::new())
    }

    pub fn submit_generate_with_cancellation(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> InferenceJob<ChatCompletionResponse> {
        submit_one_shot(
            self.llm_workers > 0,
            "text-generation",
            cancellation,
            Arc::clone(&self.metrics),
            |context, response| {
                tracing::Span::current().record("n_messages", request.messages.len());
                GenerateJob {
                    context,
                    request,
                    response,
                }
            },
            |job| self.generate_queue.push(job),
        )
    }

    /// Submit a streaming LLM request; returns an mpsc receiver that yields
    /// text deltas as the model generates them.
    ///
    /// The selected worker remains occupied through stream completion or
    /// receiver cancellation.
    ///
    /// # Errors
    ///
    /// Returns an error immediately if no LLM-capable device is registered.
    /// Stream-level errors are sent as `Err(_)` items through the receiver.
    pub fn generate_stream(
        &self,
        request: ChatRequest,
    ) -> InferenceResult<mpsc::Receiver<InferenceResult<StreamToken>>> {
        Ok(self.submit_generate_stream(request)?.stream)
    }

    pub fn submit_generate_stream(
        &self,
        request: ChatRequest,
    ) -> InferenceResult<StreamingInferenceJob<StreamToken>> {
        self.submit_generate_stream_with_cancellation(request, CancellationToken::new())
    }

    pub fn submit_generate_stream_with_cancellation(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> InferenceResult<StreamingInferenceJob<StreamToken>> {
        if self.llm_workers == 0 {
            self.metrics.unavailable();
            return Err(InferenceError::CapabilityUnavailable {
                capability: "text-generation",
            });
        }
        let (tx, rx) = mpsc::channel(64);
        let (completion_tx, completion_rx) = oneshot::channel();
        self.generate_stream_queue.push(GenerateStreamJob {
            context: JobContext::new(cancellation.clone(), Arc::clone(&self.metrics)),
            request,
            response: tx,
            completion: completion_tx,
        });
        Ok(StreamingInferenceJob {
            completion: JobCompletion::from_receiver(
                cancellation.clone(),
                completion_rx,
                Some(Arc::clone(&self.metrics)),
            ),
            cancellation,
            stream: rx,
        })
    }

    /// Convenience wrapper: submit a single-turn user prompt and return the
    /// assistant's reply text.
    pub async fn ask(&self, prompt: impl Into<String>) -> anyhow::Result<String> {
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
    ) -> InferenceResult<Vec<RerankDocument>> {
        self.submit_rerank(query, documents, top_n).await
    }

    pub fn submit_rerank(
        &self,
        query: impl Into<String>,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> InferenceJob<Vec<RerankDocument>> {
        self.submit_rerank_with_cancellation(query, documents, top_n, CancellationToken::new())
    }

    pub fn submit_rerank_with_cancellation(
        &self,
        query: impl Into<String>,
        documents: Vec<String>,
        top_n: Option<usize>,
        cancellation: CancellationToken,
    ) -> InferenceJob<Vec<RerankDocument>> {
        submit_one_shot(
            self.reranking_workers > 0,
            "reranking",
            cancellation,
            Arc::clone(&self.metrics),
            |context, response| {
                let query = query.into();
                tracing::Span::current().record("n_docs", documents.len());
                if let Some(n) = top_n {
                    tracing::Span::current().record("top_n", n);
                }
                RerankJob {
                    context,
                    query,
                    documents,
                    top_n,
                    response,
                }
            },
            |job| self.rerank_queue.push(job),
        )
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    /// Returns the current number of pending jobs for each capability type.
    pub fn stats(&self) -> QueueStats {
        QueueStats {
            pending_embeddings: self.embed_dispatcher.pending(),
            pending_transcriptions: self.transcribe_queue.pending(),
            pending_syntheses: self.synthesize_queue.pending(),
            pending_generations: self.generate_queue.pending(),
            pending_generation_streams: self.generate_stream_queue.pending(),
            pending_rerankings: self.rerank_queue.pending(),
            counters: self.metrics.snapshot(),
        }
    }

    /// Whether any embedding-capable worker is registered.
    pub fn has_embedding(&self) -> bool {
        self.embedding_workers > 0
    }

    /// Whether any transcription-capable worker is registered.
    pub fn has_transcription(&self) -> bool {
        self.transcription_workers > 0
    }

    /// Whether any TTS-capable worker is registered.
    pub fn has_tts(&self) -> bool {
        self.tts_workers > 0
    }

    /// Whether any LLM-capable worker is registered.
    pub fn has_text_generation(&self) -> bool {
        self.llm_workers > 0
    }

    /// Whether any reranking-capable worker is registered.
    pub fn has_reranking(&self) -> bool {
        self.reranking_workers > 0
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
            .field("embedding_workers", &self.embedding_workers)
            .field("transcription_workers", &self.transcription_workers)
            .field("tts_workers", &self.tts_workers)
            .field("llm_workers", &self.llm_workers)
            .field("reranking_workers", &self.reranking_workers)
            .finish()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use anyhow::Result;
    use tokio::sync::Semaphore;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::ai::transcription::TranscriptionProvider;
    use crate::test_helpers::require_integration_url;

    use super::super::builder::InferenceQueueBuilder;
    use super::super::jobs::{TranscribeJob, WorkQueue};
    use super::super::weighted::WeightedEmbedDispatcher;
    use super::super::workers::{run_embed_worker, run_transcribe_worker};
    use super::*;

    /// Build a minimal valid mono 16-bit 16 kHz PCM WAV file of silence.
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
        buf.extend(std::iter::repeat_n(0u8, data_size as usize));
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

    struct BlockingEmbeddingProvider {
        calls: Arc<AtomicUsize>,
        started: Arc<Semaphore>,
        release: Arc<Semaphore>,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for BlockingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.add_permits(1);
            let permit = self.release.acquire().await.unwrap();
            permit.forget();
            Ok(vec![0.0; MOCK_DIMS])
        }

        async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            let mut output = Vec::with_capacity(texts.len());
            for text in texts {
                output.push(self.embed(&text).await?);
            }
            Ok(output)
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

    struct FailingEmbeddingProvider {
        calls: Arc<AtomicUsize>,
        started: Arc<Semaphore>,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for FailingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.add_permits(1);
            anyhow::bail!("deterministic transient failure")
        }

        async fn embed_batch(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            unreachable!()
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

    fn build_embedding_queue(provider: Arc<dyn EmbeddingProvider>) -> InferenceQueue {
        InferenceQueueBuilder::new()
            .with_provider(crate::lemonade::BuiltProvider {
                name: "deterministic/mock".to_string(),
                capability: crate::lemonade::Capability::Embedding,
                provider: crate::lemonade::ProviderSlot::Embedding(provider),
                weight: 100,
            })
            .build()
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
        //
        // Two-phase pattern (mirrors InferenceQueueBuilder::build):
        // 1. Register all embed workers with the dispatcher.
        // 2. Wrap dispatcher in Arc, then spawn tasks with the Arc.
        let mut embed_dispatcher = WeightedEmbedDispatcher::new();
        let transcribe_queue = Arc::new(WorkQueue::<TranscribeJob>::new());
        let synthesize_queue = Arc::new(WorkQueue::new());

        let provider: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbeddingProvider);
        let (embed_q, embed_ewma) = embed_dispatcher.add_worker(100, "mock-npu");

        // Wrap before spawning so the worker can call steal_from_busiest.
        let embed_dispatcher = Arc::new(embed_dispatcher);
        {
            let dispatcher = Arc::clone(&embed_dispatcher);
            tokio::spawn(async move {
                run_embed_worker(
                    embed_q,
                    provider,
                    "mock-npu".to_string(),
                    embed_ewma,
                    dispatcher,
                )
                .await;
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
            embed_dispatcher,
            transcribe_queue,
            synthesize_queue,
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: Some(Arc::from("mock@768")),
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
            builder.providers.is_empty(),
            "New builder should have no providers registered"
        );
    }

    #[tokio::test]
    async fn builder_derives_embedding_registration_output() {
        let queue = build_embedding_queue(Arc::new(MockEmbeddingProvider));

        assert_eq!(queue.embedding_worker_count(), 1);
        assert_eq!(
            queue.embedding_space_fingerprint(),
            Some("deterministic/mock@unknown")
        );
    }

    #[tokio::test]
    async fn builder_skips_mismatched_capability_slots() {
        let queue = InferenceQueueBuilder::new()
            .with_provider(crate::lemonade::BuiltProvider {
                name: "mismatched/mock".into(),
                capability: crate::lemonade::Capability::Transcription,
                provider: crate::lemonade::ProviderSlot::Embedding(Arc::new(MockEmbeddingProvider)),
                weight: 100,
            })
            .build();

        assert_eq!(queue.embedding_worker_count(), 0);
        assert_eq!(queue.transcription_worker_count(), 0);
        assert_eq!(queue.embedding_space_fingerprint(), None);
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
    async fn cancelled_pending_job_never_invokes_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let queue = build_embedding_queue(Arc::new(BlockingEmbeddingProvider {
            calls: calls.clone(),
            started: started.clone(),
            release: release.clone(),
        }));

        let first = queue.submit_embed("active");
        started.acquire().await.unwrap().forget();
        let second = queue.submit_embed("pending");
        second.cancel();
        assert!(matches!(second.await, Err(InferenceError::Cancelled)));
        release.add_permits(1);
        first.await.unwrap();
        tokio::task::yield_now().await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let counters = queue.stats().counters;
        assert_eq!(counters.succeeded, 1);
        assert_eq!(counters.cancelled_pending, 1);
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_backoff() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let queue = build_embedding_queue(Arc::new(FailingEmbeddingProvider {
            calls: calls.clone(),
            started: started.clone(),
        }));

        let job = queue.submit_embed("retrying");
        started.acquire().await.unwrap().forget();
        job.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), job)
            .await
            .expect("retry backoff did not react to cancellation");
        assert!(matches!(result, Err(InferenceError::Cancelled)));
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.stats().counters.retries, 1);
    }

    #[tokio::test]
    async fn cancellation_interrupts_active_provider_future() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let queue = build_embedding_queue(Arc::new(BlockingEmbeddingProvider {
            calls: calls.clone(),
            started: started.clone(),
            release: Arc::new(Semaphore::new(0)),
        }));

        let job = queue.submit_embed("active");
        started.acquire().await.unwrap().forget();
        job.cancel();
        assert!(matches!(job.await, Err(InferenceError::Cancelled)));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(queue.stats().counters.cancelled_active, 1);
    }

    #[tokio::test]
    async fn parent_cancellation_stops_embedding_fanout() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let cancellation = CancellationToken::new();
        let queue = build_embedding_queue(Arc::new(BlockingEmbeddingProvider {
            calls: calls.clone(),
            started: started.clone(),
            release: Arc::new(Semaphore::new(0)),
        }));
        let task = tokio::spawn({
            let queue = queue.clone();
            let cancellation = cancellation.clone();
            async move {
                queue
                    .embed_many_with_cancellation(
                        vec!["a".into(), "b".into(), "c".into(), "d".into()],
                        cancellation,
                    )
                    .await
            }
        });
        started.acquire().await.unwrap().forget();
        cancellation.cancel();
        assert!(matches!(
            task.await.unwrap(),
            Err(InferenceError::Cancelled)
        ));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stats_remain_consistent_during_concurrent_completion_and_cancellation() {
        let queue = build_mock_queue();
        let mut jobs = Vec::new();
        for index in 0..200 {
            let job = queue.submit_embed(format!("job-{index}"));
            if index % 3 == 0 {
                job.cancel();
            }
            jobs.push(job);
        }
        futures::future::join_all(jobs).await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let counters = queue.stats().counters;
                if counters.started + counters.cancelled_pending == counters.submitted {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let counters = queue.stats().counters;
        assert_eq!(counters.submitted, 200);
        assert_eq!(
            counters.started,
            counters.succeeded + counters.provider_failed + counters.cancelled_active
        );
        assert_eq!(counters.queue_wait.samples, counters.started);
        assert_eq!(counters.service_time.samples, counters.started);
        assert_eq!(
            counters.submitted,
            counters.started + counters.cancelled_pending
        );
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
    async fn unavailable_stream_submission_is_accounted_once() {
        let queue = build_mock_queue();
        let request = ChatRequest::new(vec![crate::lemonade::ChatMessage::user("test")]);

        assert!(matches!(
            queue.submit_generate_stream(request),
            Err(InferenceError::CapabilityUnavailable {
                capability: "text-generation"
            })
        ));
        let counters = queue.stats().counters;
        assert_eq!(counters.submitted, 1);
        assert_eq!(counters.unavailable, 1);
        assert_eq!(counters.started, 0);
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: None,
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: None,
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
            async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
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

        let mut embed_dispatcher = WeightedEmbedDispatcher::new();
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(SlowProvider);
        let (q, ewma) = embed_dispatcher.add_worker(100, "slow-npu");
        let embed_dispatcher = Arc::new(embed_dispatcher);
        {
            let dispatcher = Arc::clone(&embed_dispatcher);
            tokio::spawn(async move {
                run_embed_worker(q, provider, "slow-npu".to_string(), ewma, dispatcher).await;
            });
        }

        let queue = InferenceQueue {
            embed_dispatcher,
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: Some(Arc::from("slow@768")),
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: Some(Arc::from("mock@768")),
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
            debug.contains("embedding_workers: 1"),
            "Debug must show embedding worker count"
        );
        assert!(
            debug.contains("transcription_workers: 2"),
            "Debug must show worker counts"
        );
    }

    #[test]
    fn test_worker_count_accessors() {
        let q = InferenceQueue {
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: Some(Arc::from("mock@768")),
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
            transcribe_queue: Arc::new(WorkQueue::new()),
            synthesize_queue: Arc::new(WorkQueue::new()),
            generate_queue: Arc::new(WorkQueue::new()),
            generate_stream_queue: Arc::new(WorkQueue::new()),
            rerank_queue: Arc::new(WorkQueue::new()),
            metrics: Arc::new(QueueMetrics::default()),
            embedding_space_fingerprint: Some(Arc::from("mock@768")),
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
    async fn test_queue_embed_via_provider_factory() {
        let url = require_integration_url!();

        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url)
            .await
            .unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let embed_sel = selector
            .select_embedding_models()
            .into_iter()
            .next()
            .expect("No embedding model found in catalog");
        let already_loaded: Vec<String> = catalog
            .loaded
            .iter()
            .map(|m| m.model_name.clone())
            .collect();
        let built = crate::lemonade::ProviderFactory::build(
            &embed_sel,
            crate::lemonade::Capability::Embedding,
            &url,
            100,
            None,
            &already_loaded,
        )
        .await
        .expect("Failed to build embedding provider");

        let queue = InferenceQueueBuilder::new().with_provider(built).build();

        let vec = queue
            .embed("The foundation stood for a thousand years")
            .await;
        assert!(vec.is_ok(), "embed() failed: {:?}", vec.err());
        let vec = vec.unwrap();
        assert!(!vec.is_empty(), "Expected non-empty embedding");
        assert!(
            vec.iter().all(|&x: &f32| x.is_finite()),
            "Embedding contains non-finite values"
        );
    }

    async fn build_live_transcription_queue_for_recipe(
        url: &str,
        recipe: &str,
        workers: usize,
    ) -> Option<InferenceQueue> {
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url)
            .await
            .unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let Some(selected) = selector
            .select_stt_models()
            .into_iter()
            .find(|model| model.recipe == recipe)
        else {
            eprintln!("SKIP: No {recipe} transcription model available in catalog");
            return None;
        };
        let already_loaded: Vec<String> = catalog
            .loaded
            .iter()
            .map(|m| m.model_name.clone())
            .collect();

        let mut builder = InferenceQueueBuilder::new();
        for index in 0..workers {
            let built = crate::lemonade::ProviderFactory::build(
                &selected,
                crate::lemonade::Capability::Transcription,
                url,
                100,
                None,
                &already_loaded,
            )
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "Failed to build {recipe} transcription provider {index} for '{}': {err}",
                    selected.model_id
                )
            });
            builder = builder.with_provider(built);
        }

        Some(builder.build())
    }

    #[tokio::test]
    async fn test_queue_transcribe_via_provider_factory_flm() {
        let url = require_integration_url!();
        let Some(queue) = build_live_transcription_queue_for_recipe(&url, "flm", 1).await else {
            return;
        };

        let wav = make_test_silence_wav(1.0);
        let result = queue.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "FLM transcribe() failed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_queue_transcribe_via_provider_factory_llamacpp() {
        let url = require_integration_url!();
        let Some(queue) = build_live_transcription_queue_for_recipe(&url, "llamacpp", 1).await
        else {
            return;
        };

        let wav = make_test_silence_wav(1.0);
        let result = queue.transcribe(wav, "silence.wav").await;
        assert!(
            result.is_ok(),
            "llamacpp transcribe() failed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_queue_two_transcription_workers_compete() {
        let url = require_integration_url!();
        let Some(queue) = build_live_transcription_queue_for_recipe(&url, "flm", 2).await else {
            return;
        };

        assert_eq!(
            queue.transcription_worker_count(),
            2,
            "Expected 2 transcription workers"
        );

        let wav = make_test_silence_wav(0.5);

        let (r1, r2) = tokio::join!(
            queue.transcribe(wav.clone(), "a.wav"),
            queue.transcribe(wav.clone(), "b.wav"),
        );
        assert!(r1.is_ok(), "Job 1 failed: {:?}", r1.err());
        assert!(r2.is_ok(), "Job 2 failed: {:?}", r2.err());
    }
}
