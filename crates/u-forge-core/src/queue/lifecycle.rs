//! Public cancellation and completion primitives for inference operations.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::task::{Context, Poll};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::sync::oneshot;

use super::jobs::JobContext;
use super::telemetry::QueueMetrics;

/// Stable timeout categories exposed to callers and queue telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutClass {
    #[error("queue wait")]
    QueueWait,
    #[error("provider request")]
    Provider,
    #[error("model activation")]
    ModelActivation,
    #[error("first token")]
    FirstToken,
    #[error("stream idle")]
    StreamIdle,
    #[error("parent operation")]
    Operation,
}

/// Typed terminal outcomes for queue jobs.
///
/// Provider errors retain their full formatted error chain without making
/// user-directed cancellation look like a provider failure.
#[derive(Debug, Clone, thiserror::Error)]
pub enum InferenceError {
    #[error("inference operation was cancelled")]
    Cancelled,
    #[error("inference operation was superseded")]
    Superseded,
    #[error("inference operation timed out during {class}")]
    TimedOut { class: TimeoutClass },
    #[error("inference provider failed: {message}")]
    ProviderFailed { message: String },
    #[error("inference worker dropped before reporting completion")]
    WorkerDropped,
    #[error("no {capability} inference provider is registered")]
    CapabilityUnavailable { capability: &'static str },
}

impl InferenceError {
    pub(crate) fn provider(error: impl std::fmt::Display) -> Self {
        Self::ProviderFailed {
            message: format!("{error:#}"),
        }
    }

    pub(crate) fn classify_timeout(
        error: impl std::fmt::Display,
        default_class: TimeoutClass,
    ) -> Self {
        let message = format!("{error:#}");
        let lower = message.to_ascii_lowercase();
        if lower.contains("timed out") || lower.contains("timeout") {
            let class = if lower.contains("first token") {
                TimeoutClass::FirstToken
            } else if lower.contains("stream idle") {
                TimeoutClass::StreamIdle
            } else {
                default_class
            };
            Self::TimedOut { class }
        } else {
            Self::ProviderFailed { message }
        }
    }
}

pub type InferenceResult<T> = Result<T, InferenceError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellationReason {
    Cancelled,
    Superseded,
    TimedOut(TimeoutClass),
}

impl CancellationReason {
    fn encode(self) -> u8 {
        match self {
            Self::Cancelled => 1,
            Self::Superseded => 2,
            Self::TimedOut(TimeoutClass::QueueWait) => 3,
            Self::TimedOut(TimeoutClass::Provider) => 4,
            Self::TimedOut(TimeoutClass::ModelActivation) => 5,
            Self::TimedOut(TimeoutClass::FirstToken) => 6,
            Self::TimedOut(TimeoutClass::StreamIdle) => 7,
            Self::TimedOut(TimeoutClass::Operation) => 8,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            2 => Self::Superseded,
            3 => Self::TimedOut(TimeoutClass::QueueWait),
            4 => Self::TimedOut(TimeoutClass::Provider),
            5 => Self::TimedOut(TimeoutClass::ModelActivation),
            6 => Self::TimedOut(TimeoutClass::FirstToken),
            7 => Self::TimedOut(TimeoutClass::StreamIdle),
            8 => Self::TimedOut(TimeoutClass::Operation),
            _ => Self::Cancelled,
        }
    }

