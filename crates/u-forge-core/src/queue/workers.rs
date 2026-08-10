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
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tracing::{Instrument, debug};

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use crate::lemonade::{
    CoordinatedChatProvider, LemonadeTtsProvider, ReasoningPolicy, RerankProvider,
};

use super::jobs::{
    EmbedJob, GenerateJob, GenerateStreamJob, RerankJob, SynthesizeJob, TranscribeJob, WorkQueue,
};
use super::lifecycle::{InferenceError, TimeoutClass};
use super::weighted::WeightedEmbedDispatcher;

/// Maximum number of attempts for a single embed job before the error is
/// returned to the caller.  Retries guard against transient server hiccups
/// (e.g. a Lemonade instance that is momentarily swapping a model in/out).
const EMBED_MAX_ATTEMPTS: u32 = 3;

/// Base delay before the first retry.  Doubles on each subsequent attempt
/// (100 ms → 200 ms) so three attempts add at most ~300 ms of backoff.
const EMBED_RETRY_BASE_MS: u64 = 100;

fn queue_job_span(
    context: &super::jobs::JobContext,
    capability: &'static str,
    device_name: &str,
    stolen: bool,
) -> tracing::Span {
    tracing::info_span!(
        "inference_queue_job",
        job_id = context.id,
        capability,
        selected_worker = device_name,
        stolen,
        queue_wait_us = context.enqueued_at.elapsed().as_micros() as u64,
        service_time_us = tracing::field::Empty,
        retries = 0_u32,
        cancellation_point = tracing::field::Empty,
        timeout_class = tracing::field::Empty,
        outcome = tracing::field::Empty,
    )
}

