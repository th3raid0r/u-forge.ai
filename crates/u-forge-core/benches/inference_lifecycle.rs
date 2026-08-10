//! Deterministic evidence scenarios for inference routing and lifecycle policy.
//!
//! The initial evidence run found cold and preseeded heterogeneous routing in
//! the same ~35–36 ms band, and one-worker/two-worker retry recovery in the
//! same ~307–308 ms band. Consequently this feature keeps zero EWMA fallback,
//! fixed bounded backoff, existing worker counts, and existing queue capacity.
//! Re-run this target against future routing changes before changing those
//! policies; wall-clock values are a local baseline, while scenario parity is
//! the decision signal.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use u_forge_core::lemonade::{BuiltProvider, Capability, ProviderSlot};
use u_forge_core::{
    EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
    queue::{InferenceQueue, InferenceQueueBuilder},
};

const DIMS: usize = 8;

struct TimedProvider {
    delay: Duration,
    activation_delay: Duration,
    failures_remaining: AtomicUsize,
}

impl TimedProvider {
    fn stable(delay_ms: u64) -> Self {
        Self {
            delay: Duration::from_millis(delay_ms),
            activation_delay: Duration::ZERO,
            failures_remaining: AtomicUsize::new(0),
        }
    }

    fn retrying(failures: usize) -> Self {
        Self {
            delay: Duration::from_millis(1),
            activation_delay: Duration::ZERO,
            failures_remaining: AtomicUsize::new(failures),
        }
    }

    fn capacity_one_churn() -> Self {
        Self {
            delay: Duration::from_millis(1),
            activation_delay: Duration::from_millis(2),
            failures_remaining: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for TimedProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        tokio::time::sleep(self.activation_delay).await;
        if self
            .failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            anyhow::bail!("deterministic transient failure");
        }
        tokio::time::sleep(self.delay).await;
        Ok(vec![text.len() as f32; DIMS])
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut output = Vec::with_capacity(texts.len());
        for text in texts {
            output.push(self.embed(&text).await?);
        }
        Ok(output)
    }

    fn dimensions(&self) -> Result<usize> {
        Ok(DIMS)
    }

    fn max_tokens(&self) -> Result<usize> {
        Ok(512)
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        EmbeddingProviderType::Lemonade
    }

    fn model_info(&self) -> Option<EmbeddingModelInfo> {
        None
    }
}

fn queue(
    providers: impl IntoIterator<Item = (&'static str, u32, TimedProvider)>,
) -> InferenceQueue {
    providers
        .into_iter()
        .fold(
            InferenceQueueBuilder::new(),
            |builder, (name, weight, provider)| {
                builder.with_provider(BuiltProvider {
                    name: name.to_string(),
                    capability: Capability::Embedding,
                    provider: ProviderSlot::Embedding(Arc::new(provider)),
                    weight,
                })
            },
        )
        .build()
}

async fn batch(queue: &InferenceQueue, count: usize) {
    let texts = (0..count).map(|index| format!("text-{index}")).collect();
    black_box(queue.embed_many(texts).await.unwrap());
}

fn inference_lifecycle(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("inference_lifecycle");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("cold_heterogeneous_routing", |b| {
        b.to_async(&runtime).iter_batched(
            || {
                queue([
                    ("fast", 50, TimedProvider::stable(1)),
                    ("slow", 100, TimedProvider::stable(4)),
                ])
            },
            |queue| async move {
                batch(&queue, 16).await;
                black_box(queue.stats().counters);
            },
            BatchSize::SmallInput,
        );
    });

    let steady = {
        let _guard = runtime.enter();
        queue([
            ("fast", 50, TimedProvider::stable(1)),
            ("slow", 100, TimedProvider::stable(4)),
        ])
    };
    runtime.block_on(batch(&steady, 16));
    group.bench_function("preseeded_heterogeneous_routing", |b| {
        b.to_async(&runtime).iter(|| batch(&steady, 16));
    });
    group.bench_function("steady_state_routing", |b| {
        b.to_async(&runtime).iter(|| batch(&steady, 32));
    });

    let churn = {
        let _guard = runtime.enter();
        queue([("capacity-one", 100, TimedProvider::capacity_one_churn())])
    };
    group.bench_function("capacity_one_load_churn", |b| {
        b.to_async(&runtime).iter(|| batch(&churn, 8));
    });

    group.bench_function("retry_recovery", |b| {
        b.to_async(&runtime).iter_batched(
            || queue([("flaky", 100, TimedProvider::retrying(2))]),
            |queue| async move {
                batch(&queue, 1).await;
                black_box(queue.stats().counters.retries);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("retry_recovery_lockstep", |b| {
        b.to_async(&runtime).iter_batched(
            || {
                queue([
                    ("flaky-a", 100, TimedProvider::retrying(2)),
                    ("flaky-b", 50, TimedProvider::retrying(2)),
                ])
            },
            |queue| async move {
                batch(&queue, 2).await;
                black_box(queue.stats().counters.retries);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("work_stealing", |b| {
        b.to_async(&runtime).iter_batched(
            || {
                queue([
                    ("fast", 10, TimedProvider::stable(1)),
                    ("slow", 100, TimedProvider::stable(8)),
                ])
            },
            |queue| async move {
                batch(&queue, 32).await;
                black_box(queue.stats().counters.steals);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, inference_lifecycle);
criterion_main!(benches);
