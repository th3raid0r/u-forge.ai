//! Weighted embedding dispatcher — routes jobs to the worker with the
//! shortest predicted completion time, with work-stealing for idle workers.
//!
//! # Dispatch (submit-time routing)
//!
//! Each worker slot tracks an EWMA of its job duration.  When a job arrives,
//! the dispatcher picks the worker with the lowest predicted completion time:
//!
//! ```text
//! cost(worker) = (pending_jobs + 1) × ewma_duration_us
//! ```
//!
//! When no timing data is available yet (`ewma_us == 0`), cost falls back to
//! `pending_jobs` so burst jobs spread evenly before warmup completes.
//!
//! The static `weight` field is used only as a tiebreaker when costs are equal.
//!
//! # Work stealing
//!
//! A fast device (GPU) may drain its local queue long before a slow device
//! (NPU) drains its backlog.  Without stealing, the fast device would sleep
//! while the slow one grinds on for tens of seconds.
//!
//! Whenever a worker finishes a job and finds its own queue empty, it calls
//! [`WeightedEmbedDispatcher::steal_from_busiest`] to grab one job from the
//! most-loaded other worker's queue.  The steal loop runs without sleeping, so
//! the fast worker keeps processing until all queues are empty.
//!
//! A `global_notify` is also broadcast on every `submit()`.  Idle workers
//! listen on both their own queue Notify and the global one, so they wake
//! immediately when any new work arrives — even if it lands in another
//! worker's queue.
//!
//! # EWMA update rule (in `run_embed_worker`)
//!
//! After each job completes (success or final retry failure):
//!
//! ```text
//! new_ewma = old_ewma / 2 + elapsed_us / 2        (α = 0.5)
//! ```
//!
//! On the first sample (`old_ewma == 0`), `new_ewma = elapsed_us` directly.
//!
//! # Priority order (default weights — tiebreaker only)
//!
//! | Device | Default weight |
//! |--------|---------------|
//! | NPU    | 100           |
//! | GPU    | 50            |
//! | CPU    | 10            |

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use tokio::sync::Notify;

use super::jobs::{EmbedJob, WorkQueue};

// ── WeightedWorkerSlot ────────────────────────────────────────────────────────

struct WeightedWorkerSlot {
    queue: Arc<WorkQueue<EmbedJob>>,
    weight: u32,
    /// Human-readable device name (e.g. "NPU", "GPU").  Emitted as a span
    /// field (`selected_worker_id`) by [`WeightedEmbedDispatcher::submit`].
    name: String,
    /// Idle flag shared with the worker task.  Retained for future work-stealing
    /// enhancements — e.g. preferring to steal *to* idle workers rather than
    /// relying purely on EWMA cost comparison.
    #[allow(dead_code)]
    idle: Arc<AtomicBool>,
    /// EWMA job duration in microseconds.  `0` = no completed job yet.
    ewma_us: Arc<AtomicU64>,
}

// ── WeightedEmbedDispatcher ───────────────────────────────────────────────────

/// Weighted dispatcher for embedding jobs.
///
/// Construct via [`WeightedEmbedDispatcher::new`], register workers with
/// [`add_worker`](Self::add_worker), wrap in an `Arc`, then pass to both
/// [`InferenceQueue`](super::dispatch::InferenceQueue) and each
/// [`run_embed_worker`](super::workers::run_embed_worker) task.
pub(super) struct WeightedEmbedDispatcher {
    workers: Vec<WeightedWorkerSlot>,
    /// Broadcast on every `submit()`.  Idle workers sleep on this in addition
    /// to their per-queue Notify, so they wake immediately when work lands in
    /// *any* worker's queue — enabling work stealing.
    pub(super) global_notify: Arc<Notify>,
}

impl WeightedEmbedDispatcher {
    pub(super) fn new() -> Self {
        Self {
            workers: Vec::new(),
            global_notify: Arc::new(Notify::new()),
        }
    }

    /// Register a new worker.
    ///
    /// Returns `(queue, idle_flag, ewma_us)` — pass all three to
    /// [`run_embed_worker`](super::workers::run_embed_worker).
    pub(super) fn add_worker(
        &mut self,
        weight: u32,
        name: impl Into<String>,
    ) -> (Arc<WorkQueue<EmbedJob>>, Arc<AtomicBool>, Arc<AtomicU64>) {
        let queue = Arc::new(WorkQueue::<EmbedJob>::new());
        let idle = Arc::new(AtomicBool::new(false));
        let ewma_us = Arc::new(AtomicU64::new(0));

        self.workers.push(WeightedWorkerSlot {
            queue: Arc::clone(&queue),
            weight,
            name: name.into(),
            idle: Arc::clone(&idle),
            ewma_us: Arc::clone(&ewma_us),
        });

        (queue, idle, ewma_us)
    }

