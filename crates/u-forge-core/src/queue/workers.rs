//! Background worker loop implementations — one per (device, capability) pair.
//!
//! Loop invariant (race-free wakeup):
//!
//!   1. Create `notified` future — registers a permit listener BEFORE the
//!      deque is checked.
//!   2. Try to pop a job.
//!   3a. If a job is found: drop the `notified` future, process the job.
//!   3b. If no job: `.await` the `notified` future.  Wakes immediately if
//!       `notify_one()` was called between steps 1 and 3b.
//!
//! This is the canonical race-free pattern from the Tokio `Notify` docs.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tracing::debug;

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use crate::lemonade::{LemonadeChatProvider, LemonadeRerankProvider, LemonadeSttProvider, LemonadeTtsProvider};

use super::jobs::{EmbedJob, GenerateJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue};

/// Maximum number of attempts for a single embed job before the error is
/// returned to the caller.  Retries guard against transient server hiccups
/// (e.g. a Lemonade instance that is momentarily swapping a model in/out).
const EMBED_MAX_ATTEMPTS: u32 = 3;

/// Base delay before the first retry.  Doubles on each subsequent attempt
/// (100 ms → 200 ms) so three attempts add at most ~300 ms of backoff.
const EMBED_RETRY_BASE_MS: u64 = 100;

/// LLM generation worker — services both GPU llamacpp and NPU FLM chat providers.
///
/// When `provider.gpu` is `Some`, the [`GpuResourceManager`] inside the provider
/// handles GPU locking automatically inside `complete()`.  When `None` (NPU), the
/// call goes directly to the FLM model with no locking.
pub(super) async fn run_llm_worker(
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

pub(super) async fn run_rerank_worker(
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

/// Embedding worker loop.
///
/// `idle` is an `AtomicBool` shared with the [`WeightedEmbedDispatcher`](super::weighted::WeightedEmbedDispatcher).
/// The worker sets it `true` before sleeping (so the dispatcher can see that it
/// is free) and `false` immediately after popping a job.  The window between
/// the `false` store and the actual pop is negligible; the window between job
/// completion and the `true` store is also small (just before `notified.await`).
/// Both races are acceptable — they cause a job to go to a slightly non-optimal
/// worker, never to be lost.
pub(super) async fn run_embed_worker(
    queue: Arc<WorkQueue<EmbedJob>>,
    provider: Arc<dyn EmbeddingProvider>,
    device_name: String,
    idle: Arc<AtomicBool>,
) {
    loop {
        // Step 1: register before checking
        let notified = queue.notify.notified();

        // Step 2: try to pop
        if let Some(job) = queue.try_pop() {
            idle.store(false, Ordering::Relaxed);
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
            // Step 3b: nothing in queue — mark idle then sleep until notified.
            idle.store(true, Ordering::Relaxed);
            notified.await;
        }
    }
}

pub(super) async fn run_transcribe_worker(
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
pub(super) async fn run_gpu_stt_worker(
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

pub(super) async fn run_tts_worker(
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