fn finish_queue_span<T>(span: &tracing::Span, result: &Result<T, InferenceError>, service_us: u64) {
    span.record("service_time_us", service_us);
    let (outcome, cancellation_point, timeout_class) = match result {
        Ok(_) => ("succeeded", None, None),
        Err(InferenceError::Cancelled) => ("cancelled", Some("active_provider"), None),
        Err(InferenceError::Superseded) => ("superseded", Some("active_provider"), None),
        Err(InferenceError::TimedOut { class }) => (
            "timed_out",
            Some("active_provider"),
            Some(class.to_string()),
        ),
        Err(InferenceError::ProviderFailed { .. }) => ("provider_failed", None, None),
        Err(InferenceError::WorkerDropped) => ("worker_dropped", None, None),
        Err(InferenceError::CapabilityUnavailable { .. }) => ("unavailable", None, None),
    };
    span.record("outcome", outcome);
    if let Some(point) = cancellation_point {
        span.record("cancellation_point", point);
    }
    if let Some(class) = timeout_class {
        span.record("timeout_class", class);
    }
}

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
    provider: CoordinatedChatProvider,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let provider = provider.clone();
        let device_name = device_name.clone();
        async move {
            if job.context.cancellation.is_cancelled() {
                let error = job.context.cancellation.error();
                job.context.metrics.cancelled_pending(&error);
                let _ = job.response.send(Err(error));
                return;
            }
            let span = queue_job_span(&job.context, "text_generation", &device_name, false);
            let start = job.context.begin(false);
            let n_messages = job.request.messages.len();
            let result = async {
                tokio::select! {
                    _ = job.context.cancellation.cancelled() => {
                        Err(job.context.cancellation.error())
                    }
                    result = complete_coordinated(&provider, job.request) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(span.clone())
            .await;
            finish_queue_span(&span, &result, start.elapsed().as_micros() as u64);
            job.context.metrics.finished(
                &result.as_ref().map(|_| ()).map_err(|error| error),
                start.elapsed().as_micros() as u64,
            );
            debug!(
                job_id = job.context.id,
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

pub(super) async fn run_llm_stream_worker(
    queue: Arc<WorkQueue<GenerateStreamJob>>,
    provider: CoordinatedChatProvider,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let provider = provider.clone();
        let device_name = device_name.clone();
        async move {
            if job.context.cancellation.is_cancelled() {
                let error = job.context.cancellation.error();
                job.context.metrics.cancelled_pending(&error);
                let _ = job.completion.send(Err(error));
                return;
            }
            let span = queue_job_span(&job.context, "text_generation_stream", &device_name, false);
            let start = job.context.begin(false);
            let requested_model = job.request.model.as_deref();
            if requested_model.is_some_and(|model| model != provider.profile.model_id) {
                let error = InferenceError::provider(anyhow::anyhow!(
                    "queued provider {} cannot serve requested model {}",
                    provider.profile.model_id,
                    requested_model.unwrap_or_default()
                ));
                let _ = job
                    .response
                    .send(Err(InferenceError::provider(&error)))
                    .await;
                job.context
                    .metrics
                    .finished(&Err(&error), start.elapsed().as_micros() as u64);
                let _ = job.completion.send(Err(error));
                return;
            }
            let mut profile = provider.profile.clone();
            profile.reasoning = reasoning_policy(job.request.enable_thinking);
            let lease = tokio::select! {
                _ = job.context.cancellation.cancelled() => {
                    let error = job.context.cancellation.error();
                    job.context.metrics.finished(
                        &Err(&error),
                        start.elapsed().as_micros() as u64,
                    );
                    let _ = job.completion.send(Err(error));
                    return;
                }
                result = provider.runtime.acquire(&profile) => match result {
                    Ok(lease) => lease,
                    Err(error) => {
                        let error = InferenceError::classify_timeout(
                            error,
                            TimeoutClass::ModelActivation,
                        );
                        let _ = job.response.send(Err(InferenceError::provider(&error))).await;
                        job.context.metrics.finished(
                            &Err(&error),
                            start.elapsed().as_micros() as u64,
                        );
                        let _ = job.completion.send(Err(error));
                        return;
                    }
                }
            };
            let mut stream = provider
                .provider
                .complete_stream_with_lease_and_cancellation(
                    job.request,
                    lease,
                    job.context.cancellation.clone(),
                );
            let terminal = loop {
                let item = tokio::select! {
                    _ = job.context.cancellation.cancelled() => {
                        break Err(job.context.cancellation.error());
                    }
                    item = stream.recv() => item,
                };
                let Some(item) = item else {
                    break Ok(());
                };
                let item = item.map_err(|error| {
                    InferenceError::classify_timeout(error, TimeoutClass::Provider)
                });
                let failed = item.is_err();
                if job.response.send(item).await.is_err() {
                    job.context.cancellation.cancel();
                    break Err(job.context.cancellation.error());
                }
                if failed {
                    break Err(InferenceError::provider("stream provider failed"));
                }
            };
            job.context.metrics.finished(
                &terminal.as_ref().map(|_| ()).map_err(|error| error),
                start.elapsed().as_micros() as u64,
            );
            finish_queue_span(&span, &terminal, start.elapsed().as_micros() as u64);
            debug!(
                job_id = job.context.id,
                device = %device_name,
                ok = terminal.is_ok(),
                duration_ms = start.elapsed().as_millis(),
                "LLM streaming job complete"
            );
            let _ = job.completion.send(terminal);
        }
    })
    .await;
}

async fn complete_coordinated(
    provider: &CoordinatedChatProvider,
    request: crate::lemonade::ChatRequest,
) -> anyhow::Result<crate::lemonade::ChatCompletionResponse> {
    if request
        .model
        .as_deref()
        .is_some_and(|model| model != provider.profile.model_id)
    {
        anyhow::bail!(
            "queued provider {} cannot serve requested model {}",
            provider.profile.model_id,
            request.model.as_deref().unwrap_or_default()
        );
    }
    let mut profile = provider.profile.clone();
    profile.reasoning = reasoning_policy(request.enable_thinking);
    let lease = provider.runtime.acquire(&profile).await?;
    provider.provider.complete_with_lease(request, lease).await
}

fn reasoning_policy(enable_thinking: Option<bool>) -> ReasoningPolicy {
    match enable_thinking {
        None => ReasoningPolicy::Default,
        Some(true) => ReasoningPolicy::Enabled,
        Some(false) => ReasoningPolicy::Disabled,
    }
}

pub(super) async fn run_rerank_worker(
    queue: Arc<WorkQueue<RerankJob>>,
    provider: Arc<dyn RerankProvider>,
    device_name: String,
) {
    run_worker_loop(queue, move |job| {
        let provider = Arc::clone(&provider);
        let device_name = device_name.clone();
        async move {
            if job.context.cancellation.is_cancelled() {
                let error = job.context.cancellation.error();
                job.context.metrics.cancelled_pending(&error);
                let _ = job.response.send(Err(error));
                return;
            }
            let span = queue_job_span(&job.context, "reranking", &device_name, false);
            let start = job.context.begin(false);
            let n_docs = job.documents.len();
            let result = async {
                tokio::select! {
                    _ = job.context.cancellation.cancelled() => {
                        Err(job.context.cancellation.error())
                    }
                    result = provider.rerank(&job.query, job.documents, job.top_n) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(span.clone())
            .await;
            finish_queue_span(&span, &result, start.elapsed().as_micros() as u64);
            job.context.metrics.finished(
                &result.as_ref().map(|_| ()).map_err(|error| error),
                start.elapsed().as_micros() as u64,
            );
            debug!(
                job_id = job.context.id,
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
    stolen: bool,
) {
    if job.context.cancellation.is_cancelled() {
        let error = job.context.cancellation.error();
        job.context.metrics.cancelled_pending(&error);
        let _ = job.response.send(Err(error));
        return;
    }
    let span = queue_job_span(&job.context, "embedding", device_name, stolen);
    let start = job.context.begin(stolen);
    let mut last_err: Option<InferenceError> = None;
    let mut result: Option<Vec<f32>> = None;

    for attempt in 1..=EMBED_MAX_ATTEMPTS {
        let attempt_result = async {
            tokio::select! {
                _ = job.context.cancellation.cancelled() => {
                    Err(job.context.cancellation.error())
                }
                result = provider.embed(&job.text) => result.map_err(|error| {
                    InferenceError::classify_timeout(error, TimeoutClass::Provider)
                }),
            }
        }
        .instrument(span.clone())
        .await;
        match attempt_result {
            Ok(vec) => {
                result = Some(vec);
                break;
            }
            Err(
                error @ (InferenceError::Cancelled
                | InferenceError::Superseded
                | InferenceError::TimedOut { .. }),
            ) => {
                let elapsed_us = start.elapsed().as_micros() as u64;
                job.context.metrics.finished(&Err(&error), elapsed_us);
                let _ = job.response.send(Err(error));
                return;
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
                    job.context.metrics.retry();
                    span.record("retries", attempt);
                    tokio::select! {
                        _ = job.context.cancellation.cancelled() => {
                            let error = job.context.cancellation.error();
                            job.context.metrics.finished(
                                &Err(&error),
                                start.elapsed().as_micros() as u64,
                            );
                            let _ = job.response.send(Err(error));
                            return;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                }
            }
        }
    }

    let final_result = result.ok_or_else(|| {
        last_err.unwrap_or_else(|| InferenceError::provider("embed failed with no error detail"))
    });

    let elapsed_us = start.elapsed().as_micros() as u64;
    debug!(
        job_id = job.context.id,
        device = %device_name,
        text_len = job.text.len(),
        ok = final_result.is_ok(),
        duration_ms = elapsed_us / 1000,
        "Embed job complete"
    );

    job.context.metrics.finished(
        &final_result.as_ref().map(|_| ()).map_err(|error| error),
        elapsed_us,
    );
    finish_queue_span(&span, &final_result, elapsed_us);

    // Cancellation never trains the dispatch estimator. Successful work and
    // terminal provider failures do, because both represent occupied service
    // time on this worker.
    if !matches!(
        final_result,
        Err(InferenceError::Cancelled
            | InferenceError::Superseded
            | InferenceError::TimedOut { .. })
    ) {
        let old = ewma_us.load(Ordering::Relaxed);
        let new_ewma = if old == 0 {
            elapsed_us
        } else {
            old / 2 + elapsed_us / 2
        };
        ewma_us.store(new_ewma, Ordering::Relaxed);
    }

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
            execute_embed_job(job, &provider, &device_name, &ewma_us, false).await;
            continue;
        }

        // Try to steal from the most-loaded other worker.  This drains
        // backlogged neighbours without any additional synchronisation.
        if let Some(job) = dispatcher.steal_from_busiest(&queue) {
            idle.store(false, Ordering::Relaxed);
            debug!(device = %device_name, "Work-stealing embed job from neighbour queue");
            execute_embed_job(job, &provider, &device_name, &ewma_us, true).await;
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
            if job.context.cancellation.is_cancelled() {
                let error = job.context.cancellation.error();
                job.context.metrics.cancelled_pending(&error);
                let _ = job.response.send(Err(error));
                return;
            }
            let span = queue_job_span(&job.context, "transcription", &device_name, false);
            let start = job.context.begin(false);
            let result = async {
                tokio::select! {
                    _ = job.context.cancellation.cancelled() => {
                        Err(job.context.cancellation.error())
                    }
                    result = provider.transcribe(job.audio_bytes, &job.filename) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(span.clone())
            .await;
            finish_queue_span(&span, &result, start.elapsed().as_micros() as u64);
            job.context.metrics.finished(
                &result.as_ref().map(|_| ()).map_err(|error| error),
                start.elapsed().as_micros() as u64,
            );
            debug!(
                job_id = job.context.id,
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
            if job.context.cancellation.is_cancelled() {
                let error = job.context.cancellation.error();
                job.context.metrics.cancelled_pending(&error);
                let _ = job.response.send(Err(error));
                return;
            }
            let span = queue_job_span(&job.context, "text_to_speech", &device_name, false);
            let start = job.context.begin(false);
            let provider_call = async {
                match &job.voice {
                    Some(voice) => tts.synthesize(&job.text, Some(voice)).await,
                    None => tts.synthesize_default(&job.text).await,
                }
            };
            let result = async {
                tokio::select! {
                    _ = job.context.cancellation.cancelled() => {
                        Err(job.context.cancellation.error())
                    }
                    result = provider_call => result.map_err(|error| {
                        InferenceError::classify_timeout(error, TimeoutClass::Provider)
                    }),
                }
            }
            .instrument(span.clone())
            .await;
            finish_queue_span(&span, &result, start.elapsed().as_micros() as u64);
            job.context.metrics.finished(
                &result.as_ref().map(|_| ()).map_err(|error| error),
                start.elapsed().as_micros() as u64,
            );
            debug!(
                job_id = job.context.id,
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