    /// Submit a job to the worker with the lowest predicted completion time.
    ///
    /// Returns the name of the selected worker for tracing span recording.
    /// Also fires `global_notify` so any idle worker can wake and steal if
    /// the chosen worker turns out to be slower than the idle one.
    pub(super) fn submit(&self, job: EmbedJob) -> &str {
        if self.workers.is_empty() {
            return "";
        }

        let target = self
            .workers
            .iter()
            .min_by(|a, b| {
                let ca = estimated_cost(a);
                let cb = estimated_cost(b);
                ca.cmp(&cb).then(b.weight.cmp(&a.weight))
            })
            .expect("workers is non-empty");

        target.queue.push(job);
        // Wake any idle worker so it can steal if the chosen worker is slow.
        self.global_notify.notify_one();
        &target.name
    }

    /// Try to steal one job from the most-loaded worker other than `my_queue`.
    ///
    /// Returns `None` if all other queues are empty.  There is a benign TOCTOU
    /// race between the `pending()` check and `try_pop()`; the worst case is a
    /// spurious `None` that causes the caller to check again next iteration.
    pub(super) fn steal_from_busiest(
        &self,
        my_queue: &Arc<WorkQueue<EmbedJob>>,
    ) -> Option<EmbedJob> {
        let target = self
            .workers
            .iter()
            .filter(|w| !Arc::ptr_eq(&w.queue, my_queue))
            .max_by_key(|w| w.queue.pending())?;

        target.queue.try_pop()
    }

    /// Total pending embedding jobs across all worker queues.
    pub(super) fn pending(&self) -> usize {
        self.workers.iter().map(|w| w.queue.pending()).sum()
    }
}

