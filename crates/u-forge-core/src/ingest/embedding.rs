//! Batch embedding helpers.
//!
//! [`embed_all_chunks`] embeds every un-embedded text chunk in a
//! [`KnowledgeGraph`] using an [`InferenceQueue`], for both standard
//! (768-dim) and high-quality (4096-dim) targets.
//!
//! [`build_hq_embed_queue`] is a convenience constructor that builds a
//! single-worker [`InferenceQueue`] for the first high-quality embedding model
//! selected by [`ModelSelector`] from a live [`LemonadeServerCatalog`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use tracing::{info, warn};

use crate::HIGH_QUALITY_EMBEDDING_DIMENSIONS;
use crate::KnowledgeGraph;
use crate::config::AppConfig;
use crate::lemonade::catalog::LemonadeServerCatalog;
use crate::lemonade::provider_factory::{BuiltProvider, Capability, ProviderFactory};
use crate::lemonade::selector::{ModelSelector, QualityTier};
use crate::queue::{CancellationToken, InferenceQueue, InferenceQueueBuilder};

/// Which embedding index to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingTarget {
    /// Standard 768-dim embeddings (`chunks_vec`).
    Standard,
    /// High-quality 4096-dim embeddings (`chunks_vec_hq`).
    HighQuality,
}

/// Outcome of an [`embed_all_chunks`] call.
#[derive(Debug)]
pub struct EmbeddingResult {
    /// Chunks successfully embedded and stored.
    pub stored: usize,
    /// Chunks that failed to store (logged individually).
    pub skipped: usize,
    /// Total chunks that were candidates for embedding.
    pub total: usize,
}

/// Aggregated outcome from an [`EmbeddingPlan`] execution.
#[derive(Debug, Default)]
pub struct EmbeddingOutcome {
    /// Chunks successfully embedded at standard quality.
    pub stored: usize,
    /// Chunks that failed to embed (logged individually).
    pub skipped: usize,
    /// Chunks successfully embedded at high quality (0 when no HQ queue).
    pub hq_stored: usize,
}

/// Progress event emitted by [`EmbeddingPlan::execute`].
#[derive(Debug, Clone)]
pub enum EmbeddingProgress {
    /// Rechunking plan: node `done` of `total` nodes processed.
    Rechunking { done: usize, total: usize },
}

enum EmbeddingTask {
    Rechunk(Vec<crate::types::ObjectId>),
    EmbedAll,
}

/// Declarative description of an embedding job — either rechunking specific
/// nodes or sweeping all unembedded chunks.
///
/// Build with [`EmbeddingPlan::rechunk`] or [`EmbeddingPlan::embed_all`],
/// then call [`EmbeddingPlan::execute`].
pub struct EmbeddingPlan {
    task: EmbeddingTask,
}

impl EmbeddingPlan {
    /// Plan to rechunk and re-embed a specific set of nodes.
    pub fn rechunk(node_ids: Vec<crate::types::ObjectId>) -> Self {
        Self {
            task: EmbeddingTask::Rechunk(node_ids),
        }
    }

    /// Plan to embed all chunks that are not yet embedded (bulk sweep).
    pub fn embed_all() -> Self {
        Self {
            task: EmbeddingTask::EmbedAll,
        }
    }

