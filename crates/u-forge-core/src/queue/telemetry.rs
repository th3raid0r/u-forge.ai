//! Race-safe, content-free inference queue counters and latency summaries.

use std::sync::atomic::{AtomicU64, Ordering};

use super::lifecycle::InferenceError;

/// Bounded latency accumulator. Values saturate instead of wrapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencySummary {
    pub samples: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl LatencySummary {
    pub fn mean_us(self) -> u64 {
        self.total_us.checked_div(self.samples).unwrap_or(0)
    }
}

/// Monotonic queue lifecycle totals. No request content is retained.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueCounters {
    pub submitted: u64,
    pub started: u64,
    pub succeeded: u64,
    pub provider_failed: u64,
    pub cancelled_pending: u64,
    pub cancelled_active: u64,
    pub timed_out: u64,
    pub superseded: u64,
    pub worker_dropped: u64,
    pub retries: u64,
    pub steals: u64,
    pub queue_wait: LatencySummary,
    pub service_time: LatencySummary,
}

#[derive(Debug, Default)]
pub(crate) struct QueueMetrics {
    submitted: AtomicU64,
    started: AtomicU64,
    succeeded: AtomicU64,
    provider_failed: AtomicU64,
    cancelled_pending: AtomicU64,
    cancelled_active: AtomicU64,
    timed_out: AtomicU64,
    superseded: AtomicU64,
    worker_dropped: AtomicU64,
    retries: AtomicU64,
    steals: AtomicU64,
    queue_wait_samples: AtomicU64,
    queue_wait_total_us: AtomicU64,
    queue_wait_max_us: AtomicU64,
    service_samples: AtomicU64,
    service_total_us: AtomicU64,
    service_max_us: AtomicU64,
}

fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

fn add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

impl QueueMetrics {
    pub(super) fn submitted(&self) {
        increment(&self.submitted);
    }

    pub(super) fn started(&self, queue_wait_us: u64, stolen: bool) {
        increment(&self.started);
        increment(&self.queue_wait_samples);
        add(&self.queue_wait_total_us, queue_wait_us);
        self.queue_wait_max_us
            .fetch_max(queue_wait_us, Ordering::Relaxed);
        if stolen {
            increment(&self.steals);
        }
    }

    pub(super) fn retry(&self) {
        increment(&self.retries);
    }

    pub(super) fn worker_dropped(&self) {
        increment(&self.worker_dropped);
    }

    pub(super) fn cancelled_pending(&self, error: &InferenceError) {
        increment(&self.cancelled_pending);
        self.record_cancellation_kind(error);
    }

    pub(super) fn finished(&self, result: &Result<(), &InferenceError>, service_us: u64) {
        increment(&self.service_samples);
        add(&self.service_total_us, service_us);
        self.service_max_us.fetch_max(service_us, Ordering::Relaxed);
        match result {
            Ok(()) => increment(&self.succeeded),
            Err(InferenceError::Cancelled) => increment(&self.cancelled_active),
            Err(InferenceError::Superseded) => {
                increment(&self.cancelled_active);
                increment(&self.superseded);
            }
            Err(InferenceError::TimedOut { .. }) => {
                increment(&self.cancelled_active);
                increment(&self.timed_out);
            }
            Err(InferenceError::ProviderFailed { .. })
            | Err(InferenceError::CapabilityUnavailable { .. }) => {
                increment(&self.provider_failed);
            }
            Err(InferenceError::WorkerDropped) => increment(&self.worker_dropped),
        }
    }

    fn record_cancellation_kind(&self, error: &InferenceError) {
        match error {
            InferenceError::Superseded => increment(&self.superseded),
            InferenceError::TimedOut { .. } => increment(&self.timed_out),
            _ => {}
        }
    }