    fn error(self) -> InferenceError {
        match self {
            Self::Cancelled => InferenceError::Cancelled,
            Self::Superseded => InferenceError::Superseded,
            Self::TimedOut(class) => InferenceError::TimedOut { class },
        }
    }
}

#[derive(Debug)]
struct CancellationState {
    token: tokio_util::sync::CancellationToken,
    reason: AtomicU8,
}

/// Cloneable parent token shared by an operation and every queue job it owns.
///
/// The first terminal reason wins, so a late task teardown cannot overwrite a
/// more useful `Superseded` or timeout outcome.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                token: tokio_util::sync::CancellationToken::new(),
                reason: AtomicU8::new(0),
            }),
        }
    }

    pub fn cancel(&self) {
        self.cancel_with(CancellationReason::Cancelled);
    }

    pub fn supersede(&self) {
        self.cancel_with(CancellationReason::Superseded);
    }

    pub fn time_out(&self, class: TimeoutClass) {
        self.cancel_with(CancellationReason::TimedOut(class));
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.token.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.state.token.cancelled().await;
    }

    /// Return the recorded cancellation reason when this operation has ended.
    pub fn check_cancelled(&self) -> InferenceResult<()> {
        if self.is_cancelled() {
            Err(self.error())
        } else {
            Ok(())
        }
    }

    pub fn error(&self) -> InferenceError {
        CancellationReason::decode(self.state.reason.load(Ordering::Acquire)).error()
    }

    fn cancel_with(&self, reason: CancellationReason) {
        let _ = self.state.reason.compare_exchange(
            0,
            reason.encode(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.state.token.cancel();
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Completion half of an [`InferenceJob`].
pub struct JobCompletion<T> {
    future: Pin<Box<dyn Future<Output = InferenceResult<T>> + Send + 'static>>,
}

impl<T: Send + 'static> JobCompletion<T> {
    pub(crate) fn from_receiver(
        cancellation: CancellationToken,
        receiver: oneshot::Receiver<InferenceResult<T>>,
        metrics: Option<Arc<QueueMetrics>>,
    ) -> Self {
        Self {
            future: Box::pin(async move {
                tokio::select! {
                    biased;
                    result = receiver => match result {
                        Ok(result) => result,
                        Err(_) => {
                            if let Some(metrics) = metrics {
                                metrics.worker_dropped();
                            }
                            Err(InferenceError::WorkerDropped)
                        }
                    },
                    _ = cancellation.cancelled() => Err(cancellation.error()),
                }
            }),
        }
    }

    pub(crate) fn ready(result: InferenceResult<T>) -> Self {
        Self {
            future: Box::pin(std::future::ready(result)),
        }
    }
}

impl<T> Future for JobCompletion<T> {
    type Output = InferenceResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

/// Cancellable queue submission with independently owned completion.
pub struct InferenceJob<T> {
    pub cancellation: CancellationToken,
    pub completion: JobCompletion<T>,
}

impl<T> InferenceJob<T> {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn supersede(&self) {
        self.cancellation.supersede();
    }

    pub fn into_parts(self) -> (CancellationToken, JobCompletion<T>) {
        (self.cancellation, self.completion)
    }
}

impl<T> Future for InferenceJob<T> {
    type Output = InferenceResult<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().completion).poll(cx)
    }
}

/// Construct and enqueue one accepted one-shot job, or return an unavailable
/// completion without invoking either capability-specific closure.
pub(super) fn submit_one_shot<T, J>(
    available: bool,
    capability: &'static str,
    cancellation: CancellationToken,
    metrics: Arc<QueueMetrics>,
    make_job: impl FnOnce(JobContext, oneshot::Sender<InferenceResult<T>>) -> J,
    enqueue: impl FnOnce(J),
) -> InferenceJob<T>
where
    T: Send + 'static,
{
    if !available {
        metrics.unavailable();
        return InferenceJob {
            completion: JobCompletion::ready(Err(InferenceError::CapabilityUnavailable {
                capability,
            })),
            cancellation,
        };
    }

    let (response, receiver) = oneshot::channel();
    let context = JobContext::new(cancellation.clone(), Arc::clone(&metrics));
    enqueue(make_job(context, response));
    InferenceJob {
        completion: JobCompletion::from_receiver(cancellation.clone(), receiver, Some(metrics)),
        cancellation,
    }
}

/// Single terminal authority for an accepted one-shot job.
pub(super) struct OneShotReporter<T> {
    context: JobContext,
    response: Option<oneshot::Sender<InferenceResult<T>>>,
    span: tracing::Span,
    started_at: Instant,
}

impl<T> OneShotReporter<T> {
    /// Skip pending cancellation without starting service, otherwise begin the
    /// job exactly once and return its terminal reporter.
    pub(super) fn begin(
        context: JobContext,
        response: oneshot::Sender<InferenceResult<T>>,
        capability: &'static str,
        device_name: &str,
        stolen: bool,
    ) -> Option<Self> {
        if context.cancellation.is_cancelled() {
            let error = context.cancellation.error();
            record_pending_cancellation(&context, capability, device_name, &error);
            let _ = response.send(Err(error));
            return None;
        }

        let span = queue_job_span(&context, capability, device_name, stolen);
        let started_at = context.begin(stolen);
        Some(Self {
            context,
            response: Some(response),
            span,
            started_at,
        })
    }

    pub(super) fn cancellation(&self) -> &CancellationToken {
        &self.context.cancellation
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(super) fn job_id(&self) -> u64 {
        self.context.id
    }

    pub(super) fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    pub(super) fn finish(mut self, result: InferenceResult<T>) {
        self.record_terminal(&result);
        if let Some(response) = self.response.take() {
            let _ = response.send(result);
        }
    }

    fn record_terminal(&self, result: &InferenceResult<T>) {
        let service_us = self.elapsed().as_micros() as u64;
        self.context
            .metrics
            .finished(&result.as_ref().map(|_| ()), service_us);
        finish_queue_span(&self.span, result, service_us);
    }
}

impl<T> Drop for OneShotReporter<T> {
    fn drop(&mut self) {
        let Some(response) = self.response.take() else {
            return;
        };
        let result = Err(InferenceError::WorkerDropped);
        self.record_terminal(&result);
        let _ = response.send(result);
    }
}

/// Streaming terminal authority. Item delivery remains separate from the
/// awaitable lifecycle completion owned here.
pub(super) struct StreamingReporter<T> {
    context: JobContext,
    response: mpsc::Sender<InferenceResult<T>>,
    completion: Option<oneshot::Sender<InferenceResult<()>>>,
    span: tracing::Span,
    started_at: Instant,
}

impl<T> StreamingReporter<T> {
    pub(super) fn begin(
        context: JobContext,
        response: mpsc::Sender<InferenceResult<T>>,
        completion: oneshot::Sender<InferenceResult<()>>,
        capability: &'static str,
        device_name: &str,
    ) -> Option<Self> {
        if context.cancellation.is_cancelled() {
            let error = context.cancellation.error();
            record_pending_cancellation(&context, capability, device_name, &error);
            let _ = completion.send(Err(error));
            return None;
        }

        let span = queue_job_span(&context, capability, device_name, false);
        let started_at = context.begin(false);
        Some(Self {
            context,
            response,
            completion: Some(completion),
            span,
            started_at,
        })
    }

    pub(super) fn cancellation(&self) -> &CancellationToken {
        &self.context.cancellation
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(super) fn job_id(&self) -> u64 {
        self.context.id
    }

    pub(super) fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// Forward one stream item. Closing the item channel cancels the operation
    /// but leaves terminal completion owned by the reporter.
    pub(super) async fn send_item(&self, item: InferenceResult<T>) -> InferenceResult<()> {
        if self.response.send(item).await.is_err() {
            self.context.cancellation.cancel();
            Err(self.context.cancellation.error())
        } else {
            Ok(())
        }
    }

    pub(super) fn finish(mut self, result: InferenceResult<()>) {
        self.record_terminal(&result);
        if let Some(completion) = self.completion.take() {
            let _ = completion.send(result);
        }
    }

    fn record_terminal(&self, result: &InferenceResult<()>) {
        let service_us = self.elapsed().as_micros() as u64;
        self.context
            .metrics
            .finished(&result.as_ref().map(|_| ()), service_us);
        finish_queue_span(&self.span, result, service_us);
    }
}

impl<T> Drop for StreamingReporter<T> {
    fn drop(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let result = Err(InferenceError::WorkerDropped);
        self.record_terminal(&result);
        let _ = completion.send(result);
    }
}

pub(super) fn queue_job_span(
    context: &JobContext,
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

pub(super) fn finish_queue_span<T>(
    span: &tracing::Span,
    result: &InferenceResult<T>,
    service_us: u64,
) {
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

pub(super) fn record_pending_cancellation(
    context: &JobContext,
    capability: &'static str,
    device_name: &str,
    error: &InferenceError,
) {
    context.metrics.cancelled_pending(error);
    let outcome = match error {
        InferenceError::Superseded => "superseded",
        InferenceError::TimedOut { .. } => "timed_out",
        _ => "cancelled",
    };
    tracing::info!(
        job_id = context.id,
        capability,
        selected_worker = device_name,
        queue_wait_us = context.enqueued_at.elapsed().as_micros() as u64,
        cancellation_point = "pending",
        outcome,
        "Inference queue job skipped before provider invocation"
    );
}

/// Streaming queue submission with explicit cancellation and termination.
pub struct StreamingInferenceJob<T> {
    pub cancellation: CancellationToken,
    pub stream: mpsc::Receiver<InferenceResult<T>>,
    pub completion: JobCompletion<()>,
}

impl<T> StreamingInferenceJob<T> {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_shot_submission_constructs_and_accounts_once() {
        let metrics = Arc::new(QueueMetrics::default());
        let job = submit_one_shot(
            true,
            "test",
            CancellationToken::new(),
            Arc::clone(&metrics),
            |_, response| response,
            |response| {
                response.send(Ok(42)).unwrap();
            },
        );

        assert_eq!(job.await.unwrap(), 42);
        let counters = metrics.snapshot();
        assert_eq!(counters.submitted, 1);
        assert_eq!(counters.unavailable, 0);
    }

    #[tokio::test]
    async fn unavailable_one_shot_skips_payload_and_queue_construction() {
        let metrics = Arc::new(QueueMetrics::default());
        let job: InferenceJob<()> = submit_one_shot(
            false,
            "test",
            CancellationToken::new(),
            Arc::clone(&metrics),
            |_, _| panic!("unavailable submission built a payload"),
            |_: ()| panic!("unavailable submission reached a queue"),
        );

        assert!(matches!(
            job.await,
            Err(InferenceError::CapabilityUnavailable { capability: "test" })
        ));
        let counters = metrics.snapshot();
        assert_eq!(counters.submitted, 1);
        assert_eq!(counters.unavailable, 1);
        assert_eq!(counters.started, 0);
    }

    #[tokio::test]
    async fn one_shot_reporter_owns_started_terminal_accounting() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (response, receiver) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            receiver,
            Some(Arc::clone(&metrics)),
        );
        let reporter = OneShotReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            response,
            "test",
            "worker",
            false,
        )
        .unwrap();

        reporter.finish(Ok(42));
        assert_eq!(completion.await.unwrap(), 42);
        let counters = metrics.snapshot();
        assert_eq!(counters.submitted, 1);
        assert_eq!(counters.started, 1);
        assert_eq!(counters.succeeded, 1);
        assert_eq!(counters.service_time.samples, 1);
    }

    #[tokio::test]
    async fn one_shot_reporter_drop_delivers_worker_dropped_once() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (response, receiver) = oneshot::channel::<InferenceResult<()>>();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            receiver,
            Some(Arc::clone(&metrics)),
        );
        let reporter = OneShotReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            response,
            "test",
            "worker",
            false,
        )
        .unwrap();

        drop(reporter);
        assert!(matches!(
            completion.await,
            Err(InferenceError::WorkerDropped)
        ));
        let counters = metrics.snapshot();
        assert_eq!(counters.started, 1);
        assert_eq!(counters.worker_dropped, 1);
        assert_eq!(counters.service_time.samples, 1);
    }

    #[tokio::test]
    async fn one_shot_reporter_skips_pending_cancellation() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (response, receiver) = oneshot::channel::<InferenceResult<()>>();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            receiver,
            Some(Arc::clone(&metrics)),
        );

        assert!(
            OneShotReporter::begin(
                JobContext::new(cancellation, Arc::clone(&metrics)),
                response,
                "test",
                "worker",
                false,
            )
            .is_none()
        );
        assert!(matches!(completion.await, Err(InferenceError::Cancelled)));
        let counters = metrics.snapshot();
        assert_eq!(counters.submitted, 1);
        assert_eq!(counters.started, 0);
        assert_eq!(counters.cancelled_pending, 1);
        assert_eq!(counters.service_time.samples, 0);
    }

    #[tokio::test]
    async fn streaming_reporter_skips_pending_cancellation() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (item_tx, _item_rx) = mpsc::channel::<InferenceResult<()>>(1);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );

        assert!(
            StreamingReporter::begin(
                JobContext::new(cancellation, Arc::clone(&metrics)),
                item_tx,
                completion_tx,
                "test_stream",
                "worker",
            )
            .is_none()
        );
        assert!(matches!(completion.await, Err(InferenceError::Cancelled)));
        let counters = metrics.snapshot();
        assert_eq!(counters.started, 0);
        assert_eq!(counters.cancelled_pending, 1);
        assert_eq!(counters.service_time.samples, 0);
    }

    #[tokio::test]
    async fn streaming_reporter_separates_items_from_normal_completion() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (item_tx, mut item_rx) = mpsc::channel(1);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );
        let reporter = StreamingReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            item_tx,
            completion_tx,
            "test_stream",
            "worker",
        )
        .unwrap();

        reporter.send_item(Ok(7)).await.unwrap();
        reporter.finish(Ok(()));
        assert_eq!(item_rx.recv().await.unwrap().unwrap(), 7);
        completion.await.unwrap();
        let counters = metrics.snapshot();
        assert_eq!(counters.started, 1);
        assert_eq!(counters.succeeded, 1);
        assert_eq!(counters.service_time.samples, 1);
    }

    #[tokio::test]
    async fn streaming_reporter_preserves_setup_failure_for_item_and_terminal() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (item_tx, mut item_rx) = mpsc::channel::<InferenceResult<()>>(1);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );
        let reporter = StreamingReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            item_tx,
            completion_tx,
            "test_stream",
            "worker",
        )
        .unwrap();
        let error = InferenceError::ProviderFailed {
            message: "model mismatch".into(),
        };

        reporter.send_item(Err(error.clone())).await.unwrap();
        reporter.finish(Err(error));
        assert!(matches!(
            item_rx.recv().await.unwrap(),
            Err(InferenceError::ProviderFailed { message }) if message == "model mismatch"
        ));
        assert!(matches!(
            completion.await,
            Err(InferenceError::ProviderFailed { message }) if message == "model mismatch"
        ));
        assert_eq!(metrics.snapshot().provider_failed, 1);
    }

    #[tokio::test]
    async fn streaming_reporter_preserves_activation_timeout_class() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (item_tx, mut item_rx) = mpsc::channel::<InferenceResult<()>>(1);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );
        let reporter = StreamingReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            item_tx,
            completion_tx,
            "test_stream",
            "worker",
        )
        .unwrap();
        let error = InferenceError::TimedOut {
            class: TimeoutClass::ModelActivation,
        };

        reporter.send_item(Err(error.clone())).await.unwrap();
        reporter.finish(Err(error));
        assert!(matches!(
            item_rx.recv().await.unwrap(),
            Err(InferenceError::TimedOut {
                class: TimeoutClass::ModelActivation
            })
        ));
        assert!(matches!(
            completion.await,
            Err(InferenceError::TimedOut {
                class: TimeoutClass::ModelActivation
            })
        ));
        let counters = metrics.snapshot();
        assert_eq!(counters.cancelled_active, 1);
        assert_eq!(counters.timed_out, 1);
    }

    #[tokio::test]
    async fn streaming_item_receiver_closure_cancels_and_completes() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (item_tx, item_rx) = mpsc::channel::<InferenceResult<()>>(1);
        drop(item_rx);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );
        let reporter = StreamingReporter::begin(
            JobContext::new(cancellation.clone(), Arc::clone(&metrics)),
            item_tx,
            completion_tx,
            "test_stream",
            "worker",
        )
        .unwrap();

        let terminal = reporter.send_item(Ok(())).await.unwrap_err();
        assert!(matches!(terminal, InferenceError::Cancelled));
        assert!(cancellation.is_cancelled());
        reporter.finish(Err(terminal));
        assert!(matches!(completion.await, Err(InferenceError::Cancelled)));
        let counters = metrics.snapshot();
        assert_eq!(counters.cancelled_active, 1);
        assert_eq!(counters.service_time.samples, 1);
    }

    #[tokio::test]
    async fn streaming_reporter_drop_delivers_worker_dropped_once() {
        let metrics = Arc::new(QueueMetrics::default());
        let cancellation = CancellationToken::new();
        let (item_tx, _item_rx) = mpsc::channel::<InferenceResult<()>>(1);
        let (completion_tx, completion_rx) = oneshot::channel();
        let completion = JobCompletion::from_receiver(
            cancellation.clone(),
            completion_rx,
            Some(Arc::clone(&metrics)),
        );
        let reporter = StreamingReporter::begin(
            JobContext::new(cancellation, Arc::clone(&metrics)),
            item_tx,
            completion_tx,
            "test_stream",
            "worker",
        )
        .unwrap();

        drop(reporter);
        assert!(matches!(
            completion.await,
            Err(InferenceError::WorkerDropped)
        ));
        let counters = metrics.snapshot();
        assert_eq!(counters.worker_dropped, 1);
        assert_eq!(counters.service_time.samples, 1);
    }

    #[tokio::test]
    async fn cancellation_reason_is_preserved() {
        let token = CancellationToken::new();
        let (_tx, rx) = oneshot::channel::<InferenceResult<()>>();
        let completion = JobCompletion::from_receiver(token.clone(), rx, None);
        token.supersede();
        assert!(matches!(completion.await, Err(InferenceError::Superseded)));
    }

    #[tokio::test]
    async fn completion_wins_when_already_available() {
        let token = CancellationToken::new();
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(42)).unwrap();
        token.cancel();
        assert_eq!(
            JobCompletion::from_receiver(token, rx, None).await.unwrap(),
            42
        );
    }

    #[tokio::test]
    async fn timeout_and_worker_drop_are_distinct_terminal_outcomes() {
        let timeout = CancellationToken::new();
        let (_tx, rx) = oneshot::channel::<InferenceResult<()>>();
        let completion = JobCompletion::from_receiver(timeout.clone(), rx, None);
        timeout.time_out(TimeoutClass::FirstToken);
        assert!(matches!(
            completion.await,
            Err(InferenceError::TimedOut {
                class: TimeoutClass::FirstToken
            })
        ));

        let metrics = Arc::new(QueueMetrics::default());
        let token = CancellationToken::new();
        let (tx, rx) = oneshot::channel::<InferenceResult<()>>();
        drop(tx);
        assert!(matches!(
            JobCompletion::from_receiver(token, rx, Some(metrics.clone())).await,
            Err(InferenceError::WorkerDropped)
        ));
        assert_eq!(metrics.snapshot().worker_dropped, 1);
    }

    #[test]
    fn provider_timeout_text_is_classified_by_phase() {
        assert!(matches!(
            InferenceError::classify_timeout(
                "Lemonade first token timeout after 1s",
                TimeoutClass::Provider
            ),
            InferenceError::TimedOut {
                class: TimeoutClass::FirstToken
            }
        ));
        assert!(matches!(
            InferenceError::classify_timeout("connection refused", TimeoutClass::Provider),
            InferenceError::ProviderFailed { .. }
        ));
    }
}