    /// Short machine-readable kind string for tracing spans.
    pub fn kind(&self) -> &'static str {
        match &self.task {
            EmbeddingTask::Rechunk(_) => "rechunk",
            EmbeddingTask::EmbedAll => "embed_all",
        }
    }

    /// Human-readable initial label for the plan (for status bar display).
    pub fn label(&self) -> String {
        match &self.task {
            EmbeddingTask::Rechunk(ids) => format!("Re-embedding {} node(s)…", ids.len()),
            EmbeddingTask::EmbedAll => "Embedding…".to_string(),
        }
    }

    /// Return whether this plan has storage work to do before the UI starts it.
    ///
    /// The executor still performs its own checks because storage can change
    /// between scheduling and execution; this is only a cheap preflight to avoid
    /// flashing an embedding status when everything is already indexed.
    pub fn has_pending_work(
        &self,
        graph: &KnowledgeGraph,
        standard_enabled: bool,
        hq_enabled: bool,
    ) -> Result<bool> {
        match &self.task {
            EmbeddingTask::Rechunk(ids) => Ok(!ids.is_empty()),
            EmbeddingTask::EmbedAll => {
                let stats = graph.get_stats()?;
                let standard_complete = stats.embedded_count == stats.chunk_count;
                Ok(
                    (standard_enabled && stats.chunk_count > stats.embedded_count)
                        || (hq_enabled
                            && standard_complete
                            && stats.chunk_count > stats.embedded_hq_count),
                )
            }
        }
    }

    /// Execute the plan, emitting [`EmbeddingProgress`] events via `on_progress`
    /// as work proceeds.  Returns an [`EmbeddingOutcome`] when complete.
    ///
    /// `on_progress` is called synchronously from within the async task — keep it
    /// cheap (e.g. write to an `Arc<Mutex<_>>`).
    pub async fn execute(
        self,
        graph: &KnowledgeGraph,
        queue: &InferenceQueue,
        hq_queue: Option<&InferenceQueue>,
        on_progress: impl Fn(EmbeddingProgress) + Send,
    ) -> EmbeddingOutcome {
        self.execute_with_cancellation(
            graph,
            queue,
            hq_queue,
            CancellationToken::new(),
            on_progress,
        )
        .await
    }

    /// Execute every child embedding job under one parent token.
    pub async fn execute_with_cancellation(
        self,
        graph: &KnowledgeGraph,
        queue: &InferenceQueue,
        hq_queue: Option<&InferenceQueue>,
        cancellation: CancellationToken,
        on_progress: impl Fn(EmbeddingProgress) + Send,
    ) -> EmbeddingOutcome {
        let t0 = std::time::Instant::now();
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_inflight = Arc::new(AtomicUsize::new(0));

        match self.task {
            EmbeddingTask::Rechunk(node_ids) => {
                let total = node_ids.len();
                let mut stored = 0usize;
                let mut skipped = 0usize;
                for (done, oid) in node_ids.iter().enumerate() {
                    if cancellation.is_cancelled() {
                        skipped += total.saturating_sub(done);
                        break;
                    }
                    let cur = inflight.fetch_add(1, Ordering::Relaxed) + 1;
                    max_inflight.fetch_max(cur, Ordering::Relaxed);
                    match rechunk_and_embed_with_cancellation(
                        graph,
                        queue,
                        hq_queue,
                        *oid,
                        cancellation.clone(),
                    )
                    .await
                    {
                        Ok(n) => stored += n,
                        Err(_) if cancellation.is_cancelled() => {
                            skipped += total.saturating_sub(done);
                            inflight.fetch_sub(1, Ordering::Relaxed);
                            break;
                        }
                        Err(e) => {
                            warn!(object_id = %oid, %e, "rechunk_and_embed failed");
                            skipped += 1;
                        }
                    }
                    inflight.fetch_sub(1, Ordering::Relaxed);
                    on_progress(EmbeddingProgress::Rechunking {
                        done: done + 1,
                        total,
                    });
                }
                let peak = max_inflight.load(Ordering::Relaxed);
                let duration_ms = t0.elapsed().as_millis() as u64;
                info!(
                    target: "u_forge::ingest",
                    max_inflight = peak,
                    total_jobs = total,
                    duration_ms,
                    "EmbeddingPlan::Rechunk complete"
                );
                EmbeddingOutcome {
                    stored,
                    skipped,
                    hq_stored: 0,
                }
            }
            EmbeddingTask::EmbedAll => {
                let std_result = embed_all_chunks_with_cancellation(
                    graph,
                    queue,
                    EmbeddingTarget::Standard,
                    cancellation.clone(),
                )
                .await;
                let standard_complete = graph
                    .get_stats()
                    .is_ok_and(|stats| stats.embedded_count == stats.chunk_count);
                let hq_result = if let Some(hq) =
                    hq_queue.filter(|_| !cancellation.is_cancelled() && standard_complete)
                {
                    Some(
                        embed_all_chunks_with_cancellation(
                            graph,
                            hq,
                            EmbeddingTarget::HighQuality,
                            cancellation.clone(),
                        )
                        .await,
                    )
                } else {
                    if hq_queue.is_some() && !standard_complete && !cancellation.is_cancelled() {
                        warn!(
                            "HQ embedding skipped because the standard embedding lane is incomplete"
                        );
                    }
                    None
                };

                let (stored, skipped, total_jobs) = match std_result {
                    Ok(r) => (r.stored, r.skipped, r.total),
                    Err(_) if cancellation.is_cancelled() => (0, 0, 0),
                    Err(e) => {
                        warn!(%e, "embed_all_chunks (standard) failed");
                        (0, 0, 0)
                    }
                };
                let hq_stored = match hq_result {
                    Some(Ok(result)) => result.stored,
                    Some(Err(error)) => {
                        warn!(%error, "embed_all_chunks (HQ) failed");
                        0
                    }
                    None => 0,
                };

                // embed_many uses (workers * 2).max(4) as its concurrency cap.
                let concurrency_cap = (queue.embedding_worker_count() * 2).max(4);
                let peak = concurrency_cap.min(total_jobs);
                let duration_ms = t0.elapsed().as_millis() as u64;
                info!(
                    target: "u_forge::ingest",
                    max_inflight = peak,
                    total_jobs,
                    duration_ms,
                    "EmbeddingPlan::EmbedAll complete"
                );

                EmbeddingOutcome {
                    stored,
                    skipped,
                    hq_stored,
                }
            }
        }
    }
}