    pub(super) fn snapshot(&self) -> QueueCounters {
        QueueCounters {
            submitted: self.submitted.load(Ordering::Relaxed),
            started: self.started.load(Ordering::Relaxed),
            succeeded: self.succeeded.load(Ordering::Relaxed),
            provider_failed: self.provider_failed.load(Ordering::Relaxed),
            cancelled_pending: self.cancelled_pending.load(Ordering::Relaxed),
            cancelled_active: self.cancelled_active.load(Ordering::Relaxed),
            timed_out: self.timed_out.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            worker_dropped: self.worker_dropped.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            steals: self.steals.load(Ordering::Relaxed),
            queue_wait: LatencySummary {
                samples: self.queue_wait_samples.load(Ordering::Relaxed),
                total_us: self.queue_wait_total_us.load(Ordering::Relaxed),
                max_us: self.queue_wait_max_us.load(Ordering::Relaxed),
            },
            service_time: LatencySummary {
                samples: self.service_samples.load(Ordering::Relaxed),
                total_us: self.service_total_us.load(Ordering::Relaxed),
                max_us: self.service_max_us.load(Ordering::Relaxed),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{InferenceResult, TimeoutClass};

    #[test]
    fn pending_terminal_matrix_records_one_classification_per_submission() {
        let cases = [
            (InferenceError::Cancelled, 0, 0),
            (InferenceError::Superseded, 1, 0),
            (
                InferenceError::TimedOut {
                    class: TimeoutClass::QueueWait,
                },
                0,
                1,
            ),
        ];

        for (error, superseded, timed_out) in cases {
            let metrics = QueueMetrics::default();
            metrics.submitted();
            metrics.cancelled_pending(&error);

            let counters = metrics.snapshot();
            assert_eq!(counters.submitted, 1);
            assert_eq!(counters.started, 0);
            assert_eq!(counters.cancelled_pending, 1);
            assert_eq!(counters.cancelled_active, 0);
            assert_eq!(counters.superseded, superseded);
            assert_eq!(counters.timed_out, timed_out);
            assert_eq!(counters.queue_wait.samples, 0);
            assert_eq!(counters.service_time.samples, 0);
        }
    }

    #[test]
    fn started_terminal_matrix_records_one_service_transition() {
        struct Case {
            result: InferenceResult<()>,
            succeeded: u64,
            provider_failed: u64,
            cancelled_active: u64,
            timed_out: u64,
            superseded: u64,
            worker_dropped: u64,
        }

        let cases = [
            Case {
                result: Ok(()),
                succeeded: 1,
                provider_failed: 0,
                cancelled_active: 0,
                timed_out: 0,
                superseded: 0,
                worker_dropped: 0,
            },
            Case {
                result: Err(InferenceError::ProviderFailed {
                    message: "provider failed".into(),
                }),
                succeeded: 0,
                provider_failed: 1,
                cancelled_active: 0,
                timed_out: 0,
                superseded: 0,
                worker_dropped: 0,
            },
            Case {
                result: Err(InferenceError::Cancelled),
                succeeded: 0,
                provider_failed: 0,
                cancelled_active: 1,
                timed_out: 0,
                superseded: 0,
                worker_dropped: 0,
            },
            Case {
                result: Err(InferenceError::Superseded),
                succeeded: 0,
                provider_failed: 0,
                cancelled_active: 1,
                timed_out: 0,
                superseded: 1,
                worker_dropped: 0,
            },
            Case {
                result: Err(InferenceError::TimedOut {
                    class: TimeoutClass::Provider,
                }),
                succeeded: 0,
                provider_failed: 0,
                cancelled_active: 1,
                timed_out: 1,
                superseded: 0,
                worker_dropped: 0,
            },
            Case {
                result: Err(InferenceError::WorkerDropped),
                succeeded: 0,
                provider_failed: 0,
                cancelled_active: 0,
                timed_out: 0,
                superseded: 0,
                worker_dropped: 1,
            },
            Case {
                result: Err(InferenceError::CapabilityUnavailable { capability: "test" }),
                succeeded: 0,
                provider_failed: 1,
                cancelled_active: 0,
                timed_out: 0,
                superseded: 0,
                worker_dropped: 0,
            },
        ];

        for case in cases {
            let metrics = QueueMetrics::default();
            metrics.submitted();
            metrics.started(17, false);
            metrics.finished(&case.result.as_ref().map(|_| ()), 23);

            let counters = metrics.snapshot();
            assert_eq!(counters.submitted, 1);
            assert_eq!(counters.started, 1);
            assert_eq!(counters.succeeded, case.succeeded);
            assert_eq!(counters.provider_failed, case.provider_failed);
            assert_eq!(counters.cancelled_active, case.cancelled_active);
            assert_eq!(counters.timed_out, case.timed_out);
            assert_eq!(counters.superseded, case.superseded);
            assert_eq!(counters.worker_dropped, case.worker_dropped);
            assert_eq!(counters.queue_wait.samples, 1);
            assert_eq!(counters.queue_wait.total_us, 17);
            assert_eq!(counters.service_time.samples, 1);
            assert_eq!(counters.service_time.total_us, 23);
            assert_eq!(
                counters.started,
                counters.succeeded
                    + counters.provider_failed
                    + counters.cancelled_active
                    + counters.worker_dropped
            );
        }
    }
}
