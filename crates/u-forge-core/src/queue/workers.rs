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
    atomic::{AtomicU64, Ordering},
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
use super::lifecycle::{InferenceError, OneShotReporter, StreamingReporter, TimeoutClass};
use super::weighted::WeightedEmbedDispatcher;

/// Maximum number of attempts for a single embed job before the error is
/// returned to the caller.  Retries guard against transient server hiccups
/// (e.g. a Lemonade instance that is momentarily swapping a model in/out).
const EMBED_MAX_ATTEMPTS: u32 = 3;

/// Base delay before the first retry.  Doubles on each subsequent attempt
/// (100 ms → 200 ms) so three attempts add at most ~300 ms of backoff.
const EMBED_RETRY_BASE_MS: u64 = 100;

// Evidence decision (inference_lifecycle/retry_recovery_lockstep): two
// deterministic workers recover concurrently in the same bounded 300 ms
// backoff window as one worker. No shared-provider contention appeared, so
// adding random jitter would add variance without evidence of benefit.

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
            let GenerateJob {
                context,
                request,
                response,
            } = job;
            let Some(reporter) = OneShotReporter::begin(
                context,
                response,
                "text_generation",
                &device_name,
                false,
            ) else {
                return;
            };
            let n_messages = request.messages.len();
            let cancellation = reporter.cancellation().clone();
            let result = async {
                tokio::select! {
                    _ = cancellation.cancelled() => Err(cancellation.error()),
                    result = complete_coordinated(&provider, request) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(reporter.span().clone())
            .await;
            debug!(
                job_id = reporter.job_id(),
                device = %device_name,
                n_messages,
                ok = result.is_ok(),
                duration_ms = reporter.elapsed().as_millis(),
                "LLM generation job complete"
            );
            reporter.finish(result);
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
            let GenerateStreamJob {
                context,
                request,
                response,
                completion,
            } = job;
            let Some(reporter) = StreamingReporter::begin(
                context,
                response,
                completion,
                "text_generation_stream",
                &device_name,
            ) else {
                return;
            };
            let cancellation = reporter.cancellation().clone();
            let terminal = async {
                let requested_model = request.model.as_deref();
                if requested_model.is_some_and(|model| model != provider.profile.model_id) {
                    let error = InferenceError::provider(anyhow::anyhow!(
                        "queued provider {} cannot serve requested model {}",
                        provider.profile.model_id,
                        requested_model.unwrap_or_default()
                    ));
                    reporter.send_item(Err(error.clone())).await?;
                    return Err(error);
                }

                let mut profile = provider.profile.clone();
                profile.reasoning = reasoning_policy(request.enable_thinking);
                let lease_result = tokio::select! {
                    error = reporter.item_receiver_closed() => return Err(error),
                    result = provider.runtime.acquire_with_cancellation(&profile, &cancellation) => result,
                };
                let lease = match lease_result {
                    Ok(lease) => lease,
                    Err(error) => {
                        reporter.send_item(Err(error.clone())).await?;
                        return Err(error);
                    }
                };
                let mut stream = provider
                    .provider
                    .complete_stream_with_lease_and_cancellation(
                        request,
                        lease,
                        cancellation.clone(),
                    );
                loop {
                    let item = tokio::select! {
                        _ = cancellation.cancelled() => return Err(cancellation.error()),
                        error = reporter.item_receiver_closed() => return Err(error),
                        item = stream.recv() => item,
                    };
                    match item {
                        None => return Ok(()),
                        Some(Ok(token)) => reporter.send_item(Ok(token)).await?,
                        Some(Err(error)) => {
                            let error =
                                InferenceError::classify_timeout(error, TimeoutClass::Provider);
                            reporter.send_item(Err(error.clone())).await?;
                            return Err(error);
                        }
                    }
                }
            }
            .instrument(reporter.span().clone())
            .await;
            debug!(
                job_id = reporter.job_id(),
                device = %device_name,
                ok = terminal.is_ok(),
                duration_ms = reporter.elapsed().as_millis(),
                "LLM streaming job complete"
            );
            reporter.finish(terminal);
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
            let RerankJob {
                context,
                query,
                documents,
                top_n,
                response,
            } = job;
            let Some(reporter) =
                OneShotReporter::begin(context, response, "reranking", &device_name, false)
            else {
                return;
            };
            let n_docs = documents.len();
            let cancellation = reporter.cancellation().clone();
            let result = async {
                tokio::select! {
                    _ = cancellation.cancelled() => Err(cancellation.error()),
                    result = provider.rerank(&query, documents, top_n) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(reporter.span().clone())
            .await;
            debug!(
                job_id = reporter.job_id(),
                device = %device_name,
                n_docs,
                top_n = ?top_n,
                ok = result.is_ok(),
                duration_ms = reporter.elapsed().as_millis(),
                "Rerank job complete"
            );
            reporter.finish(result);
        }
    })
    .await;
}