/// Re-chunk a single object and embed all its chunks, waiting until complete.
///
/// This is the per-node analogue of the bulk [`embed_all_chunks`] pipeline:
/// 1. Load the node's metadata and resolve edge display lines.
/// 2. Delete all existing chunks for the node (triggers clean up FTS5 + vector indexes).
/// 3. Flatten the node into embedding text via [`ObjectMetadata::flatten_for_embedding`].
/// 4. Create new chunk(s) via [`KnowledgeGraph::add_text_chunk`].
/// 5. Embed every chunk with `queue` (standard 768-dim).
/// 6. If `hq_queue` is provided, also embed every chunk at high quality (4096-dim).
///
/// Returns the number of chunks created (and embedded).
///
/// # Errors
/// - Node not found.
/// - Neither the standard nor high-quality queue has an embedding worker.
/// - An active embedding lane has no stable model fingerprint.
/// - Any individual embed or upsert call fails.
pub async fn rechunk_and_embed(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    object_id: crate::types::ObjectId,
) -> Result<usize> {
    rechunk_and_embed_with_cancellation(graph, queue, hq_queue, object_id, CancellationToken::new())
        .await
}

pub async fn rechunk_and_embed_with_cancellation(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    object_id: crate::types::ObjectId,
    cancellation: CancellationToken,
) -> Result<usize> {
    use crate::types::ChunkType;

    if cancellation.is_cancelled() {
        return Err(cancellation.error().into());
    }

    let has_standard = queue.has_embedding();
    let hq_queue = hq_queue.filter(|queue| queue.has_embedding());
    if !has_standard {
        anyhow::bail!(
            "A standard embedding provider is required before high-quality embeddings can be added"
        );
    }

    if has_standard {
        let standard_fingerprint = queue
            .embedding_space_fingerprint()
            .ok_or_else(|| anyhow::anyhow!("Embedding queue has no model fingerprint"))?;
        graph.ensure_embedding_space(EmbeddingTarget::Standard, standard_fingerprint)?;
    }
    if let Some(hq) = hq_queue {
        let hq_fingerprint = hq
            .embedding_space_fingerprint()
            .ok_or_else(|| anyhow::anyhow!("HQ embedding queue has no model fingerprint"))?;
        graph.ensure_embedding_space(EmbeddingTarget::HighQuality, hq_fingerprint)?;
    }

    let meta = graph
        .get_object(object_id)?
        .ok_or_else(|| anyhow::anyhow!("Node {object_id} not found"))?;

    let edge_lines = graph.edge_display_lines(&meta);
    let flat_text = meta.flatten_for_embedding(&edge_lines);

    // Remove stale chunks (triggers clean up FTS5 + vector tables).
    if cancellation.is_cancelled() {
        return Err(cancellation.error().into());
    }
    let deleted = graph.delete_chunks_for_node(object_id)?;
    if deleted > 0 {
        tracing::debug!(object_id = %object_id, deleted, "Deleted old chunks");
    }

    // Create fresh chunks from the flattened text.
    if cancellation.is_cancelled() {
        return Err(cancellation.error().into());
    }
    let chunk_ids = graph.add_text_chunk(object_id, flat_text, ChunkType::Description)?;
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    // Retrieve the newly created chunks so we have their content for embedding.
    let chunks = graph.get_text_chunks(object_id)?;

    if has_standard {
        let mut embeddings = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            embeddings.push((
                chunk.id,
                queue
                    .submit_embed_with_cancellation(&chunk.content, cancellation.clone())
                    .await?,
            ));
        }
        if cancellation.is_cancelled() {
            return Err(cancellation.error().into());
        }
        graph.upsert_chunk_embeddings(embeddings)?;
    }

    // Embed with the HQ queue if available.
    if let Some(hq) = hq_queue {
        let mut hq_embeddings = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            hq_embeddings.push((
                chunk.id,
                hq.submit_embed_with_cancellation(&chunk.content, cancellation.clone())
                    .await?,
            ));
        }
        if cancellation.is_cancelled() {
            return Err(cancellation.error().into());
        }
        graph.upsert_chunk_embeddings_hq(hq_embeddings)?;
    }

    tracing::info!(
        object_id = %object_id,
        name = %meta.name,
        chunks = chunks.len(),
        standard = has_standard,
        hq = hq_queue.is_some(),
        "Rechunked and embedded node"
    );

    Ok(chunks.len())
}