/// Predicted time (μs) until a newly dispatched job would complete.
fn estimated_cost(slot: &WeightedWorkerSlot) -> u64 {
    let ewma = slot.ewma_us.load(Ordering::Relaxed);
    let pending = slot.queue.pending() as u64;
    if ewma == 0 {
        pending
    } else {
        (pending + 1) * ewma
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use tokio::sync::oneshot;

    use super::*;
    use crate::queue::jobs::EmbedJob;

    fn make_job() -> (
        EmbedJob,
        tokio::sync::oneshot::Receiver<anyhow::Result<Vec<f32>>>,
    ) {
        let (tx, rx) = oneshot::channel();
        (
            EmbedJob {
                text: "test".into(),
                response: tx,
            },
            rx,
        )
    }

    #[test]
    fn test_empty_dispatcher_pending_is_zero() {
        let d = WeightedEmbedDispatcher::new();
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn test_add_worker_returns_queue_idle_and_ewma() {
        let mut d = WeightedEmbedDispatcher::new();
        let (_q, idle, ewma) = d.add_worker(100, "NPU");
        assert!(!idle.load(Ordering::Relaxed));
        assert_eq!(ewma.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_single_worker_receives_job() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q, _idle, _ewma) = d.add_worker(100, "NPU");
        let (job, _rx) = make_job();
        d.submit(job);
        assert_eq!(q.pending(), 1);
        assert_eq!(d.pending(), 1);
    }

    // ── Pre-warmup (ewma == 0) ────────────────────────────────────────────────

    #[test]
    fn test_no_data_prefers_highest_weight_when_all_empty() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        let (job, _) = make_job();
        d.submit(job);
        assert_eq!(q_npu.pending(), 1, "NPU wins weight tiebreak");
        assert_eq!(q_gpu.pending(), 0);
    }

    #[test]
    fn test_no_data_burst_spreads_across_workers() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        let (j1, _) = make_job();
        let (j2, _) = make_job();
        d.submit(j1);
        d.submit(j2);
        assert_eq!(q_npu.pending(), 1);
        assert_eq!(q_gpu.pending(), 1, "burst spreads");
    }

    #[test]
    fn test_no_data_routes_to_least_loaded() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        for _ in 0..3 {
            let (j, _) = make_job();
            q_npu.push(j);
        }
        let (j, _) = make_job();
        q_gpu.push(j);
        let (new_job, _) = make_job();
        d.submit(new_job);
        assert_eq!(q_npu.pending(), 3);
        assert_eq!(q_gpu.pending(), 2, "GPU wins (1 < 3)");
    }

    // ── Post-warmup (ewma > 0) ────────────────────────────────────────────────

    #[test]
    fn test_faster_worker_wins_when_both_empty() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, ewma_npu) = d.add_worker(100, "NPU");
        let (q_gpu, _, ewma_gpu) = d.add_worker(50, "GPU");
        ewma_npu.store(500_000, Ordering::Relaxed);
        ewma_gpu.store(50_000, Ordering::Relaxed);
        let (job, _) = make_job();
        d.submit(job);
        assert_eq!(q_npu.pending(), 0);
        assert_eq!(q_gpu.pending(), 1, "GPU wins (faster)");
    }

    #[test]
    fn test_npu_takes_overflow_at_equilibrium() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, ewma_npu) = d.add_worker(100, "NPU");
        let (q_gpu, _, ewma_gpu) = d.add_worker(50, "GPU");
        ewma_npu.store(500_000, Ordering::Relaxed);
        ewma_gpu.store(50_000, Ordering::Relaxed);
        for _ in 0..9 {
            let (j, _) = make_job();
            q_gpu.push(j);
        }
        let (job, _) = make_job();
        d.submit(job);
        // cost(NPU) = 1*500k, cost(GPU) = 10*50k = 500k → tie → NPU wins weight
        assert_eq!(q_npu.pending(), 1, "NPU takes overflow at equilibrium");
        assert_eq!(q_gpu.pending(), 9);
    }

    #[test]
    fn test_npu_does_not_take_overflow_before_saturation() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, ewma_npu) = d.add_worker(100, "NPU");
        let (q_gpu, _, ewma_gpu) = d.add_worker(50, "GPU");
        ewma_npu.store(500_000, Ordering::Relaxed);
        ewma_gpu.store(50_000, Ordering::Relaxed);
        for _ in 0..8 {
            let (j, _) = make_job();
            q_gpu.push(j);
        }
        let (job, _) = make_job();
        d.submit(job);
        // cost(GPU) = 9*50k = 450k < 500k = cost(NPU)
        assert_eq!(q_npu.pending(), 0);
        assert_eq!(q_gpu.pending(), 9, "GPU still cheaper");
    }

    // ── Work stealing ─────────────────────────────────────────────────────────

    #[test]
    fn test_steal_from_busiest_returns_job_from_other_queue() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        // Put two jobs in NPU's queue
        for _ in 0..2 {
            let (j, _) = make_job();
            q_npu.push(j);
        }
        // GPU steals one
        let stolen = d.steal_from_busiest(&q_gpu);
        assert!(stolen.is_some(), "GPU should steal from NPU");
        assert_eq!(q_npu.pending(), 1);
    }

    #[test]
    fn test_steal_does_not_steal_from_self() {
        let mut d = WeightedEmbedDispatcher::new();
        let (_q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        // Only GPU has work
        for _ in 0..2 {
            let (j, _) = make_job();
            q_gpu.push(j);
        }
        // GPU tries to steal from others — should find nothing (NPU is empty)
        let stolen = d.steal_from_busiest(&q_gpu);
        assert!(stolen.is_none(), "cannot steal from self");
        assert_eq!(q_gpu.pending(), 2, "GPU queue unchanged");
    }

    #[test]
    fn test_steal_returns_none_when_others_empty() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        // Both empty
        assert!(d.steal_from_busiest(&q_gpu).is_none());
        assert!(d.steal_from_busiest(&q_npu).is_none());
    }

    #[test]
    fn test_steal_picks_most_loaded_queue() {
        let mut d = WeightedEmbedDispatcher::new();
        let (q_npu, _, _) = d.add_worker(100, "NPU");
        let (q_gpu, _, _) = d.add_worker(50, "GPU");
        let (q_cpu, _, _) = d.add_worker(10, "CPU");
        // NPU: 5 jobs, GPU: 2 jobs
        for _ in 0..5 {
            let (j, _) = make_job();
            q_npu.push(j);
        }
        for _ in 0..2 {
            let (j, _) = make_job();
            q_gpu.push(j);
        }
        // CPU steals — should take from NPU (most loaded)
        let _ = d.steal_from_busiest(&q_cpu);
        assert_eq!(q_npu.pending(), 4, "NPU lost one job to CPU steal");
        assert_eq!(q_gpu.pending(), 2, "GPU unchanged");
    }
}
