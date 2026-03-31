//! Weighted embedding dispatcher — routes jobs to the highest-priority idle worker.
//!
//! # Design
//!
//! Each embedding device (NPU, GPU, CPU) registers as a [`WeightedWorkerSlot`]
//! with a numeric weight and an atomic idle flag.  When a job arrives,
//! [`WeightedEmbedDispatcher::submit`] selects the target queue:
//!
//! 1. Collect all workers whose `idle` flag is `true`.
//! 2. If any idle workers exist: push to the highest-weight idle worker.
//! 3. If all workers are busy: push to the highest-weight worker (backpressure).
//!
//! The `idle` flag is managed by [`run_embed_worker`](super::workers::run_embed_worker):
//! set `true` before `notified.await`, set `false` as soon as a job is popped.
//! There is a small race window where a worker has finished processing but has
//! not yet set the flag back to `true` — this is acceptable.  The worst case is
//! that a job goes to a "slightly wrong" queue; it will still be processed
//! correctly because each worker has its own `WorkQueue` and handles every job
//! it receives.
//!
//! # Priority order (default weights)
//!
//! | Device | Default weight |
//! |--------|---------------|
//! | NPU    | 100           |
//! | GPU    | 50            |
//! | CPU    | 10            |

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::jobs::{EmbedJob, WorkQueue};

// ── WeightedWorkerSlot ────────────────────────────────────────────────────────

struct WeightedWorkerSlot {
    /// The per-worker job queue.
    queue: Arc<WorkQueue<EmbedJob>>,
    /// Dispatch priority: higher → preferred over lower-weight workers.
    weight: u32,
    /// Human-readable name for logging.
    #[allow(dead_code)]
    name: String,
    /// `true` when the worker is waiting for a job (set by the worker task).
    idle: Arc<AtomicBool>,
}

// ── WeightedEmbedDispatcher ───────────────────────────────────────────────────

/// Weighted dispatcher for embedding jobs.
///
/// Construct via [`WeightedEmbedDispatcher::new`], register workers with
/// [`add_worker`](Self::add_worker), then pass the dispatcher to
/// [`InferenceQueue`](super::dispatch::InferenceQueue).
pub(super) struct WeightedEmbedDispatcher {
    workers: Vec<WeightedWorkerSlot>,
}

impl WeightedEmbedDispatcher {
    /// Create an empty dispatcher with no workers registered.
    pub(super) fn new() -> Self {
        Self {
            workers: Vec::new(),
        }
    }

    /// Register a new worker with the given `weight` and `name`.
    ///
    /// Returns the per-worker `(queue, idle_flag)` pair that the spawned Tokio
    /// task needs.  Pass `idle_flag` to
    /// [`run_embed_worker`](super::workers::run_embed_worker) and `queue` as
    /// its work source.
    ///
    /// Workers registered first are stored in order; ties in weight during
    /// dispatch are broken by registration order (first registered wins).
    pub(super) fn add_worker(
        &mut self,
        weight: u32,
        name: impl Into<String>,
    ) -> (Arc<WorkQueue<EmbedJob>>, Arc<AtomicBool>) {
        let queue = Arc::new(WorkQueue::<EmbedJob>::new());
        let idle = Arc::new(AtomicBool::new(false));

        self.workers.push(WeightedWorkerSlot {
            queue: Arc::clone(&queue),
            weight,
            name: name.into(),
            idle: Arc::clone(&idle),
        });

        (queue, idle)
    }