/// Embed all un-embedded chunks in `graph` using `queue`.
///
/// Returns `Ok(EmbeddingResult)` with `total == 0` when:
/// - the queue has no embedding worker, or
/// - all chunks are already embedded for the given `target`.
///
/// Individual upsert failures are counted in `skipped` and logged as
/// warnings rather than aborting the batch.
pub async fn embed_all_chunks(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    target: EmbeddingTarget,
) -> Result<EmbeddingResult> {
    embed_all_chunks_with_cancellation(graph, queue, target, CancellationToken::new()).await
}

pub async fn embed_all_chunks_with_cancellation(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    target: EmbeddingTarget,
    cancellation: CancellationToken,
) -> Result<EmbeddingResult> {
    if cancellation.is_cancelled() {
        return Err(cancellation.error().into());
    }
    if !queue.has_embedding() {
        return Ok(EmbeddingResult {
            stored: 0,
            skipped: 0,
            total: 0,
        });
    }
    let stats = graph.get_stats()?;
    if target == EmbeddingTarget::HighQuality && stats.embedded_count < stats.chunk_count {
        anyhow::bail!(
            "High-quality embeddings require a complete standard embedding lane ({} of {} chunks are standard-embedded)",
            stats.embedded_count,
            stats.chunk_count
        );
    }
    let fingerprint = queue
        .embedding_space_fingerprint()
        .ok_or_else(|| anyhow::anyhow!("Embedding queue has no model fingerprint"))?;
    graph.ensure_embedding_space(target, fingerprint)?;

    let needs_embedding = match target {
        EmbeddingTarget::Standard => stats.chunk_count > stats.embedded_count,
        EmbeddingTarget::HighQuality => stats.chunk_count > stats.embedded_hq_count,
    };

    if !needs_embedding {
        info!(
            target = ?target,
            chunks = stats.chunk_count,
            "All chunks already embedded — skipping"
        );
        return Ok(EmbeddingResult {
            stored: 0,
            skipped: 0,
            total: 0,
        });
    }

    info!(target = ?target, "Embedding chunks");

    let chunks_to_embed = match target {
        EmbeddingTarget::Standard => graph.get_unembedded_chunks()?,
        EmbeddingTarget::HighQuality => graph.get_unembedded_chunks_hq()?,
    };

    let total = chunks_to_embed.len();
    let texts: Vec<String> = chunks_to_embed.iter().map(|c| c.content.clone()).collect();

    match queue
        .embed_many_with_cancellation(texts, cancellation.clone())
        .await
    {
        Err(_) if cancellation.is_cancelled() => Err(cancellation.error().into()),
        Err(e) => {
            warn!(%e, target = ?target, "Embedding failed");
            Ok(EmbeddingResult {
                stored: 0,
                skipped: total,
                total,
            })
        }
        Ok(vecs) => {
            if cancellation.is_cancelled() {
                return Err(cancellation.error().into());
            }
            let embeddings = chunks_to_embed
                .iter()
                .zip(vecs)
                .map(|(chunk, vec)| (chunk.id, vec))
                .collect::<Vec<_>>();
            let result = match target {
                EmbeddingTarget::Standard => graph.upsert_chunk_embeddings(embeddings),
                EmbeddingTarget::HighQuality => graph.upsert_chunk_embeddings_hq(embeddings),
            };
            let (stored, skipped) = match result {
                Ok(()) => (total, 0),
                Err(e) => {
                    warn!(%e, target = ?target, "Could not store embedding batch");
                    (0, total)
                }
            };
            info!(stored, skipped, total, target = ?target, "Embedding complete");
            Ok(EmbeddingResult {
                stored,
                skipped,
                total,
            })
        }
    }
}