/// Embedding worker loop.
///
/// Execute a single embedding job: retry loop, EWMA update, send result.
async fn execute_embed_job(
    job: EmbedJob,
    provider: &Arc<dyn EmbeddingProvider>,
    device_name: &str,
    ewma_us: &Arc<AtomicU64>,
    stolen: bool,
) {
    let EmbedJob {
        context,
        text,
        response,
    } = job;
    let Some(reporter) =
        OneShotReporter::begin(context, response, "embedding", device_name, stolen)
    else {
        return;
    };
    let cancellation = reporter.cancellation().clone();
    let final_result = async {
        for attempt in 1..=EMBED_MAX_ATTEMPTS {
            let attempt_result = tokio::select! {
                _ = cancellation.cancelled() => Err(cancellation.error()),
                result = provider.embed(&text) => result.map_err(|error| {
                    InferenceError::classify_timeout(error, TimeoutClass::Provider)
                }),
            };
            match attempt_result {
                Ok(vector) => return Ok(vector),
                Err(
                    error @ (InferenceError::Cancelled
                    | InferenceError::Superseded
                    | InferenceError::TimedOut { .. }),
                ) => return Err(error),
                Err(error) => {
                    let delay_ms = EMBED_RETRY_BASE_MS * (1 << (attempt - 1));
                    debug!(
                        device = %device_name,
                        attempt,
                        EMBED_MAX_ATTEMPTS,
                        delay_ms,
                        error = %error,
                        "Embed attempt failed — retrying"
                    );
                    if attempt == EMBED_MAX_ATTEMPTS {
                        return Err(error);
                    }
                    reporter.record_retry(attempt);
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(cancellation.error()),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                }
            }
        }
        Err(InferenceError::provider(
            "embed failed with no error detail",
        ))
    }
    .instrument(reporter.span().clone())
    .await;

    let elapsed_us = reporter.elapsed().as_micros() as u64;
    debug!(
        job_id = reporter.job_id(),
        device = %device_name,
        text_len = text.len(),
        ok = final_result.is_ok(),
        duration_ms = elapsed_us / 1000,
        "Embed job complete"
    );

    // Cancellation never trains the dispatch estimator. Successful work and
    // terminal provider failures do, because both represent occupied service
    // time on this worker.
    if trains_embedding_ewma(&final_result) {
        let old = ewma_us.load(Ordering::Relaxed);
        let new_ewma = if old == 0 {
            elapsed_us
        } else {
            old / 2 + elapsed_us / 2
        };
        ewma_us.store(new_ewma, Ordering::Relaxed);
    }

    reporter.finish(final_result);
}

fn trains_embedding_ewma<T>(result: &Result<T, InferenceError>) -> bool {
    !matches!(
        result,
        Err(InferenceError::Cancelled
            | InferenceError::Superseded
            | InferenceError::TimedOut { .. })
    )
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
            execute_embed_job(job, &provider, &device_name, &ewma_us, false).await;
            continue;
        }

        // Try to steal from the most-loaded other worker.  This drains
        // backlogged neighbours without any additional synchronisation.
        if let Some(job) = dispatcher.steal_from_busiest(&queue) {
            debug!(device = %device_name, "Work-stealing embed job from neighbour queue");
            execute_embed_job(job, &provider, &device_name, &ewma_us, true).await;
            continue;
        }

        // Nothing to do — sleep until our queue or any other queue gets work.
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
            let TranscribeJob {
                context,
                audio_bytes,
                filename,
                response,
            } = job;
            let Some(reporter) =
                OneShotReporter::begin(context, response, "transcription", &device_name, false)
            else {
                return;
            };
            let cancellation = reporter.cancellation().clone();
            let result = async {
                tokio::select! {
                    _ = cancellation.cancelled() => Err(cancellation.error()),
                    result = provider.transcribe(audio_bytes, &filename) => {
                        result.map_err(|error| InferenceError::classify_timeout(error, TimeoutClass::Provider))
                    }
                }
            }
            .instrument(reporter.span().clone())
            .await;
            debug!(
                job_id = reporter.job_id(),
                device = %device_name,
                filename = %filename,
                ok = result.is_ok(),
                duration_ms = reporter.elapsed().as_millis(),
                "Transcription job complete"
            );
            reporter.finish(result);
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
            let SynthesizeJob {
                context,
                text,
                voice,
                response,
            } = job;
            let Some(reporter) =
                OneShotReporter::begin(context, response, "text_to_speech", &device_name, false)
            else {
                return;
            };
            let provider_call = async {
                match &voice {
                    Some(voice) => tts.synthesize(&text, Some(voice)).await,
                    None => tts.synthesize_default(&text).await,
                }
            };
            let cancellation = reporter.cancellation().clone();
            let result = async {
                tokio::select! {
                    _ = cancellation.cancelled() => Err(cancellation.error()),
                    result = provider_call => result.map_err(|error| {
                        InferenceError::classify_timeout(error, TimeoutClass::Provider)
                    }),
                }
            }
            .instrument(reporter.span().clone())
            .await;
            debug!(
                job_id = reporter.job_id(),
                device = %device_name,
                text_len = text.len(),
                voice = voice.as_ref().map(|v| v.as_str()).unwrap_or("default"),
                ok = result.is_ok(),
                duration_ms = reporter.elapsed().as_millis(),
                "TTS job complete"
            );
            reporter.finish(result);
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_ewma_trains_only_on_observed_service() {
        assert!(trains_embedding_ewma(&Ok::<_, InferenceError>(())));
        assert!(trains_embedding_ewma::<()>(&Err(
            InferenceError::ProviderFailed {
                message: "terminal provider failure".into(),
            }
        )));

        for result in [
            Err(InferenceError::Cancelled),
            Err(InferenceError::Superseded),
            Err(InferenceError::TimedOut {
                class: TimeoutClass::Provider,
            }),
            Err(InferenceError::TimedOut {
                class: TimeoutClass::Operation,
            }),
        ] {
            assert!(!trains_embedding_ewma::<()>(&result));
        }
    }
}