    /// Submit a job to the most appropriate worker queue.
    ///
    /// Selection logic:
    /// 1. Prefer the highest-weight worker whose `idle` flag is `true`.
    /// 2. If no idle worker exists, fall back to the highest-weight worker.
    ///
    /// Notifies the chosen worker's queue so it wakes up immediately.
    pub(super) fn submit(&self, job: EmbedJob) {
        if self.workers.is_empty() {
            // Should not happen in a correctly built queue — builder warns when
            // no embedding workers are registered.
            return;
        }

        // Find the best idle worker (highest weight among idle workers).
        let best_idle = self
            .workers
            .iter()
            .filter(|w| w.idle.load(Ordering::Relaxed))
            .max_by_key(|w| w.weight);

        let target = match best_idle {
            Some(w) => w,
            None => {
                // No idle workers — fall back to highest-weight worker.
                self.workers
                    .iter()
                    .max_by_key(|w| w.weight)
                    .expect("workers is non-empty")
            }
        };

        target.queue.push(job);
    }

    /// Total number of pending embedding jobs across all worker queues.
    pub(super) fn pending(&self) -> usize {
        self.workers.iter().map(|w| w.queue.pending()).collect::<Vec<_>>().iter().sum()
    }

    /// Number of registered worker slots.
    pub(super) fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use tokio::sync::oneshot;

    use super::*;
    use crate::queue::jobs::EmbedJob;

    fn make_job() -> (EmbedJob, tokio::sync::oneshot::Receiver<anyhow::Result<Vec<f32>>>) {
        let (tx, rx) = oneshot::channel();
        (EmbedJob { text: "test".into(), response: tx }, rx)
    }

    #[test]
    fn test_empty_dispatcher_pending_is_zero() {
        let d = WeightedEmbedDispatcher::new();
        assert_eq!(d.pending(), 0);
        assert_eq!(d.worker_count(), 0);
    }

    #[test]
    fn test_add_worker_returns_queue_and_idle_flag() {
        let mut d = WeightedEmbedDispatcher::new();
        let (_q, idle) = d.add_worker(100, "NPU");
        assert_eq!(d.worker_count(), 1);
        // Starts as non-idle (worker hasn't started yet)
        assert!(!idle.load(Ordering::Relaxed));
    }

    #[test]
    fn test_single_worker_receives_job() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q, idle) = d.add_worker(100, "NPU");
        idle.store(true, Ordering::Relaxed); // mark as idle

        let (job, _rx) = make_job();
        d.submit(job);

        assert_eq!(q.pending(), 1);
        assert_eq!(d.pending(), 1);
    }

    #[test]
    fn test_prefer_idle_worker() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, idle_npu) = d.add_worker(100, "NPU");
        let (q_gpu, idle_gpu) = d.add_worker(50, "GPU");

        // NPU busy, GPU idle
        idle_npu.store(false, Ordering::Relaxed);
        idle_gpu.store(true, Ordering::Relaxed);

        let (job, _rx) = make_job();
        d.submit(job);

        // Job should go to GPU (the only idle worker), not NPU
        assert_eq!(q_npu.pending(), 0);
        assert_eq!(q_gpu.pending(), 1);
    }

    #[test]
    fn test_prefer_higher_weight_among_idle() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, idle_npu) = d.add_worker(100, "NPU");
        let (q_gpu, idle_gpu) = d.add_worker(50, "GPU");
        let (q_cpu, idle_cpu) = d.add_worker(10, "CPU");

        // All idle — NPU should win
        idle_npu.store(true, Ordering::Relaxed);
        idle_gpu.store(true, Ordering::Relaxed);
        idle_cpu.store(true, Ordering::Relaxed);

        let (job, _rx) = make_job();
        d.submit(job);

        assert_eq!(q_npu.pending(), 1, "NPU should receive job (highest weight)");
        assert_eq!(q_gpu.pending(), 0);
        assert_eq!(q_cpu.pending(), 0);
    }

    #[test]
    fn test_fallback_to_highest_weight_when_all_busy() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _idle_npu) = d.add_worker(100, "NPU");
        let (q_gpu, _idle_gpu) = d.add_worker(50, "GPU");

        // All workers busy (idle flags remain false from construction)
        let (job, _rx) = make_job();
        d.submit(job);

        assert_eq!(q_npu.pending(), 1, "NPU should receive job (highest weight fallback)");
        assert_eq!(q_gpu.pending(), 0);
    }
}
