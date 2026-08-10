//! Public cancellation and completion primitives for inference operations.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::task::{Context, Poll};

use tokio::sync::mpsc;
use tokio::sync::oneshot;

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
#[derive(Debug, thiserror::Error)]
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