/// Build a single-worker [`InferenceQueue`] for the high-quality (4096-dim)
/// embedding model, if the catalog advertises one and HQ embedding is
/// enabled in `app_cfg`.
///
/// Returns `None` when:
/// - HQ embedding is disabled in config
/// - No suitable HQ embedding model is downloaded
/// - The model fails to load
/// - The model's dimensions don't match [`HIGH_QUALITY_EMBEDDING_DIMENSIONS`]
pub async fn build_hq_embed_queue(
    catalog: &LemonadeServerCatalog,
    app_cfg: &AppConfig,
) -> Option<InferenceQueue> {
    let connection =
        Arc::new(crate::lemonade::LemonadeConnection::external(&catalog.base_url).ok()?);
    build_hq_embed_queue_with_connection(catalog, app_cfg, connection).await
}

pub async fn build_hq_embed_queue_with_connection(
    catalog: &LemonadeServerCatalog,
    app_cfg: &AppConfig,
    connection: Arc<crate::lemonade::LemonadeConnection>,
) -> Option<InferenceQueue> {
    if !app_cfg.embedding.high_quality_embedding {
        return None;
    }

    let selector = ModelSelector::new(catalog, &app_cfg.models, &app_cfg.embedding);
    let hq_model = selector
        .select_embedding_models()
        .into_iter()
        .find(|s| s.quality_tier == QualityTier::High)?;

    let hq_model_id = hq_model.model_id.clone();
    info!(model = %hq_model_id, "Loading HQ embedding model");

    let already_loaded: Vec<String> = catalog
        .loaded
        .iter()
        .map(|m| m.model_name.clone())
        .collect();

    let built: BuiltProvider = match ProviderFactory::build_with_connection(
        &hq_model,
        Capability::Embedding,
        connection,
        app_cfg.embedding.gpu_weight,
        None,
        &already_loaded,
    )
    .await
    {
        Err(e) => {
            warn!(%e, model = %hq_model_id, "HQ embedding model load failed");
            return None;
        }
        Ok(p) => p,
    };

    // Verify dimensions before registering.
    if let crate::lemonade::provider_factory::ProviderSlot::Embedding(ref provider) = built.provider
    {
        let dims = provider.dimensions().unwrap_or(0);
        if dims != HIGH_QUALITY_EMBEDDING_DIMENSIONS {
            warn!(
                actual = dims,
                expected = HIGH_QUALITY_EMBEDDING_DIMENSIONS,
                model = %hq_model_id,
                "HQ model dimension mismatch — skipped"
            );
            return None;
        }
        info!(model = %hq_model_id, dims, "HQ embedding model ready");
    }

    Some(
        InferenceQueueBuilder::new()
            .with_config(app_cfg.clone())
            .with_provider(built)
            .build(),
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use tempfile::TempDir;
    use tokio::sync::Semaphore;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::lemonade::{BuiltProvider, Capability, ProviderSlot};
    use crate::queue::InferenceQueueBuilder;
    use crate::types::ChunkType;
    use crate::{KnowledgeGraph, ObjectBuilder};

    use super::*;

    // ── Mock embedding provider ───────────────────────────────────────────────

    struct MockEmbeddingProvider {
        dimensions: usize,
    }

    struct BlockingEmbeddingProvider {
        calls: Arc<AtomicUsize>,
        started: Arc<Semaphore>,
    }

    #[async_trait]
    impl EmbeddingProvider for BlockingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.add_permits(1);
            std::future::pending().await
        }

        async fn embed_batch(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            unreachable!()
        }

        fn dimensions(&self) -> anyhow::Result<usize> {
            Ok(crate::EMBEDDING_DIMENSIONS)
        }

        fn max_tokens(&self) -> anyhow::Result<usize> {
            Ok(512)
        }

        fn provider_type(&self) -> EmbeddingProviderType {
            EmbeddingProviderType::Lemonade
        }

        fn model_info(&self) -> Option<EmbeddingModelInfo> {
            None
        }
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let seed = text.len() as f32 + text.chars().next().unwrap_or('a') as u32 as f32;
            Ok((0..self.dimensions)
                .map(|i| ((seed + i as f32) % 1000.0) / 1000.0)
                .collect())
        }

        async fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::new();
            for t in &texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }

        fn dimensions(&self) -> anyhow::Result<usize> {
            Ok(self.dimensions)
        }
        fn max_tokens(&self) -> anyhow::Result<usize> {
            Ok(512)
        }
        fn provider_type(&self) -> EmbeddingProviderType {
            EmbeddingProviderType::Lemonade
        }
        fn model_info(&self) -> Option<EmbeddingModelInfo> {
            None
        }
    }

    fn make_embed_queue_with_dimensions(dimensions: usize) -> crate::queue::InferenceQueue {
        let built = BuiltProvider {
            name: "mock-embed".to_string(),
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(Arc::new(MockEmbeddingProvider { dimensions })),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_embed_queue() -> crate::queue::InferenceQueue {
        make_embed_queue_with_dimensions(crate::EMBEDDING_DIMENSIONS)
    }

    fn make_graph() -> (KnowledgeGraph, TempDir) {
        let tmp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(tmp.path()).unwrap();
        (graph, tmp)
    }

    /// Verify that `embed_all_chunks` is incremental: after an initial full
    /// embedding pass, only newly added chunks are embedded on the next call.
    #[tokio::test]
    async fn test_embed_all_chunks_is_incremental() {
        let (graph, _tmp) = make_graph();
        let queue = make_embed_queue();

        // Add 10 objects, each with one text chunk.
        for i in 0..10 {
            let oid = ObjectBuilder::character(format!("Character {i}"))
                .add_to_graph(&graph)
                .unwrap();
            graph
                .add_text_chunk(
                    oid,
                    format!("Description for character number {i}."),
                    ChunkType::Description,
                )
                .unwrap();
        }

        let stats = graph.get_stats().unwrap();
        assert_eq!(
            stats.chunk_count, 10,
            "Expected 10 chunks after initial inserts"
        );
        assert_eq!(stats.embedded_count, 0, "No chunks embedded yet");

        // First pass: embed all 10.
        let result = embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard)
            .await
            .unwrap();

        assert_eq!(result.total, 10);
        assert_eq!(result.stored, 10);
        assert_eq!(result.skipped, 0);

        let stats = graph.get_stats().unwrap();
        assert_eq!(
            stats.embedded_count, 10,
            "All 10 chunks should now be embedded"
        );

        // Add 2 more objects with chunks.
        for i in 10..12 {
            let oid = ObjectBuilder::character(format!("Character {i}"))
                .add_to_graph(&graph)
                .unwrap();
            graph
                .add_text_chunk(
                    oid,
                    format!("Description for character number {i}."),
                    ChunkType::Description,
                )
                .unwrap();
        }

        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.chunk_count, 12);
        assert_eq!(
            stats.embedded_count, 10,
            "The 2 new chunks should be unembedded"
        );

        // Second pass: only the 2 new chunks should be processed.
        let result = embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard)
            .await
            .unwrap();

        assert_eq!(
            result.total, 2,
            "Only 2 unembedded chunks should be processed"
        );
        assert_eq!(result.stored, 2);
        assert_eq!(result.skipped, 0);

        let stats = graph.get_stats().unwrap();
        assert_eq!(
            stats.embedded_count, 12,
            "All 12 chunks should now be embedded"
        );
    }

    #[tokio::test]
    async fn cancelled_embedding_batch_performs_no_vector_writes() {
        let (graph, _tmp) = make_graph();
        let graph = Arc::new(graph);
        let object_id = ObjectBuilder::character("Cancelled character".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .add_text_chunk(
                object_id,
                "This embedding must never be stored.".to_string(),
                ChunkType::Description,
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Semaphore::new(0));
        let queue = InferenceQueueBuilder::new()
            .with_provider(BuiltProvider {
                name: "blocking-embed".to_string(),
                capability: Capability::Embedding,
                provider: ProviderSlot::Embedding(Arc::new(BlockingEmbeddingProvider {
                    calls: calls.clone(),
                    started: started.clone(),
                })),
                weight: 100,
            })
            .build();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn({
            let graph = graph.clone();
            let queue = queue.clone();
            let cancellation = cancellation.clone();
            async move {
                embed_all_chunks_with_cancellation(
                    &graph,
                    &queue,
                    EmbeddingTarget::Standard,
                    cancellation,
                )
                .await
            }
        });
        started.acquire().await.unwrap().forget();
        cancellation.cancel();
        assert!(task.await.unwrap().is_err());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(graph.get_stats().unwrap().embedded_count, 0);
    }

    #[tokio::test]
    async fn rechunk_rejects_an_hq_only_embedding_lane_without_deleting_source_chunks() {
        let (graph, _tmp) = make_graph();
        let standard_queue = InferenceQueueBuilder::new().build();
        let hq_queue = make_embed_queue_with_dimensions(HIGH_QUALITY_EMBEDDING_DIMENSIONS);
        let object_id = ObjectBuilder::character("HQ-only character".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .add_text_chunk(
                object_id,
                "Existing searchable text".to_string(),
                ChunkType::Description,
            )
            .unwrap();

        let error = rechunk_and_embed(&graph, &standard_queue, Some(&hq_queue), object_id)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("standard embedding provider"));
        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.chunk_count, 1);
        assert_eq!(stats.embedded_count, 0);
        assert_eq!(stats.embedded_hq_count, 0);
        assert!(
            !EmbeddingPlan::embed_all()
                .has_pending_work(&graph, false, true)
                .unwrap(),
            "HQ work must wait until the standard lane can be populated"
        );
        assert!(
            EmbeddingPlan::embed_all()
                .has_pending_work(&graph, true, true)
                .unwrap(),
            "the same graph still needs work when a standard lane is active"
        );
    }

    #[tokio::test]
    async fn direct_hq_sweep_requires_standard_embeddings_for_every_chunk() {
        let (graph, _tmp) = make_graph();
        let hq_queue = make_embed_queue_with_dimensions(HIGH_QUALITY_EMBEDDING_DIMENSIONS);
        let object_id = ObjectBuilder::character("Unembedded character".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .add_text_chunk(
                object_id,
                "Standard must be written first".to_string(),
                ChunkType::Description,
            )
            .unwrap();

        let error = embed_all_chunks(&graph, &hq_queue, EmbeddingTarget::HighQuality)
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("complete standard embedding lane")
        );
        let stats = graph.get_stats().unwrap();
        assert_eq!(stats.embedded_count, 0);
        assert_eq!(stats.embedded_hq_count, 0);
    }
}
