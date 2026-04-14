//! Background worker loop implementations — one per (device, capability) pair.
//!
//! Loop invariant (race-free wakeup):
//!
//!   1. Create `notified` future — registers a permit listener BEFORE the
//!      deque is checked.
//!   2. Try to pop a job.
//!   3. If a job is found: drop the `notified` future, process the job.
//!      If no job: `.await` the `notified` future. Wakes immediately if
//!      `notify_one()` was called between steps 1 and 3.
//!
//! This is the canonical race-free pattern from the Tokio `Notify` docs.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use tracing::debug;

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use crate::lemonade::{LemonadeChatProvider, LemonadeRerankProvider, LemonadeTtsProvider};

use super::jobs::{EmbedJob, GenerateJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue};
use super::weighted::WeightedEmbedDispatcher;

/// Maximum number of attempts for a single embed job before the error is
/// returned to the caller.  Retries guard against transient server hiccups
/// (e.g. a Lemonade instance that is momentarily swapping a model in/out).
const EMBED_MAX_ATTEMPTS: u32 = 3;

/// Base delay before the first retry.  Doubles on each subsequent attempt
/// (100 ms → 200 ms) so three attempts add at most ~300 ms of backoff.
const EMBED_RETRY_BASE_MS: u64 = 100;

/// Generic single-consumer worker loop shared by all non-embedding workers.
///
/// On each iteration:
/// 1. Register a `notified` future BEFORE checking the queue (race-free).
/// 2. Pop a job if one is ready, drop the future, and call `process`.
/// 3. If the queue is empty, sleep until the queue is notified.
async fn run_worker_loop<J, F, Fut>(queue: Arc<WorkQueue<J>>, process: F)
where
    J: Send,
    F: Fn(J) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    loop {
        let notified = queue.notify.notified();
        if let Some(job) = queue.try_pop() {
            drop(notified);
            process(job).await;
        } else {
            notified.await;
        }
    }
}

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
    run_worker_loop(queue, move |job| {
        let provider = provider.clone();
        let device_name = device_name.clone();
        async move {
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
        }
    })
    .await;
}

pub(super) async fn run_rerank_worker(
    queue: Arc<WorkQueue<RerankJob>>,
    provider: LemonadeRerankProvider,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let provider = provider.clone();
        let device_name = device_name.clone();
        async move {
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
        }
    })
    .await;
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
/// Execute a single embedding job: retry loop, EWMA update, send result.
async fn execute_embed_job(
    job: EmbedJob,
    provider: &Arc<dyn EmbeddingProvider>,
    device_name: &str,
    ewma_us: &Arc<AtomicU64>,
) {
    let start = std::time::Instant::now();
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

    let elapsed_us = start.elapsed().as_micros() as u64;
    debug!(
        device = %device_name,
        text_len = job.text.len(),
        ok = final_result.is_ok(),
        duration_ms = elapsed_us / 1000,
        "Embed job complete"
    );

    // Update EWMA (α = 0.5): first sample is used directly; subsequent samples
    // converge quickly to the actual device latency.
    let old = ewma_us.load(Ordering::Relaxed);
    let new_ewma = if old == 0 {
        elapsed_us
    } else {
        old / 2 + elapsed_us / 2
    };
    ewma_us.store(new_ewma, Ordering::Relaxed);

    let _ = job.response.send(final_result);
}

/// Embedding worker loop with work stealing.
///
/// On each iteration:
/// 1. Check own queue.
/// 2. If empty, try to steal from the most-loaded other worker.
/// 3. If still nothing, sleep until either the per-queue Notify or the
///    dispatcher's global Notify fires — whichever comes first.
///
/// The global Notify fires on every `submit()`, so an idle worker wakes
/// immediately when work lands in any queue (including a slow neighbour's).
/// Once awake, the steal loop keeps the worker busy until all queues are
/// empty, eliminating the "GPU idle while NPU backlog burns" scenario.
pub(super) async fn run_embed_worker(
    queue: Arc<WorkQueue<EmbedJob>>,
    provider: Arc<dyn EmbeddingProvider>,
    device_name: String,
    idle: Arc<AtomicBool>,
    ewma_us: Arc<AtomicU64>,
    dispatcher: Arc<WeightedEmbedDispatcher>,
) {
    loop {
        // Register interest in both notifiers BEFORE any queue checks so we
        // cannot miss a wakeup that fires between checking and sleeping.
        let local_notified = queue.notify.notified();
        let global_notified = dispatcher.global_notify.notified();

        // Own queue first.
        if let Some(job) = queue.try_pop() {
            idle.store(false, Ordering::Relaxed);
            execute_embed_job(job, &provider, &device_name, &ewma_us).await;
            continue;
        }

        // Try to steal from the most-loaded other worker.  This drains
        // backlogged neighbours without any additional synchronisation.
        if let Some(job) = dispatcher.steal_from_busiest(&queue) {
            idle.store(false, Ordering::Relaxed);
            debug!(device = %device_name, "Work-stealing embed job from neighbour queue");
            execute_embed_job(job, &provider, &device_name, &ewma_us).await;
            continue;
        }

        // Nothing to do — sleep until our queue or any other queue gets work.
        idle.store(true, Ordering::Relaxed);
        tokio::select! {
            _ = local_notified => {}
            _ = global_notified => {}
        }
    }
}

pub(super) async fn run_transcribe_worker(
    queue: Arc<WorkQueue<TranscribeJob>>,
    provider: Arc<dyn TranscriptionProvider>,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let provider = Arc::clone(&provider);
        let device_name = device_name.clone();
        async move {
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
        }
    })
    .await;
}


pub(super) async fn run_tts_worker(
    queue: Arc<WorkQueue<SynthesizeJob>>,
    tts: LemonadeTtsProvider,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let tts = tts.clone();
        let device_name = device_name.clone();
        async move {
            let start = std::time::Instant::now();
            let result = match &job.voice {
                Some(voice) => tts.synthesize(&job.text, Some(voice)).await,
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
        }
    })
    .await;
}
