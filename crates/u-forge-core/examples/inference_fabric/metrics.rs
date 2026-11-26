use std::{path::Path, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Serialize;
use u_forge_core::{
    EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
    lemonade::{LemonadeProvider, LemonadeRerankProvider, RerankDocument, RerankProvider},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Batch,
    Embed,
    Rerank,
}

#[derive(Clone, Copy)]
pub struct Span {
    pub start: Instant,
    pub end: Instant,
}

impl Span {
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn ms(self) -> f64 {
        ms(self.start, self.end)
    }
}

#[derive(Clone, Copy)]
pub struct Call {
    pub span: Span,
    pub kind: Kind,
    pub route: usize,
    pub items: usize,
    pub text_bytes: usize,
    pub ok: bool,
}

pub type Trace = Arc<Mutex<Vec<Call>>>;

pub struct Metered<P> {
    pub inner: P,
    pub route: usize,
    pub trace: Trace,
}

impl<P> Metered<P> {
    async fn record<T>(
        &self,
        kind: Kind,
        items: usize,
        text_bytes: usize,
        call: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let start = Instant::now();
        let result = call.await;
        self.trace.lock().push(Call {
            span: Span {
                start,
                end: Instant::now(),
            },
            kind,
            route: self.route,
            items,
            text_bytes,
            ok: result.is_ok(),
        });
        result
    }
}

#[async_trait]
impl EmbeddingProvider for Metered<LemonadeProvider> {
    async fn embed(&self, input: &str) -> Result<Vec<f32>> {
        self.record(Kind::Embed, 1, input.len(), self.inner.embed(input))
            .await
    }

    async fn embed_batch(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.record(Kind::Batch, input.len(), 0, self.inner.embed_batch(input))
            .await
    }

    fn dimensions(&self) -> Result<usize> {
        self.inner.dimensions()
    }

    fn max_tokens(&self) -> Result<usize> {
        self.inner.max_tokens()
    }

    fn model_info(&self) -> Option<EmbeddingModelInfo> {
        self.inner.model_info()
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        self.inner.provider_type()
    }
}

#[async_trait]
impl RerankProvider for Metered<LemonadeRerankProvider> {
    async fn rerank(
        &self,
        query: &str,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>> {
        self.record(
            Kind::Rerank,
            documents.len(),
            query.len() + documents.iter().map(String::len).sum::<usize>(),
            self.inner.rerank(query, documents, top_n),
        )
        .await
    }
}

pub fn ms(start: Instant, end: Instant) -> f64 {
    (end - start).as_secs_f64() * 1000.0
}

#[derive(Serialize)]
pub struct Spread {
    n: usize,
    min: Option<f64>,
    p50: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    max: Option<f64>,
}

impl Spread {
    pub fn of(mut values: Vec<f64>) -> Self {
        values.sort_unstable_by(f64::total_cmp);
        let n = values.len();
        let percentile = |p: usize| {
            n.checked_mul(p)
                .unwrap()
                .div_ceil(100)
                .checked_sub(1)
                .map(|i| values[i])
        };
        Self {
            n,
            min: values.first().copied(),
            p50: percentile(50),
            p95: percentile(95),
            p99: percentile(99),
            max: values.last().copied(),
        }
    }
}

pub fn read_number(path: &Path) -> Result<f64> {
    let value: f64 = std::fs::read_to_string(path)?
        .trim()
        .parse()
        .context("expected one numeric counter value")?;
    anyhow::ensure!(value.is_finite(), "counter value is not finite");
    Ok(value)
}

/// Compare the same delivered delta ordinals. Count each intersecting gap at
/// most once for this union of windows, and never let a faster gap erase a stall.
pub fn added_stall(
    baseline: &[f64],
    treatment: &[f64],
    stream: &[Instant],
    windows: &[Span],
) -> Option<f64> {
    if stream.len() < 2
        || baseline.len() != treatment.len()
        || treatment.len() + 1 != stream.len()
        || windows.is_empty()
        || windows
            .iter()
            .any(|w| w.start < stream[0] || w.end > stream[stream.len() - 1])
    {
        return None;
    }
    Some(
        stream
            .windows(2)
            .zip(baseline.iter().zip(treatment))
            .filter_map(|(gap, (base, treated))| {
                let gap = Span {
                    start: gap[0],
                    end: gap[1],
                };
                windows
                    .iter()
                    .any(|w| gap.overlaps(*w))
                    .then_some((treated - base).max(0.0))
            })
            .sum(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn matching_counts_positive_excess_once_per_gap_and_requires_complete_overlap() {
        let start = Instant::now();
        let t = |ms| start + Duration::from_millis(ms);
        let stream = [t(0), t(10), t(61), t(65), t(69)];
        let treated = [10.0, 51.0, 4.0, 4.0];
        let baseline = [10.0; 4];
        let span = |a, b| Span {
            start: t(a),
            end: t(b),
        };
        assert_eq!(
            added_stall(&baseline, &treated, &stream, &[span(8, 40), span(40, 65)]),
            Some(41.0)
        );
        assert_eq!(
            added_stall(&baseline, &treated, &stream, &[span(40, 70)]),
            None
        );
        assert_eq!(
            added_stall(&baseline[..3], &treated, &stream, &[span(40, 60)]),
            None
        );
    }

    #[test]
    fn spread_does_not_turn_no_observations_into_zero_latency() {
        let empty = Spread::of(vec![]);
        assert_eq!(empty.n, 0);
        assert_eq!(empty.p95, None);
        let spread = Spread::of((1..=100).rev().map(f64::from).collect());
        assert_eq!(
            (spread.p50, spread.p95, spread.p99),
            (Some(50.0), Some(95.0), Some(99.0))
        );
    }
}
