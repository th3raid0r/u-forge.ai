//! Internal job types and the `WorkQueue<T>` primitive.

use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::lemonade::{
    ChatCompletionResponse, ChatRequest, KokoroVoice, RerankDocument, StreamToken,
};

use super::lifecycle::{CancellationToken, InferenceResult};
use super::telemetry::QueueMetrics;

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct JobContext {
    pub(super) id: u64,
    pub(super) cancellation: CancellationToken,
    pub(super) enqueued_at: Instant,
    pub(super) metrics: Arc<QueueMetrics>,
}

impl JobContext {
    pub(super) fn new(cancellation: CancellationToken, metrics: Arc<QueueMetrics>) -> Self {
        metrics.submitted();
        Self {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            cancellation,
            enqueued_at: Instant::now(),
            metrics,
        }
    }

    pub(super) fn begin(&self, stolen: bool) -> Instant {
        self.metrics
            .started(self.enqueued_at.elapsed().as_micros() as u64, stolen);
        Instant::now()
    }
}

// ── Internal job types ────────────────────────────────────────────────────────

/// A single text embedding job.
pub(super) struct EmbedJob {
    pub(super) context: JobContext,
    pub(super) text: String,
    pub(super) response: oneshot::Sender<InferenceResult<Vec<f32>>>,
}

/// A single audio transcription job.
pub(super) struct TranscribeJob {
    pub(super) context: JobContext,
    pub(super) audio_bytes: Vec<u8>,
    pub(super) filename: String,
    pub(super) response: oneshot::Sender<InferenceResult<String>>,
}

/// A single text-to-speech synthesis job.
pub(super) struct SynthesizeJob {
    pub(super) context: JobContext,
    pub(super) text: String,
    /// Explicit voice override; `None` uses the provider's default voice.
    pub(super) voice: Option<KokoroVoice>,
    pub(super) response: oneshot::Sender<InferenceResult<Vec<u8>>>,
}

/// A single LLM chat-completion job.
pub(super) struct GenerateJob {
    pub(super) context: JobContext,
    pub(super) request: ChatRequest,
    pub(super) response: oneshot::Sender<InferenceResult<ChatCompletionResponse>>,
}

/// A streaming LLM job. The worker owns the job until the complete stream has
/// been forwarded or the receiver is cancelled.
pub(super) struct GenerateStreamJob {
    pub(super) context: JobContext,
    pub(super) request: ChatRequest,
    pub(super) response: mpsc::Sender<InferenceResult<StreamToken>>,
    pub(super) completion: oneshot::Sender<InferenceResult<()>>,
}

/// A single document reranking job.
pub(super) struct RerankJob {
    pub(super) context: JobContext,
    pub(super) query: String,
    pub(super) documents: Vec<String>,
    pub(super) top_n: Option<usize>,
    pub(super) response: oneshot::Sender<InferenceResult<Vec<RerankDocument>>>,
}

// ── MPMC work-queue primitive ─────────────────────────────────────────────────

/// A thread-safe multi-producer / multi-consumer work queue.
///
/// Built from a `parking_lot::Mutex<VecDeque<T>>` plus a `tokio::sync::Notify`
/// to wake sleeping workers when new jobs arrive.  No additional crates needed.
pub(super) struct WorkQueue<T> {
    pub(super) queue: Mutex<VecDeque<T>>,
    pub(super) notify: Notify,
}

impl<T> WorkQueue<T> {
    pub(super) fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        }
    }

    /// Push a job and wake **one** waiting worker.
    pub(super) fn push(&self, job: T) {
        self.queue.lock().push_back(job);
        self.notify.notify_one();
    }

    /// Try to pop the next job without blocking.
    pub(super) fn try_pop(&self) -> Option<T> {
        self.queue.lock().pop_front()
    }

    /// Current number of pending jobs (for monitoring / metrics).
    pub(super) fn pending(&self) -> usize {
        self.queue.lock().len()
    }
}
