//! Batch embedding helpers.
//!
//! [`embed_all_chunks`] embeds every un-embedded text chunk in a
//! [`KnowledgeGraph`] using an [`InferenceQueue`], for both standard
//! (768-dim) and high-quality (4096-dim) targets.
//!
//! [`build_hq_embed_queue`] is a convenience constructor that builds a
//! single-worker [`InferenceQueue`] for the first high-quality embedding model
//! selected by [`ModelSelector`] from a live [`LemonadeServerCatalog`].

use anyhow::Result;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::lemonade::catalog::LemonadeServerCatalog;
use crate::lemonade::provider_factory::{BuiltProvider, Capability, ProviderFactory};
use crate::lemonade::selector::{ModelSelector, QualityTier};
use crate::queue::{InferenceQueue, InferenceQueueBuilder};
use crate::KnowledgeGraph;
use crate::HIGH_QUALITY_EMBEDDING_DIMENSIONS;

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
/// - Embedding queue has no workers.
/// - Any individual embed or upsert call fails.
pub async fn rechunk_and_embed(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    object_id: crate::types::ObjectId,
) -> Result<usize> {
    use crate::types::ChunkType;

    let meta = graph
        .get_object(object_id)?
        .ok_or_else(|| anyhow::anyhow!("Node {object_id} not found"))?;

    let edge_lines = graph.edge_display_lines(&meta);
    let flat_text = meta.flatten_for_embedding(&edge_lines);

    // Remove stale chunks (triggers clean up FTS5 + vector tables).
    let deleted = graph.delete_chunks_for_node(object_id)?;
    if deleted > 0 {
        tracing::debug!(object_id = %object_id, deleted, "Deleted old chunks");
    }

    // Create fresh chunks from the flattened text.
    let chunk_ids = graph.add_text_chunk(object_id, flat_text, ChunkType::Description)?;
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    // Retrieve the newly created chunks so we have their content for embedding.
    let chunks = graph.get_text_chunks(object_id)?;

    // Embed every chunk with the standard queue.
    for chunk in &chunks {
        let vec = queue.embed(&chunk.content).await?;
        graph.upsert_chunk_embedding(chunk.id, &vec)?;
    }

    // Embed with the HQ queue if available.
    if let Some(hq) = hq_queue {
        if hq.has_embedding() {
            for chunk in &chunks {
                let hq_vec = hq.embed(&chunk.content).await?;
                graph.upsert_chunk_embedding_hq(chunk.id, &hq_vec)?;
            }
        }
    }

    tracing::info!(
        object_id = %object_id,
        name = %meta.name,
        chunks = chunks.len(),
        hq = hq_queue.map_or(false, |q| q.has_embedding()),
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
    let stats = graph.get_stats()?;

    let needs_embedding = match target {
        EmbeddingTarget::Standard => stats.chunk_count > stats.embedded_count,
        EmbeddingTarget::HighQuality => stats.chunk_count > stats.embedded_hq_count,
    };

    if !queue.has_embedding() || !needs_embedding {
        if queue.has_embedding() && !needs_embedding {
            info!(
                target = ?target,
                chunks = stats.chunk_count,
                "All chunks already embedded — skipping"
            );
        }
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

    match queue.embed_many(texts).await {
        Err(e) => {
            warn!(%e, target = ?target, "Embedding failed");
            Ok(EmbeddingResult {
                stored: 0,
                skipped: total,
                total,
            })
        }
        Ok(vecs) => {
            let mut stored = 0usize;
            let mut skipped = 0usize;
            for (chunk, vec) in chunks_to_embed.iter().zip(vecs.iter()) {
                let result = match target {
                    EmbeddingTarget::Standard => graph.upsert_chunk_embedding(chunk.id, vec),
                    EmbeddingTarget::HighQuality => graph.upsert_chunk_embedding_hq(chunk.id, vec),
                };
                match result {
                    Ok(()) => stored += 1,
                    Err(e) => {
                        warn!(chunk_id = %chunk.id, %e, "Could not store embedding");
                        skipped += 1;
                    }
                }
            }
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

    let built: BuiltProvider = match ProviderFactory::build(
        &hq_model,
        Capability::Embedding,
        &catalog.base_url,
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
    use std::sync::Arc;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::lemonade::{BuiltProvider, Capability, ProviderSlot};
    use crate::queue::InferenceQueueBuilder;
    use crate::types::ChunkType;
    use crate::{KnowledgeGraph, ObjectBuilder};

    use super::*;

    // ── Mock embedding provider ───────────────────────────────────────────────

    struct MockEmbeddingProvider;

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let seed = text.len() as f32 + text.chars().next().unwrap_or('a') as u32 as f32;
            Ok((0..768)
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
            Ok(768)
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

    fn make_embed_queue() -> crate::queue::InferenceQueue {
        let built = BuiltProvider {
            name: "mock-embed".to_string(),
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(Arc::new(MockEmbeddingProvider)),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
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
}
