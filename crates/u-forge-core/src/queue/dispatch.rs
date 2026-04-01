//! [`InferenceQueue`] struct, its public API, and its [`QueueStats`] snapshot type.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::oneshot;
use tracing::instrument;

use crate::lemonade::{ChatCompletionResponse, ChatRequest, KokoroVoice, RerankDocument};

use super::jobs::{EmbedJob, GenerateJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue};
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
    /// Jobs waiting to be picked up by a reranking worker.
    pub pending_rerankings: usize,
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
    pub(super) rerank_queue: Arc<WorkQueue<RerankJob>>,

    // Capability presence flags — avoids dynamic trait-object dispatch for
    // the fast "no device registered" error path.
    pub(super) has_embedding: bool,
    pub(super) has_transcription: bool,
    pub(super) has_tts: bool,
    pub(super) has_text_generation: bool,
    pub(super) has_reranking: bool,

    // Worker counts per capability (informational).
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
        self.embed_dispatcher.submit(EmbedJob { text, response: tx });

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
    /// [`embed_batch`](crate::ai::embeddings::EmbeddingProvider::embed_batch) call
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
            pending_embeddings: self.embed_dispatcher.pending(),
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::ai::transcription::TranscriptionProvider;
    use crate::hardware::npu::NpuDevice;
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
        //
        // Two-phase pattern (mirrors InferenceQueueBuilder::build):
        // 1. Register all embed workers with the dispatcher.
        // 2. Wrap dispatcher in Arc, then spawn tasks with the Arc.
        let mut embed_dispatcher = WeightedEmbedDispatcher::new();
        let transcribe_queue = Arc::new(WorkQueue::<TranscribeJob>::new());
        let synthesize_queue = Arc::new(WorkQueue::new());

        let provider: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbeddingProvider);
        let (embed_q, embed_idle, embed_ewma) = embed_dispatcher.add_worker(100, "mock-npu");

        // Wrap before spawning so the worker can call steal_from_busiest.
        let embed_dispatcher = Arc::new(embed_dispatcher);
        {
            let dispatcher = Arc::clone(&embed_dispatcher);
            tokio::spawn(async move {
                run_embed_worker(
                    embed_q, provider, "mock-npu".to_string(), embed_idle, embed_ewma, dispatcher,
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
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
        let (q, idle, ewma) = embed_dispatcher.add_worker(100, "slow-npu");
        let embed_dispatcher = Arc::new(embed_dispatcher);
        {
            let dispatcher = Arc::clone(&embed_dispatcher);
            tokio::spawn(async move {
                run_embed_worker(q, provider, "slow-npu".to_string(), idle, ewma, dispatcher).await;
            });
        }

        let queue = InferenceQueue {
            embed_dispatcher,
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
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
            embed_dispatcher: Arc::new(WeightedEmbedDispatcher::new()),
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
        let url = require_integration_url!();

        let npu = NpuDevice::embedding_only(&url, None, None)
            .await
            .expect("NpuDevice construction failed");

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
        let url = require_integration_url!();

        let npu = NpuDevice::new(&url, None, None, None)
            .await
            .expect("NpuDevice construction failed");

        let queue = InferenceQueueBuilder::new().with_npu_device(npu).build();

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
        let url = require_integration_url!();

        let npu1 = NpuDevice::new(&url, None, None, None)
            .await
            .expect("NpuDevice 1 construction failed");
        let npu2 = NpuDevice::new(&url, None, None, None)
            .await
            .expect("NpuDevice 2 construction failed");

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

        let (r1, r2) = tokio::join!(
            queue.transcribe(wav.clone(), "a.wav"),
            queue.transcribe(wav.clone(), "b.wav"),
        );
        assert!(r1.is_ok(), "Job 1 failed: {:?}", r1.err());
        assert!(r2.is_ok(), "Job 2 failed: {:?}", r2.err());
    }
}
