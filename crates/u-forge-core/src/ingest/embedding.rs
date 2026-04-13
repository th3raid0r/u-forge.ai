//! Batch embedding helpers.
//!
//! [`embed_all_chunks`] embeds every un-embedded text chunk in a
//! [`KnowledgeGraph`] using an [`InferenceQueue`], for both standard
//! (768-dim) and high-quality (4096-dim) targets.
//!
//! [`build_hq_embed_queue`] is a convenience constructor that builds a
//! single-worker [`InferenceQueue`] for the high-quality embedding model
//! advertised by the [`LemonadeModelRegistry`].

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::ai::embeddings::LemonadeProvider;
use crate::config::AppConfig;
use crate::lemonade::LemonadeModelRegistry;
use crate::queue::{InferenceQueue, InferenceQueueBuilder};
use crate::EmbeddingProvider;
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

    let chunks_to_embed = graph.get_all_objects().and_then(|objs| {
        let mut all = Vec::new();
        for obj in &objs {
            for chunk in graph.get_text_chunks(obj.id)? {
                all.push(chunk);
            }
        }
        Ok(all)
    })?;

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
/// embedding model, if the registry advertises one and HQ embedding is
/// enabled in `app_cfg`.
///
/// Returns `None` when:
/// - HQ embedding is disabled in config
/// - No suitable HQ embedding model is registered
/// - The model fails to load
/// - The model's dimensions don't match [`HIGH_QUALITY_EMBEDDING_DIMENSIONS`]
pub async fn build_hq_embed_queue(
    registry: &LemonadeModelRegistry,
    app_cfg: &AppConfig,
) -> Option<InferenceQueue> {
    if !app_cfg.embedding.high_quality_embedding {
        return None;
    }
    let hq_model = registry.hq_embedding_model(true)?;
    let hq_model_id = hq_model.id.clone();
    let hq_load_opts = app_cfg.models.load_options_for(&hq_model_id);

    info!(model = %hq_model_id, "Loading HQ embedding model");

    let provider = match LemonadeProvider::new_with_load(
        &registry.base_url,
        &hq_model_id,
        &hq_load_opts,
    )
    .await
    {
        Err(e) => {
            warn!(%e, model = %hq_model_id, "HQ embedding model load failed");
            return None;
        }
        Ok(p) => p,
    };

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
    Some(
        InferenceQueueBuilder::new()
            .with_config(app_cfg.clone())
            .with_embedding_provider_weighted(
                Arc::new(provider),
                format!("hq({hq_model_id})"),
                app_cfg.embedding.gpu_weight,
            )
            .build(),
    )
}
