//! Internal job types and the `WorkQueue<T>` primitive.

use std::collections::VecDeque;

use anyhow::Result;
use parking_lot::Mutex;
use tokio::sync::{oneshot, Notify};

use crate::lemonade::{ChatCompletionResponse, ChatRequest, KokoroVoice, RerankDocument};

// ── Internal job types ────────────────────────────────────────────────────────

/// A single text embedding job.
pub(super) struct EmbedJob {
    pub(super) text: String,
    pub(super) response: oneshot::Sender<Result<Vec<f32>>>,
}

/// A single audio transcription job.
pub(super) struct TranscribeJob {
    pub(super) audio_bytes: Vec<u8>,
    pub(super) filename: String,
    pub(super) response: oneshot::Sender<Result<String>>,
}

/// A single text-to-speech synthesis job.
pub(super) struct SynthesizeJob {
    pub(super) text: String,
    /// Explicit voice override; `None` uses the provider's default voice.
    pub(super) voice: Option<KokoroVoice>,
    pub(super) response: oneshot::Sender<Result<Vec<u8>>>,
}

/// A single LLM chat-completion job.
pub(super) struct GenerateJob {
    pub(super) request: ChatRequest,
    pub(super) response: oneshot::Sender<Result<ChatCompletionResponse>>,
}

/// A single document reranking job.
pub(super) struct RerankJob {
    pub(super) query: String,
    pub(super) documents: Vec<String>,
    pub(super) top_n: Option<usize>,
    pub(super) response: oneshot::Sender<Result<Vec<RerankDocument>>>,
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
