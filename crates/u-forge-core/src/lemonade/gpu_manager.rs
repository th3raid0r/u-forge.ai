//! GPU resource manager for enforcing the GPU sharing policy.
//!
//! See the [module-level docs](super) for the full policy description.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tracing::{debug, info};

/// The current exclusive workload occupying the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuWorkload {
    /// No active workload — GPU is free.
    Idle,
    /// Whisper STT is running. LLM requests will queue; further STT requests are rejected.
    SttActive,
    /// LLM inference is running. STT requests are rejected immediately.
    LlmActive,
}

impl std::fmt::Display for GpuWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuWorkload::Idle => write!(f, "Idle"),
            GpuWorkload::SttActive => write!(f, "STT active"),
            GpuWorkload::LlmActive => write!(f, "LLM active"),
        }
    }
}

/// Enforces the GPU sharing policy between the latency-sensitive STT workload and
/// the throughput-oriented LLM inference workload.
///
/// Always construct via [`GpuResourceManager::new`], which returns an `Arc<Self>`
/// suitable for sharing across providers.
///
/// # Policy Summary
///
/// | Request | GPU state | Outcome           |
/// |---------|-----------|-------------------|
/// | STT     | Idle      | Acquired          |
/// | STT     | LlmActive | **Error** (blocked) |
/// | STT     | SttActive | Error (busy)      |
/// | LLM     | Idle      | Acquired          |
/// | LLM     | SttActive | **Queued** (waits) |
/// | LLM     | LlmActive | Queued (serialised)|
pub struct GpuResourceManager {
    workload: Mutex<GpuWorkload>,
    /// Notified whenever the workload transitions to [`GpuWorkload::Idle`].
    notify: Notify,
}

impl std::fmt::Debug for GpuResourceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuResourceManager")
            .field("workload", &*self.workload.lock())
            .finish()
    }
}

impl GpuResourceManager {
    /// Create a new, idle GPU resource manager wrapped in `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            workload: Mutex::new(GpuWorkload::Idle),
            notify: Notify::new(),
        })
    }

    /// Snapshot of the current GPU workload state.
    pub fn current_workload(&self) -> GpuWorkload {
        self.workload.lock().clone()
    }

    /// Attempt to acquire the GPU for STT work.
    ///
    /// This is a **non-blocking** call:
    /// - Returns `Ok(SttGuard)` when the GPU is idle.
    /// - Returns `Err` immediately if the GPU is busy with LLM inference or another STT
    ///   session.  Callers should surface this as a user-visible "try again later" message.
    pub fn begin_stt(self: &Arc<Self>) -> Result<SttGuard> {
        let mut w = self.workload.lock();
        match *w {
            GpuWorkload::Idle => {
                *w = GpuWorkload::SttActive;
                debug!("GPU acquired for STT");
                Ok(SttGuard {
                    manager: Arc::clone(self),
                })
            }
            GpuWorkload::LlmActive => Err(anyhow!(
                "GPU busy: LLM inference is in progress. \
                 STT is latency-sensitive and cannot be queued — retry once the \
                 LLM request completes."
            )),
            GpuWorkload::SttActive => {
                Err(anyhow!("GPU busy: an STT session is already in progress."))
            }
        }
    }

    /// Acquire the GPU for LLM inference, **waiting** if the GPU is currently busy.
    ///
    /// This is an **async** call that suspends the calling task when:
    /// - STT is active (queues until the STT session ends), or
    /// - Another LLM is active (serialises requests).
    ///
    /// It will never return an error — it simply waits for the GPU to become available.
    pub async fn begin_llm(self: &Arc<Self>) -> LlmGuard {
        loop {
            // Scope: hold the parking_lot mutex only briefly, never across .await.
            {
                let mut w = self.workload.lock();
                if *w == GpuWorkload::Idle {
                    *w = GpuWorkload::LlmActive;
                    debug!("GPU acquired for LLM inference");
                    return LlmGuard {
                        manager: Arc::clone(self),
                    };
                }
                let reason = w.clone();
                drop(w); // release before .await
                match reason {
                    GpuWorkload::SttActive => {
                        info!("LLM request queued: waiting for active STT session to complete");
                    }
                    GpuWorkload::LlmActive => {
                        debug!(
                            "LLM request queued: waiting for previous LLM inference to complete"
                        );
                    }
                    GpuWorkload::Idle => unreachable!(),
                }
            }
            // Suspend and wake up when a guard is dropped.
            self.notify.notified().await;
        }
    }

    /// Internal: release the GPU and wake all waiters.
    fn release(&self) {
        let mut w = self.workload.lock();
        *w = GpuWorkload::Idle;
        // Drop the lock before notifying so waiters don't spin on a held lock.
        drop(w);
        self.notify.notify_waiters();
    }
}

/// RAII guard that holds the GPU in [`GpuWorkload::SttActive`] mode.
///
/// When this value is dropped (normally or on error), the GPU is returned to
/// [`GpuWorkload::Idle`] and any queued LLM requests are woken.
pub struct SttGuard {
    manager: Arc<GpuResourceManager>,
}

impl std::fmt::Debug for SttGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttGuard").finish()
    }
}

impl Drop for SttGuard {
    fn drop(&mut self) {
        debug!("GPU released from STT — notifying waiters");
        self.manager.release();
    }
}

/// RAII guard that holds the GPU in [`GpuWorkload::LlmActive`] mode.
///
/// When dropped, the GPU returns to [`GpuWorkload::Idle`] and the next queued
/// request (STT or LLM) is woken.
pub struct LlmGuard {
    manager: Arc<GpuResourceManager>,
}

impl std::fmt::Debug for LlmGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmGuard").finish()
    }
}

impl Drop for LlmGuard {
    fn drop(&mut self) {
        debug!("GPU released from LLM inference — notifying waiters");
        self.manager.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gpu_initial_state_is_idle() {
        let gpu = GpuResourceManager::new();
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_stt_acquires_gpu_when_idle() {
        let gpu = GpuResourceManager::new();
        let _guard = gpu
            .begin_stt()
            .expect("Should acquire GPU for STT when idle");
        assert_eq!(gpu.current_workload(), GpuWorkload::SttActive);
    }

    #[tokio::test]
    async fn test_stt_guard_drop_releases_to_idle() {
        let gpu = GpuResourceManager::new();
        {
            let _g = gpu.begin_stt().unwrap();
            assert_eq!(gpu.current_workload(), GpuWorkload::SttActive);
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_llm_acquires_gpu_when_idle() {
        let gpu = GpuResourceManager::new();
        let _guard = gpu.begin_llm().await;
        assert_eq!(gpu.current_workload(), GpuWorkload::LlmActive);
    }

    #[tokio::test]
    async fn test_llm_guard_drop_releases_to_idle() {
        let gpu = GpuResourceManager::new();
        {
            let _g = gpu.begin_llm().await;
            assert_eq!(gpu.current_workload(), GpuWorkload::LlmActive);
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    // ── Unit: GPU resource manager — STT blocking policy ─────────────────────

    #[tokio::test]
    async fn test_stt_blocked_when_llm_active() {
        let gpu = GpuResourceManager::new();
        let _llm = gpu.begin_llm().await;

        let result = gpu.begin_stt();
        assert!(result.is_err(), "STT must be blocked when LLM is active");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LLM inference"),
            "Error should mention LLM: {msg}"
        );
    }

    #[tokio::test]
    async fn test_stt_blocked_when_stt_active() {
        let gpu = GpuResourceManager::new();
        let _stt1 = gpu.begin_stt().expect("First STT should succeed");

        let result = gpu.begin_stt();
        assert!(result.is_err(), "Second concurrent STT must be rejected");
    }

    // ── Unit: GPU resource manager — LLM queuing policy ──────────────────────

    #[tokio::test]
    async fn test_llm_queues_behind_active_stt_and_proceeds_on_release() {
        use tokio::time::{sleep, timeout, Duration};

        let gpu = GpuResourceManager::new();
        let gpu_llm = Arc::clone(&gpu);

        // Hold the GPU for STT.
        let stt_guard = gpu.begin_stt().expect("STT should acquire GPU");

        // Spawn LLM task — it must wait.
        let llm_handle = tokio::spawn(async move {
            let _guard = gpu_llm.begin_llm().await;
            // If we reach here, the GPU is ours.
        });

        // Brief pause to let the LLM task enter the wait loop.
        sleep(Duration::from_millis(50)).await;

        // LLM task must not have completed yet.
        assert!(
            !llm_handle.is_finished(),
            "LLM task should still be waiting for STT to release"
        );

        // Release STT → LLM should be unblocked.
        drop(stt_guard);

        timeout(Duration::from_secs(2), llm_handle)
            .await
            .expect("LLM task should complete within 2 s after STT release")
            .expect("LLM task should not panic");
    }

    #[tokio::test]
    async fn test_multiple_llm_requests_serialise() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::time::{sleep, Duration};

        let gpu = GpuResourceManager::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..4 {
            let g = Arc::clone(&gpu);
            let c = Arc::clone(&counter);
            handles.push(tokio::spawn(async move {
                let _guard = g.begin_llm().await;
                // Only one task should be in this critical section at a time.
                let prev = c.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(5)).await;
                c.fetch_sub(1, Ordering::SeqCst);
                // If serialisation is working, prev should always be 0.
                assert_eq!(prev, 0, "Concurrent LLM requests must not overlap");
            }));
        }

        for h in handles {
            h.await.expect("Task should not panic");
        }

        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_gpu_idle_after_sequential_stt_then_llm() {
        let gpu = GpuResourceManager::new();

        {
            let _stt = gpu.begin_stt().unwrap();
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);

        {
            let _llm = gpu.begin_llm().await;
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    // ── Integration: GPU policy end-to-end (requires LEMONADE_URL) ───────────

    #[tokio::test]
    async fn test_llm_queues_behind_simulated_stt_integration() {
        let url = crate::test_helpers::require_integration_url!();
        use tokio::time::{sleep, timeout, Duration};

        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url).await.unwrap();
        let cfg = crate::config::AppConfig::default();
        let selector = crate::lemonade::ModelSelector::new(&catalog, &cfg.models, &cfg.embedding);
        let llm = selector.select_llm_models().into_iter().next()
            .expect("No LLM model found in catalog");
        let gpu = GpuResourceManager::new();
        let chat = crate::lemonade::LemonadeChatProvider::new(&url, &llm.model_id, Some(Arc::clone(&gpu)));

        // Simulate an active STT session (no real audio upload needed).
        let stt_guard = gpu
            .begin_stt()
            .expect("STT guard should succeed when GPU is idle");

        let chat2 = chat.clone();
        let llm_task = tokio::spawn(async move { chat2.ask("Say: ready").await });

        // Give the LLM task time to enter its wait loop.
        sleep(Duration::from_millis(100)).await;
        assert!(
            !llm_task.is_finished(),
            "LLM must still be queued behind STT"
        );

        // Release the simulated STT session.
        drop(stt_guard);

        let result = timeout(Duration::from_secs(60), llm_task)
            .await
            .expect("LLM task should complete within 60 s after STT release")
            .expect("LLM task should not panic")
            .expect("LLM chat should succeed");

        assert!(
            !result.is_empty(),
            "LLM should return a non-empty response after queuing"
        );
    }

    #[tokio::test]
    async fn test_stt_blocked_during_simulated_llm_integration() {
        // This is purely a policy test — no server needed, no real LLM request.
        let gpu = GpuResourceManager::new();
        let _llm_guard = gpu.begin_llm().await;

        let result = gpu.begin_stt();
        assert!(
            result.is_err(),
            "STT must be rejected when LLM guard is held"
        );
        assert!(
            result.unwrap_err().to_string().contains("LLM inference"),
            "Error message should mention LLM inference"
        );
    }
}
