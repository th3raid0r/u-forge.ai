//! Node-centric hybrid search pipeline combining FTS5 keyword search,
//! sqlite-vec ANN semantic search, and optional cross-encoder reranking.
//!
//! # Overview
//!
//! [`search_hybrid`] is the main entry point. It uses chunk-level search
//! signals to identify the most relevant knowledge graph **nodes**, then
//! returns each winning node with full UI context plus a compact set of matched
//! chunks for LLM retrieval context.
//!
//! This design provides complete node context for downstream consumers:
//!
//! - **LLM context assembly** — a querying LLM receives whole nodes rather
//!   than isolated snippets, enabling richer reasoning across all information
//!   stored about an entity.
//! - **UI search** — results map directly to knowledge graph nodes, supporting
//!   "find the node" workflows rather than "find the snippet" workflows.
//!
//! # Search Primitives
//!
//! Chunk-level candidate retrieval uses three primitives:
//!
//! - **FTS5** — SQLite full-text search (`search_chunks_fts`). Fast,
//!   keyword/phrase matching. Returns results ordered by implicit relevance rank.
//! - **Semantic ANN** — sqlite-vec cosine nearest-neighbour search
//!   (`search_chunks_semantic`). Finds conceptually similar chunks even when
//!   keywords don't overlap. Requires an embedding worker in the queue.
//! - **Reranking** — cross-encoder rescoring (`InferenceQueue::rerank`).
//!   Expensive but highly precise. Applied at the node level using
//!   concatenated chunk content.
//!
//! # Algorithm
//!
//! 1. Retrieve chunk-level candidates via FTS5 and/or semantic ANN.
//! 2. Score each chunk using Reciprocal Rank Fusion (RRF).
//! 3. Aggregate chunk scores per parent node — nodes with more matching
//!    chunks, or chunks found by both search paths, naturally rank higher.
//! 4. Select the top-N nodes (default 3).
//! 5. For each winning node, load metadata, all text chunks for UI display, all
//!    edges, connected node summaries, and the matched chunks that contributed
//!    retrieval evidence.
//! 6. Optionally rerank the winning nodes using metadata, edge summaries, and
//!    matched chunks rather than every chunk attached to the node.
//!
//! # Merge Strategy: Reciprocal Rank Fusion (RRF)
//!
//! RRF merges the two ranked chunk lists before node aggregation.
//! Unlike raw score normalisation, RRF works with rank positions only, which
//! matters here because FTS5 does not expose numeric relevance scores.
//!
//! ```text
//! chunk_score(doc) = (1 - alpha) / (k + fts_rank)     -- FTS5 contribution
//!                  +      alpha  / (k + semantic_rank)  -- semantic contribution
//! node_score(node) = SUM(chunk_score) for all chunks belonging to node
//! ```
//!
//! where `k = 60` is the standard RRF constant (Cormack & Clarke, SIGIR 2009)
//! and `alpha ∈ [0, 1]` controls the FTS / semantic balance.
//!
//! # Graceful Degradation
//!
//! - No embedding worker registered → FTS-only mode (alpha effectively `0.0`).
//! - Embedding fails at runtime → falls back to FTS-only with a warning.
//! - No reranking worker registered (or `config.rerank = false`) → return
//!   RRF-scored results directly.
//! - Reranker fails at runtime → falls back to RRF-scored results with a warning.
//! - Neither search path returns results → returns an empty `Vec` (not an error).

mod pipeline;
mod sanitize;

pub use sanitize::fts5_sanitize;

use std::collections::HashMap;

use anyhow::Result;
use tracing::instrument;

use crate::KnowledgeGraph;
use crate::queue::{CancellationToken, InferenceQueue};
use crate::types::{Edge, ObjectId, ObjectMetadata, TextChunk};

// ── Public configuration ──────────────────────────────────────────────────────

/// Configuration for [`search_hybrid`].
///
/// All fields have sensible defaults via [`HybridSearchConfig::default`].
#[derive(Debug, Clone)]
pub struct HybridSearchConfig {
    /// Weight between FTS5 and semantic search.
    ///
    /// - `0.0` → pure FTS (semantic stage skipped entirely)
    /// - `1.0` → pure semantic (FTS stage skipped entirely)
    /// - `0.5` → equal blend (recommended starting point)
    ///
    /// Values outside `[0.0, 1.0]` are clamped at call time.
    pub alpha: f32,

    /// Number of FTS5 chunk candidates to retrieve before merging.
    ///
    /// Larger values give wider coverage of the chunk pool at the cost of
    /// slightly more CPU in the merge phase.
    pub fts_limit: usize,

    /// Number of ANN semantic chunk candidates to retrieve before merging.
    pub semantic_limit: usize,

    /// Whether to apply cross-encoder reranking to the top nodes.
    ///
    /// When enabled, the concatenated chunk content of each winning node is
    /// scored against the query by the cross-encoder.  Silently ignored
    /// (treated as `false`) when the [`InferenceQueue`] has no
    /// reranking-capable worker registered.
    pub rerank: bool,

    /// Maximum number of **nodes** to return.
    ///
    /// Applied after node-level aggregation and again after reranking.
    /// Default is 3 — enough to provide rich context to an LLM without
    /// overwhelming the context window.
    pub limit: usize,

    /// Multiplier applied to HQ (4096-dim) semantic RRF contributions.
    ///
    /// The HQ embedding model captures substantially more semantic nuance
    /// than the standard 768-dim path.  Boosting its RRF weight prevents
    /// the lower-precision standard semantic results from diluting the
    /// high-quality signal.
    ///
    /// Default is `3.0` — HQ contributions count three times as much as
    /// standard semantic contributions at the same rank position.
    /// Set to `1.0` to treat both paths equally.
    pub hq_semantic_boost: f32,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: true,
            limit: 3,
            hq_semantic_boost: 3.0,
        }
    }
}

// ── Public result types ───────────────────────────────────────────────────────

/// A single node result from [`search_hybrid`].
///
/// Contains the context for one knowledge graph node. Full chunks remain
/// available for UI display, while [`Self::matched_chunks`] gives LLM callers a
/// smaller retrieval-focused context made only from chunks that matched FTS or
/// vector search.
#[derive(Debug, Clone)]
pub struct NodeSearchResult {
    /// Full metadata for the matched knowledge graph node.
    pub node: ObjectMetadata,

    /// All text chunks belonging to this node, in storage order.
    pub chunks: Vec<TextChunk>,

    /// Chunks that directly contributed retrieval evidence for this result,
    /// sorted by descending chunk-level RRF score.
    pub matched_chunks: Vec<MatchedChunk>,

    /// All edges incident on this node (both incoming and outgoing).
    pub edges: Vec<Edge>,

    /// Lightweight summaries of the nodes at the other end of each edge,
    /// keyed by their [`ObjectId`].  Allows callers to display edge endpoints
    /// (e.g. "mentors → Frodo [character]") without additional lookups.
    pub connected_node_names: HashMap<ObjectId, ConnectedNode>,

    /// Aggregated relevance score (higher = more relevant).
    ///
    /// This is the sum of RRF chunk scores for all chunks belonging to this
    /// node that appeared in the FTS5 and/or semantic search results.
    /// When reranking is applied, this is replaced by the cross-encoder score.
    pub score: f32,

    /// Provenance — which search paths contributed evidence for this node.
    pub sources: SearchSources,
}

impl NodeSearchResult {
    /// Total token count across all chunks belonging to this node.
    pub fn total_tokens(&self) -> usize {
        self.chunks.iter().map(|c| c.token_count).sum()
    }

    /// Total token count across chunks that actually matched the query.
    pub fn matched_tokens(&self) -> usize {
        self.matched_chunks.iter().map(|c| c.token_count).sum()
    }
}

/// A chunk that contributed retrieval evidence for a [`NodeSearchResult`].
#[derive(Debug, Clone)]
pub struct MatchedChunk {
    /// Chunk identifier.
    pub id: crate::types::ChunkId,

    /// Chunk text returned by the search backend.
    pub content: String,

    /// Conservative token estimate for the matched chunk content.
    pub token_count: usize,

    /// Chunk-level RRF score after merging all contributing search paths.
    pub score: f32,

    /// FTS5 rank position if this chunk matched the keyword path.
    pub fts_rank: Option<usize>,

    /// Standard semantic distance if this chunk matched the semantic path.
    pub semantic_distance: Option<f32>,

    /// High-quality semantic distance if this chunk matched the HQ path.
    pub hq_semantic_distance: Option<f32>,
}

/// Lightweight summary of a node connected via an edge.
///
/// Used in [`NodeSearchResult::connected_node_names`] to provide enough
/// context for display without loading full node metadata for every neighbour.
#[derive(Debug, Clone)]
pub struct ConnectedNode {
    /// Display name of the connected node.
    pub name: String,

    /// Object type (e.g. "character", "location").
    pub object_type: String,
}

/// Tracks which search paths contributed evidence for a [`NodeSearchResult`].
///
/// At the node level, these represent the *best* (lowest rank / closest
/// distance) values observed across all chunks belonging to the node.
#[derive(Debug, Clone, Default)]
pub struct SearchSources {
    /// Best (lowest) 0-based FTS5 rank position among the node's chunks,
    /// if any chunk was found by the FTS path.
    pub fts_rank: Option<usize>,

    /// Best (lowest) cosine distance among the node's chunks, if any chunk
    /// was found by the 768-dim semantic ANN path (0.0 = identical, 2.0 = maximally
    /// dissimilar).
    pub semantic_distance: Option<f32>,

    /// Best (lowest) cosine distance among the node's chunks from the
    /// high-quality 4096-dim semantic ANN path, if available.
    pub hq_semantic_distance: Option<f32>,

    /// Cross-encoder relevance score assigned by the reranker, if reranking
    /// was applied (higher = more relevant).
    pub rerank_score: Option<f32>,
}

/// Result of attempting one search stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStageStatus {
    Applied,
    IntentionallySkipped,
    Unavailable,
    Failed,
}

/// Structured, safe-to-display outcome for one retrieval or ranking stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStageOutcome {
    pub status: SearchStageStatus,
    pub diagnostic: Option<String>,
}

impl SearchStageOutcome {
    fn applied() -> Self {
        Self {
            status: SearchStageStatus::Applied,
            diagnostic: None,
        }
    }

    fn skipped(diagnostic: impl Into<String>) -> Self {
        Self {
            status: SearchStageStatus::IntentionallySkipped,
            diagnostic: Some(diagnostic.into()),
        }
    }

    fn unavailable(diagnostic: impl Into<String>) -> Self {
        Self {
            status: SearchStageStatus::Unavailable,
            diagnostic: Some(diagnostic.into()),
        }
    }

    fn failed(diagnostic: impl Into<String>) -> Self {
        Self {
            status: SearchStageStatus::Failed,
            diagnostic: Some(diagnostic.into()),
        }
    }
}

/// Outcomes for every independently degradable hybrid-search stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStageOutcomes {
    pub fts: SearchStageOutcome,
    pub standard_semantic: SearchStageOutcome,
    pub high_quality_semantic: SearchStageOutcome,
    pub reranking: SearchStageOutcome,
}

/// Structured hybrid-search response for UI and API consumers.
#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<NodeSearchResult>,
    pub outcomes: SearchStageOutcomes,
}

impl SearchSources {
    /// Human-readable bracketed label indicating which paths contributed.
    ///
    /// Examples: `"[FTS]"`, `"[SEM]"`, `"[FTS+SEM+HQ]"`, `"[FTS+SEM+HQ+RR]"`.
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(4);
        if self.fts_rank.is_some() {
            parts.push("FTS");
        }
        if self.semantic_distance.is_some() {
            parts.push("SEM");
        }
        if self.hq_semantic_distance.is_some() {
            parts.push("HQ");
        }
        if self.rerank_score.is_some() {
            parts.push("RR");
        }
        if parts.is_empty() {
            "[?]".to_string()
        } else {
            format!("[{}]", parts.join("+"))
        }
    }
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Node-centric hybrid search combining FTS5 keyword search, semantic ANN
/// search, and optional cross-encoder reranking.
///
/// Uses chunk-level retrieval as signal to identify the most relevant
/// knowledge graph **nodes**, then returns each winning node with its full
/// content (metadata, all text chunks, all edges, connected node summaries).
///
/// # Arguments
///
/// * `graph`  — Knowledge graph to search.
/// * `queue`  — Inference queue providing `embed()` and (optionally) `rerank()`.
/// * `query`  — Natural-language or keyword query string.
/// * `config` — Search configuration.
///
/// # Algorithm
///
/// 1. **FTS5** — `graph.search_chunks_fts(query, config.fts_limit)`.
///    Skipped when `alpha == 1.0`.
/// 2. **Embed** — `queue.embed(query)` to obtain the query vector.
///    Skipped when `alpha == 0.0` or no embedding worker is registered.
/// 3. **Semantic ANN** — `graph.search_chunks_semantic(&vec, config.semantic_limit)`.
///    Skipped when step 2 was skipped or failed.
/// 4. **RRF merge** — deduplicate chunks by `chunk_id`, sum RRF scores from
///    both paths.
/// 5. **Node aggregation** — group chunk scores by parent `object_id`.
///    A node's score is the sum of its chunks' RRF scores.  Nodes with more
///    matching chunks or chunks found by both paths rank higher.  Take the
///    top `config.limit` nodes.
/// 6. **Node hydration** — for each winning node, load full metadata, all
///    text chunks, all edges, and connected node summaries from the graph.
/// 7. **Rerank** (optional) — `queue.rerank(query, docs, top_n)` where each
///    document is the concatenated chunk content of one node.  Reorders the
///    results by cross-encoder score.
///
/// # Returns
///
/// Up to `config.limit` node results ordered by descending relevance score.
/// Never returns an error due to a missing AI capability — always degrades
/// gracefully to the next available path.
#[instrument(
    skip(graph, queue, hq_queue, config),
    fields(query, alpha = config.alpha, limit = config.limit)
)]
pub async fn search_hybrid_response(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
) -> Result<SearchResponse> {
    search_hybrid_response_with_cancellation(
        graph,
        queue,
        hq_queue,
        query,
        config,
        CancellationToken::new(),
    )
    .await
}

/// Run the complete search pipeline under one parent cancellation token.
pub async fn search_hybrid_response_with_cancellation(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
    cancellation: CancellationToken,
) -> Result<SearchResponse> {
    tracing::Span::current().record("query", query);
    pipeline::execute(pipeline::SearchRequest::new(
        graph,
        queue,
        hq_queue,
        query,
        config,
        cancellation,
    ))
    .await
}

/// Compatibility entry point for callers that only need ranked nodes.
pub async fn search_hybrid(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
) -> Result<Vec<NodeSearchResult>> {
    Ok(
        search_hybrid_response(graph, queue, hq_queue, query, config)
            .await?
            .results,
    )
}

pub async fn search_hybrid_with_cancellation(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
    cancellation: CancellationToken,
) -> Result<Vec<NodeSearchResult>> {
    Ok(search_hybrid_response_with_cancellation(
        graph,
        queue,
        hq_queue,
        query,
        config,
        cancellation,
    )
    .await?
    .results)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::lemonade::{
        BuiltProvider, Capability, ProviderSlot, RerankDocument, RerankProvider,
    };
    use crate::queue::InferenceQueueBuilder;
    use crate::types::ChunkType;
    use crate::{KnowledgeGraph, ObjectBuilder};

    // ── Mock embedding provider ───────────────────────────────────────────────
    //
    // Produces a deterministic 768-dim vector that varies by text content.
    // No Lemonade Server required.

    struct MockEmbeddingProvider;

    struct MockHqEmbeddingProvider;

    struct RecordingEmbeddingProvider {
        queries: Arc<Mutex<Vec<String>>>,
    }

    fn mock_embedding(text: &str, dimensions: usize) -> Vec<f32> {
        let seed = text.len() as f32 + text.chars().next().unwrap_or('a') as u32 as f32;
        (0..dimensions)
            .map(|index| ((seed + index as f32) % 1000.0) / 1000.0)
            .collect()
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(mock_embedding(text, 768))
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

    #[async_trait]
    impl EmbeddingProvider for MockHqEmbeddingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(mock_embedding(text, 4096))
        }

        async fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| mock_embedding(text, 4096))
                .collect())
        }

        fn dimensions(&self) -> anyhow::Result<usize> {
            Ok(4096)
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
    impl EmbeddingProvider for RecordingEmbeddingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            self.queries.lock().unwrap().push(text.to_string());
            Ok(mock_embedding(text, 768))
        }

        async fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| mock_embedding(text, 768)).collect())
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

    struct KeywordRerankProvider {
        keyword: &'static str,
    }

    struct FailingEmbeddingProvider;

    struct FailingHqEmbeddingProvider;

    #[async_trait]
    impl EmbeddingProvider for FailingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("secret embedding backend detail")
        }

        async fn embed_batch(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("secret embedding backend detail")
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

    struct WrongDimensionEmbeddingProvider;

    struct CancellingEmbeddingProvider {
        cancellation: CancellationToken,
    }

    #[async_trait]
    impl EmbeddingProvider for CancellingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.cancellation.cancel();
            std::future::pending().await
        }

        async fn embed_batch(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("batch embedding is not used by this test provider")
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

    #[async_trait]
    impl EmbeddingProvider for FailingHqEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("secret HQ embedding backend detail")
        }

        async fn embed_batch(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("secret HQ embedding backend detail")
        }

        fn dimensions(&self) -> anyhow::Result<usize> {
            Ok(4096)
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
    impl EmbeddingProvider for WrongDimensionEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0])
        }

        async fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.into_iter().map(|_| vec![1.0]).collect())
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

    struct FailingRerankProvider;

    struct EmptyRerankProvider;

    struct CancellingRerankProvider {
        cancellation: CancellationToken,
    }

    struct RecordingRerankProvider {
        queries: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Copy, Debug)]
    enum MalformedRerankKind {
        DuplicateIndex,
        OutOfBoundsIndex,
        NonFiniteScore,
    }

    struct MalformedRerankProvider {
        kind: MalformedRerankKind,
    }

    #[async_trait]
    impl RerankProvider for FailingRerankProvider {
        async fn rerank(
            &self,
            _query: &str,
            _documents: Vec<String>,
            _top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            anyhow::bail!("secret reranker backend detail")
        }
    }

    #[async_trait]
    impl RerankProvider for EmptyRerankProvider {
        async fn rerank(
            &self,
            _query: &str,
            _documents: Vec<String>,
            _top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl RerankProvider for CancellingRerankProvider {
        async fn rerank(
            &self,
            _query: &str,
            _documents: Vec<String>,
            _top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            self.cancellation.cancel();
            std::future::pending().await
        }
    }

    #[async_trait]
    impl RerankProvider for RecordingRerankProvider {
        async fn rerank(
            &self,
            query: &str,
            documents: Vec<String>,
            _top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            self.queries.lock().unwrap().push(query.to_string());
            Ok(documents
                .into_iter()
                .enumerate()
                .map(|(index, document)| RerankDocument {
                    index,
                    score: 1.0 - index as f32 * 0.1,
                    document: Some(document),
                })
                .collect())
        }
    }

    #[async_trait]
    impl RerankProvider for MalformedRerankProvider {
        async fn rerank(
            &self,
            _query: &str,
            documents: Vec<String>,
            _top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            let len = documents.len();
            Ok(documents
                .into_iter()
                .enumerate()
                .map(|(position, document)| {
                    let index = match self.kind {
                        MalformedRerankKind::DuplicateIndex if position + 1 == len => 0,
                        MalformedRerankKind::OutOfBoundsIndex if position + 1 == len => len,
                        _ => position,
                    };
                    let score = match self.kind {
                        MalformedRerankKind::NonFiniteScore if position == 0 => f32::NAN,
                        _ => 1.0 - position as f32 * 0.1,
                    };
                    RerankDocument {
                        index,
                        score,
                        document: Some(document),
                    }
                })
                .collect())
        }
    }

    #[async_trait]
    impl RerankProvider for KeywordRerankProvider {
        async fn rerank(
            &self,
            _query: &str,
            documents: Vec<String>,
            top_n: Option<usize>,
        ) -> anyhow::Result<Vec<RerankDocument>> {
            let keyword = self.keyword.to_ascii_lowercase();
            let mut ranked: Vec<RerankDocument> = documents
                .into_iter()
                .enumerate()
                .map(|(index, document)| RerankDocument {
                    index,
                    score: if document.to_ascii_lowercase().contains(&keyword) {
                        1.0
                    } else {
                        0.1
                    },
                    document: Some(document),
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if let Some(limit) = top_n {
                ranked.truncate(limit);
            }
            Ok(ranked)
        }
    }

    // ── Test fixtures ─────────────────────────────────────────────────────────

    /// Build a graph pre-populated with a handful of objects, edges, chunks,
    /// and mock embeddings so every search path has something to find.
    fn make_graph_with_data() -> (KnowledgeGraph, TempDir) {
        let tmp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(tmp.path()).unwrap();

        let wizard_id = ObjectBuilder::character("Gandalf".to_string())
            .with_description(
                "A wizard of great power who guides the Fellowship of the Ring.".to_string(),
            )
            .add_to_graph(&graph)
            .unwrap();

        let hobbit_id = ObjectBuilder::character("Frodo".to_string())
            .with_description(
                "A brave hobbit tasked with destroying the One Ring in Mount Doom.".to_string(),
            )
            .add_to_graph(&graph)
            .unwrap();

        let shire_id = ObjectBuilder::location("The Shire".to_string())
            .with_description(
                "A peaceful rural homeland inhabited by hobbits in northwest Middle-earth."
                    .to_string(),
            )
            .add_to_graph(&graph)
            .unwrap();

        let city_id = ObjectBuilder::location("Minas Tirith".to_string())
            .with_description(
                "The great white tower city and capital of Gondor, seat of the Stewards."
                    .to_string(),
            )
            .add_to_graph(&graph)
            .unwrap();

        // Edges — give the graph some topology for edge / connected-node tests.
        graph
            .connect_objects_str(wizard_id, hobbit_id, "mentors")
            .unwrap();
        graph
            .connect_objects_str(hobbit_id, shire_id, "lives_in")
            .unwrap();
        graph
            .connect_objects_str(hobbit_id, city_id, "traveled_to")
            .unwrap();
        graph
            .connect_objects_str(city_id, shire_id, "trade_route")
            .unwrap();

        // Add explicit searchable chunks.
        graph
            .add_text_chunk(
                wizard_id,
                "Gandalf wielded the wizard staff with ancient arcane magic.".to_string(),
                ChunkType::Description,
            )
            .unwrap();
        graph
            .add_text_chunk(
                wizard_id,
                "Gandalf later studied old maps and weathered travel journals.".to_string(),
                ChunkType::UserNote,
            )
            .unwrap();
        graph
            .add_text_chunk(
                hobbit_id,
                "Frodo carried the One Ring on a perilous journey to Mount Doom.".to_string(),
                ChunkType::Description,
            )
            .unwrap();
        graph
            .add_text_chunk(
                shire_id,
                "The Shire is a tranquil hobbit homeland with rolling green hills.".to_string(),
                ChunkType::Description,
            )
            .unwrap();
        graph
            .add_text_chunk(
                city_id,
                "Minas Tirith stands as the last fortress of Gondor against darkness.".to_string(),
                ChunkType::Description,
            )
            .unwrap();

        // Populate the vec index with deterministic mock embeddings so that the
        // semantic ANN path has data to query against.
        graph
            .ensure_embedding_space(
                crate::ingest::EmbeddingTarget::Standard,
                "mock-embed@unknown",
            )
            .unwrap();
        for oid in [wizard_id, hobbit_id, shire_id, city_id] {
            for chunk in graph.get_text_chunks(oid).unwrap() {
                let embedding = mock_embedding(&chunk.content, 768);
                graph.upsert_chunk_embedding(chunk.id, &embedding).unwrap();
            }
        }

        (graph, tmp)
    }

    fn populate_hq_embeddings(graph: &KnowledgeGraph) {
        graph
            .ensure_embedding_space(
                crate::ingest::EmbeddingTarget::HighQuality,
                "mock-hq@unknown",
            )
            .unwrap();
        for object in graph.get_all_objects().unwrap() {
            for chunk in graph.get_text_chunks(object.id).unwrap() {
                let embedding = mock_embedding(&chunk.content, 4096);
                graph
                    .upsert_chunk_embedding_hq(chunk.id, &embedding)
                    .unwrap();
            }
        }
    }

    fn make_embed_queue() -> InferenceQueue {
        let built = BuiltProvider {
            name: "mock-embed".to_string(),
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(Arc::new(MockEmbeddingProvider)),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_queue_no_workers() -> InferenceQueue {
        InferenceQueueBuilder::new().build()
    }

    fn make_custom_embed_queue(provider: Arc<dyn EmbeddingProvider>) -> InferenceQueue {
        make_named_embed_queue("mock-embed", provider)
    }

    fn make_named_embed_queue(name: &str, provider: Arc<dyn EmbeddingProvider>) -> InferenceQueue {
        let built = BuiltProvider {
            // `mock-embed` keeps the fixture's persisted embedding-space
            // fingerprint compatible; other names exercise mismatch handling.
            name: name.to_string(),
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(provider),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_custom_rerank_queue(provider: Arc<dyn RerankProvider>) -> InferenceQueue {
        let built = BuiltProvider {
            name: "mock-rerank".to_string(),
            capability: Capability::Reranking,
            provider: ProviderSlot::Rerank(provider),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_failing_rerank_queue() -> InferenceQueue {
        make_custom_rerank_queue(Arc::new(FailingRerankProvider))
    }

    fn make_empty_rerank_queue() -> InferenceQueue {
        let built = BuiltProvider {
            name: "mock-rerank".to_string(),
            capability: Capability::Reranking,
            provider: ProviderSlot::Rerank(Arc::new(EmptyRerankProvider)),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_cancelling_rerank_queue(cancellation: CancellationToken) -> InferenceQueue {
        let built = BuiltProvider {
            name: "mock-rerank".to_string(),
            capability: Capability::Reranking,
            provider: ProviderSlot::Rerank(Arc::new(CancellingRerankProvider { cancellation })),
            weight: 100,
        };
        InferenceQueueBuilder::new().with_provider(built).build()
    }

    fn make_keyword_rerank_queue(keyword: &'static str) -> InferenceQueue {
        make_custom_rerank_queue(Arc::new(KeywordRerankProvider { keyword }))
    }

    fn make_recording_queue(
        embedding_queries: Arc<Mutex<Vec<String>>>,
        reranking_queries: Arc<Mutex<Vec<String>>>,
    ) -> InferenceQueue {
        InferenceQueueBuilder::new()
            .with_providers(vec![
                BuiltProvider {
                    name: "mock-embed".to_string(),
                    capability: Capability::Embedding,
                    provider: ProviderSlot::Embedding(Arc::new(RecordingEmbeddingProvider {
                        queries: embedding_queries,
                    })),
                    weight: 100,
                },
                BuiltProvider {
                    name: "mock-rerank".to_string(),
                    capability: Capability::Reranking,
                    provider: ProviderSlot::Rerank(Arc::new(RecordingRerankProvider {
                        queries: reranking_queries,
                    })),
                    weight: 100,
                },
            ])
            .build()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_hybrid_search_returns_results() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 3,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "wizard magic staff", &config)
            .await
            .unwrap();

        assert!(
            !results.is_empty(),
            "Expected at least one result for 'wizard magic staff'"
        );
        assert!(results.len() <= 3, "Result count should respect the limit");

        // Each result should have node metadata and chunks populated.
        for r in &results {
            assert!(!r.node.name.is_empty(), "Node name should not be empty");
            assert!(
                !r.chunks.is_empty(),
                "Node '{}' should have at least one chunk",
                r.node.name
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_returns_full_node_context() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "wizard magic", &config)
            .await
            .unwrap();

        // Find Gandalf in results (should match "wizard" and "magic").
        let gandalf = results.iter().find(|r| r.node.name == "Gandalf");
        assert!(gandalf.is_some(), "Expected Gandalf in search results");
        let gandalf = gandalf.unwrap();

        // Gandalf should have chunks (the description chunk we added).
        assert!(!gandalf.chunks.is_empty(), "Expected chunks for Gandalf");
        assert!(
            !gandalf.matched_chunks.is_empty(),
            "Expected matched chunks for Gandalf"
        );

        // Gandalf should have edges (mentors → Frodo).
        assert!(!gandalf.edges.is_empty(), "Expected edges for Gandalf");

        // Connected nodes should include Frodo.
        assert!(
            !gandalf.connected_node_names.is_empty(),
            "Expected connected node names for Gandalf"
        );
        let has_frodo = gandalf
            .connected_node_names
            .values()
            .any(|cn| cn.name == "Frodo");
        assert!(has_frodo, "Expected Frodo in Gandalf's connected nodes");
    }

    #[tokio::test]
    async fn test_hybrid_returns_edges_and_connected_names() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        // "hobbit ring journey" should strongly match Frodo, who has 3 edges.
        let results = search_hybrid(&graph, &queue, None, "hobbit ring journey", &config)
            .await
            .unwrap();

        let frodo = results.iter().find(|r| r.node.name == "Frodo");
        assert!(frodo.is_some(), "Expected Frodo in search results");
        let frodo = frodo.unwrap();

        // Frodo has edges: mentors (Gandalf), lives_in (Shire), traveled_to (Minas Tirith).
        assert!(
            frodo.edges.len() >= 3,
            "Expected at least 3 edges for Frodo, got {}",
            frodo.edges.len()
        );

        // Connected nodes should include Gandalf, The Shire, and Minas Tirith.
        let connected_names: Vec<&str> = frodo
            .connected_node_names
            .values()
            .map(|cn| cn.name.as_str())
            .collect();
        assert!(
            connected_names.contains(&"Gandalf"),
            "Expected Gandalf in Frodo's connected nodes, got: {connected_names:?}"
        );
        assert!(
            connected_names.contains(&"The Shire"),
            "Expected The Shire in Frodo's connected nodes, got: {connected_names:?}"
        );
        assert!(
            connected_names.contains(&"Minas Tirith"),
            "Expected Minas Tirith in Frodo's connected nodes, got: {connected_names:?}"
        );
    }

    #[tokio::test]
    async fn test_hybrid_fts_only_mode() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            alpha: 0.0, // pure FTS
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "hobbit", &config)
            .await
            .unwrap();

        // Every result must come from FTS (no semantic_distance populated).
        for r in &results {
            assert!(
                r.sources.fts_rank.is_some(),
                "Expected fts_rank on all results in FTS-only mode"
            );
            assert!(
                r.sources.semantic_distance.is_none(),
                "Unexpected semantic_distance in FTS-only mode"
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_semantic_only_mode() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            alpha: 1.0, // pure semantic
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(
            &graph,
            &queue,
            None,
            "hobbit homeland hills peaceful",
            &config,
        )
        .await
        .unwrap();

        // Every result must come from semantic search (no fts_rank populated).
        for r in &results {
            assert!(
                r.sources.semantic_distance.is_some(),
                "Expected semantic_distance on all results in semantic-only mode"
            );
            assert!(
                r.sources.fts_rank.is_none(),
                "Unexpected fts_rank in semantic-only mode"
            );
        }
    }

    #[tokio::test]
    async fn dual_semantic_lanes_preserve_independent_evidence() {
        let (graph, _tmp) = make_graph_with_data();
        populate_hq_embeddings(&graph);
        let standard_queue = make_embed_queue();
        let hq_queue = make_named_embed_queue("mock-hq", Arc::new(MockHqEmbeddingProvider));

        let response = search_hybrid_response(
            &graph,
            &standard_queue,
            Some(&hq_queue),
            "hobbit homeland hills peaceful",
            &HybridSearchConfig {
                alpha: 1.0,
                rerank: false,
                limit: 4,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(
            response.outcomes.fts.status,
            SearchStageStatus::IntentionallySkipped
        );
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Applied
        );
        assert_eq!(
            response.outcomes.high_quality_semantic.status,
            SearchStageStatus::Applied
        );
        assert!(!response.results.is_empty());
        assert!(response.results.iter().all(|result| {
            result.sources.semantic_distance.is_some()
                && result.sources.hq_semantic_distance.is_some()
                && result.matched_chunks.iter().all(|chunk| {
                    chunk.semantic_distance.is_some() && chunk.hq_semantic_distance.is_some()
                })
        }));
    }

    #[tokio::test]
    async fn semantic_fingerprint_mismatch_disables_only_the_affected_lane() {
        let (graph, _tmp) = make_graph_with_data();
        populate_hq_embeddings(&graph);

        for incompatible_standard in [true, false] {
            let standard_name = if incompatible_standard {
                "other-standard"
            } else {
                "mock-embed"
            };
            let hq_name = if incompatible_standard {
                "mock-hq"
            } else {
                "other-hq"
            };
            let standard_queue =
                make_named_embed_queue(standard_name, Arc::new(MockEmbeddingProvider));
            let hq_queue = make_named_embed_queue(hq_name, Arc::new(MockHqEmbeddingProvider));

            let response = search_hybrid_response(
                &graph,
                &standard_queue,
                Some(&hq_queue),
                "hobbit",
                &HybridSearchConfig {
                    alpha: 1.0,
                    rerank: false,
                    limit: 4,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let (standard_status, hq_status) = if incompatible_standard {
                (SearchStageStatus::Unavailable, SearchStageStatus::Applied)
            } else {
                (SearchStageStatus::Applied, SearchStageStatus::Unavailable)
            };
            assert_eq!(response.outcomes.standard_semantic.status, standard_status);
            assert_eq!(response.outcomes.high_quality_semantic.status, hq_status);
            assert!(!response.results.is_empty());
            assert!(response.results.iter().all(|result| {
                result.sources.semantic_distance.is_some() != incompatible_standard
                    && result.sources.hq_semantic_distance.is_some() == incompatible_standard
            }));
        }
    }

    #[tokio::test]
    async fn semantic_failure_preserves_results_from_the_other_lane() {
        let (graph, _tmp) = make_graph_with_data();
        populate_hq_embeddings(&graph);

        for failing_standard in [true, false] {
            let standard_provider: Arc<dyn EmbeddingProvider> = if failing_standard {
                Arc::new(FailingEmbeddingProvider)
            } else {
                Arc::new(MockEmbeddingProvider)
            };
            let hq_provider: Arc<dyn EmbeddingProvider> = if failing_standard {
                Arc::new(MockHqEmbeddingProvider)
            } else {
                Arc::new(FailingHqEmbeddingProvider)
            };
            let standard_queue = make_named_embed_queue("mock-embed", standard_provider);
            let hq_queue = make_named_embed_queue("mock-hq", hq_provider);

            let response = search_hybrid_response(
                &graph,
                &standard_queue,
                Some(&hq_queue),
                "hobbit",
                &HybridSearchConfig {
                    alpha: 1.0,
                    rerank: false,
                    limit: 4,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let (standard_status, hq_status) = if failing_standard {
                (SearchStageStatus::Failed, SearchStageStatus::Applied)
            } else {
                (SearchStageStatus::Applied, SearchStageStatus::Failed)
            };
            assert_eq!(response.outcomes.standard_semantic.status, standard_status);
            assert_eq!(response.outcomes.high_quality_semantic.status, hq_status);
            assert!(!response.results.is_empty());
            assert!(response.results.iter().all(|result| {
                result.sources.semantic_distance.is_some() != failing_standard
                    && result.sources.hq_semantic_distance.is_some() == failing_standard
            }));
        }
    }

    #[tokio::test]
    async fn test_hybrid_node_deduplication() {
        // Each node should appear at most once, even when multiple chunks
        // from the same node are found by different search paths.
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: false,
            limit: 10,
            hq_semantic_boost: 3.0,
        };

        let results = search_hybrid(&graph, &queue, None, "hobbit ring", &config)
            .await
            .unwrap();

        let ids: Vec<_> = results.iter().map(|r| r.node.id).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "Duplicate node IDs found in hybrid search results"
        );
    }

    #[tokio::test]
    async fn test_hybrid_tracks_matched_chunks_separately_from_full_context() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_queue_no_workers();

        let config = HybridSearchConfig {
            alpha: 0.0,
            rerank: false,
            limit: 1,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "wizard staff", &config)
            .await
            .unwrap();

        let gandalf = results
            .iter()
            .find(|result| result.node.name == "Gandalf")
            .expect("Expected Gandalf in FTS results");

        assert!(gandalf.chunks.len() > gandalf.matched_chunks.len());
        assert_eq!(gandalf.matched_chunks.len(), 1);
        assert!(gandalf.matched_chunks[0].content.contains("wizard staff"));
        assert!(gandalf.matched_tokens() < gandalf.total_tokens());
    }

    #[tokio::test]
    async fn test_sqlite_hybrid_search_applies_reranking() {
        let tmp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(tmp.path()).unwrap();

        let archivist = ObjectBuilder::character("Foundation Archivist".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .add_text_chunk(
                archivist,
                "Foundation records describe public Encyclopedia planning.".to_string(),
                ChunkType::Imported,
            )
            .unwrap();

        let vault_keeper = ObjectBuilder::character("Vault Keeper".to_string())
            .add_to_graph(&graph)
            .unwrap();
        graph
            .add_text_chunk(
                vault_keeper,
                "Foundation vault protocol protects the hidden crisis archive.".to_string(),
                ChunkType::Imported,
            )
            .unwrap();

        let queue = make_keyword_rerank_queue("vault protocol");
        let results = search_hybrid(
            &graph,
            &queue,
            None,
            "Foundation",
            &HybridSearchConfig {
                alpha: 0.0,
                rerank: true,
                fts_limit: 10,
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node.id, vault_keeper);
        assert_eq!(results[0].sources.rerank_score, Some(1.0));
        assert_eq!(results[1].sources.rerank_score, Some(0.1));
    }

    #[tokio::test]
    async fn test_hybrid_dual_path_scores_higher() {
        // A node with chunks found by both FTS and semantic ANN accumulates
        // RRF scores from both paths and should rank above single-path nodes.
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: false,
            limit: 10,
            hq_semantic_boost: 3.0,
        };

        let results = search_hybrid(&graph, &queue, None, "hobbit ring journey", &config)
            .await
            .unwrap();

        let dual: Vec<f32> = results
            .iter()
            .filter(|r| r.sources.fts_rank.is_some() && r.sources.semantic_distance.is_some())
            .map(|r| r.score)
            .collect();

        let fts_only: Vec<f32> = results
            .iter()
            .filter(|r| r.sources.fts_rank.is_some() && r.sources.semantic_distance.is_none())
            .map(|r| r.score)
            .collect();

        if !dual.is_empty() && !fts_only.is_empty() {
            let dual_max = dual.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let single_max = fts_only.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            assert!(
                dual_max >= single_max,
                "Dual-path max score ({dual_max:.6}) should be >= FTS-only max ({single_max:.6})"
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_graceful_no_embedding_worker() {
        // When no embedding worker is registered the function must degrade to
        // FTS-only results and not return an error.
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_queue_no_workers();

        let config = HybridSearchConfig {
            alpha: 0.5, // semantic requested but no worker available
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(
            !results.is_empty(),
            "Expected FTS fallback results when no embedding worker is registered"
        );
        for r in &results {
            assert!(
                r.sources.semantic_distance.is_none(),
                "Unexpected semantic_distance when no embedding worker registered"
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_graceful_no_reranker() {
        // When rerank = true but no reranker is registered the function must
        // return RRF-scored results without error.
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue(); // embedding only, no reranker

        let config = HybridSearchConfig {
            alpha: 0.5,
            rerank: true, // requested; unavailable reranking leaves RRF scores
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "fortress darkness tower", &config)
            .await
            .unwrap();

        for r in &results {
            assert!(
                r.sources.rerank_score.is_none(),
                "Unexpected rerank_score when no reranking worker is registered"
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_limit_respected() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        for limit in [1, 2, 3] {
            let config = HybridSearchConfig {
                rerank: false,
                limit,
                fts_limit: 50,
                semantic_limit: 50,
                ..Default::default()
            };

            let results = search_hybrid(&graph, &queue, None, "the", &config)
                .await
                .unwrap();

            assert!(
                results.len() <= limit,
                "Expected at most {limit} node results, got {}",
                results.len()
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_results_sorted_descending() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 4,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "land tower ring", &config)
            .await
            .unwrap();

        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "Results are not sorted by descending score: {:.6} < {:.6}",
                window[0].score,
                window[1].score
            );
        }
    }

    #[tokio::test]
    async fn test_hybrid_empty_graph_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(tmp.path()).unwrap();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "anything at all", &config)
            .await
            .unwrap();

        assert!(
            results.is_empty(),
            "Expected empty results for an empty graph"
        );
    }

    #[tokio::test]
    async fn structured_response_distinguishes_applied_skipped_and_unavailable_stages() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_queue_no_workers();
        let config = HybridSearchConfig {
            alpha: 0.5,
            rerank: true,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(response.outcomes.fts.status, SearchStageStatus::Applied);
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Unavailable
        );
        assert_eq!(
            response.outcomes.high_quality_semantic.status,
            SearchStageStatus::Unavailable
        );
        assert_eq!(
            response.outcomes.reranking.status,
            SearchStageStatus::Unavailable
        );

        let fts_only = HybridSearchConfig {
            alpha: 0.0,
            rerank: false,
            ..Default::default()
        };
        let response = search_hybrid_response(&graph, &queue, None, "wizard", &fts_only)
            .await
            .unwrap();
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::IntentionallySkipped
        );
        assert_eq!(
            response.outcomes.high_quality_semantic.status,
            SearchStageStatus::IntentionallySkipped
        );
        assert_eq!(
            response.outcomes.reranking.status,
            SearchStageStatus::IntentionallySkipped
        );
    }

    #[tokio::test]
    async fn empty_and_punctuation_only_queries_skip_only_fts() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();
        let config = HybridSearchConfig {
            rerank: false,
            ..Default::default()
        };

        for query in ["", "?!?!!!"] {
            let response = search_hybrid_response(&graph, &queue, None, query, &config)
                .await
                .unwrap();

            assert_eq!(response.query, query);
            assert_eq!(
                response.outcomes.fts.status,
                SearchStageStatus::IntentionallySkipped
            );
            assert_eq!(
                response.outcomes.standard_semantic.status,
                SearchStageStatus::Applied
            );
            assert_eq!(
                response.outcomes.high_quality_semantic.status,
                SearchStageStatus::Unavailable
            );
            assert_eq!(
                response.outcomes.reranking.status,
                SearchStageStatus::IntentionallySkipped
            );
            assert!(
                response
                    .results
                    .iter()
                    .all(|result| result.sources.fts_rank.is_none())
            );
        }
    }

    #[tokio::test]
    async fn fingerprint_mismatch_retains_fts_results_and_reports_unavailable_lane() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_named_embed_queue("other-embed", Arc::new(MockEmbeddingProvider));
        let config = HybridSearchConfig {
            rerank: false,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Unavailable
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.sources.fts_rank.is_some()
                    && result.sources.semantic_distance.is_none())
        );
    }

    #[tokio::test]
    async fn only_fts_sanitizes_the_original_query() {
        let (graph, _tmp) = make_graph_with_data();
        let embedding_queries = Arc::new(Mutex::new(Vec::new()));
        let reranking_queries = Arc::new(Mutex::new(Vec::new()));
        let queue = make_recording_queue(embedding_queries.clone(), reranking_queries.clone());
        let query = "wizard?!";

        let response = search_hybrid_response(
            &graph,
            &queue,
            None,
            query,
            &HybridSearchConfig {
                rerank: true,
                limit: 4,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(response.outcomes.fts.status, SearchStageStatus::Applied);
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Applied
        );
        assert_eq!(
            response.outcomes.reranking.status,
            SearchStageStatus::Applied
        );
        assert_eq!(*embedding_queries.lock().unwrap(), vec![query.to_string()]);
        assert_eq!(*reranking_queries.lock().unwrap(), vec![query.to_string()]);
        assert!(
            response.results.iter().any(|result| {
                result.sources.fts_rank.is_some() && result.node.name == "Gandalf"
            })
        );
    }

    #[tokio::test]
    async fn pre_cancelled_search_preserves_supersession() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();
        let cancellation = CancellationToken::new();
        cancellation.supersede();

        let error = search_hybrid_response_with_cancellation(
            &graph,
            &queue,
            None,
            "wizard",
            &HybridSearchConfig::default(),
            cancellation,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::queue::InferenceError>(),
            Some(crate::queue::InferenceError::Superseded)
        ));
    }

    #[tokio::test]
    async fn cancellation_during_embedding_terminates_search() {
        let (graph, _tmp) = make_graph_with_data();
        let cancellation = CancellationToken::new();
        let queue = make_custom_embed_queue(Arc::new(CancellingEmbeddingProvider {
            cancellation: cancellation.clone(),
        }));

        let error = search_hybrid_response_with_cancellation(
            &graph,
            &queue,
            None,
            "wizard",
            &HybridSearchConfig {
                rerank: false,
                ..Default::default()
            },
            cancellation,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::queue::InferenceError>(),
            Some(crate::queue::InferenceError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn cancellation_during_reranking_terminates_search() {
        let (graph, _tmp) = make_graph_with_data();
        let cancellation = CancellationToken::new();
        let queue = make_cancelling_rerank_queue(cancellation.clone());

        let error = search_hybrid_response_with_cancellation(
            &graph,
            &queue,
            None,
            "wizard",
            &HybridSearchConfig {
                alpha: 0.0,
                rerank: true,
                ..Default::default()
            },
            cancellation,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::queue::InferenceError>(),
            Some(crate::queue::InferenceError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn embedding_failure_retains_fts_results_and_reports_safe_outcome() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_custom_embed_queue(Arc::new(FailingEmbeddingProvider));
        let config = HybridSearchConfig {
            rerank: false,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Failed
        );
        assert!(
            !response
                .outcomes
                .standard_semantic
                .diagnostic
                .as_deref()
                .unwrap()
                .contains("secret")
        );
    }

    #[tokio::test]
    async fn ann_failure_retains_fts_results_and_reports_failed_stage() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_custom_embed_queue(Arc::new(WrongDimensionEmbeddingProvider));
        let config = HybridSearchConfig {
            rerank: false,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(
            response.outcomes.standard_semantic.status,
            SearchStageStatus::Failed
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.sources.fts_rank.is_some())
        );
    }

    #[tokio::test]
    async fn rerank_failure_retains_rrf_results_and_reports_safe_outcome() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_failing_rerank_queue();
        let config = HybridSearchConfig {
            alpha: 0.0,
            rerank: true,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(
            response.outcomes.reranking.status,
            SearchStageStatus::Failed
        );
        assert!(
            !response
                .outcomes
                .reranking
                .diagnostic
                .as_deref()
                .unwrap()
                .contains("secret")
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.sources.rerank_score.is_none())
        );
    }

    #[tokio::test]
    async fn malformed_rerank_success_retains_rrf_results_and_reports_failed_stage() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_empty_rerank_queue();
        let config = HybridSearchConfig {
            alpha: 0.0,
            rerank: true,
            ..Default::default()
        };

        let response = search_hybrid_response(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        assert!(!response.results.is_empty());
        assert_eq!(
            response.outcomes.reranking.status,
            SearchStageStatus::Failed
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.sources.rerank_score.is_none())
        );
    }

    #[tokio::test]
    async fn malformed_rerank_variants_leave_the_complete_rrf_order_unchanged() {
        let (graph, _tmp) = make_graph_with_data();
        let query = "Gandalf OR Frodo OR Shire OR Minas";
        let baseline = search_hybrid_response(
            &graph,
            &make_queue_no_workers(),
            None,
            query,
            &HybridSearchConfig {
                alpha: 0.0,
                rerank: false,
                fts_limit: 20,
                limit: 4,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(baseline.results.len() >= 2);
        let expected: Vec<_> = baseline
            .results
            .iter()
            .map(|result| (result.node.id, result.score))
            .collect();

        for kind in [
            MalformedRerankKind::DuplicateIndex,
            MalformedRerankKind::OutOfBoundsIndex,
            MalformedRerankKind::NonFiniteScore,
        ] {
            let queue = make_custom_rerank_queue(Arc::new(MalformedRerankProvider { kind }));
            let response = search_hybrid_response(
                &graph,
                &queue,
                None,
                query,
                &HybridSearchConfig {
                    alpha: 0.0,
                    rerank: true,
                    fts_limit: 20,
                    limit: 4,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            assert_eq!(
                response.outcomes.reranking.status,
                SearchStageStatus::Failed,
                "unexpected outcome for {kind:?}"
            );
            assert_eq!(
                response
                    .results
                    .iter()
                    .map(|result| (result.node.id, result.score))
                    .collect::<Vec<_>>(),
                expected,
                "malformed {kind:?} response changed the RRF results"
            );
            assert!(
                response
                    .results
                    .iter()
                    .all(|result| result.sources.rerank_score.is_none())
            );
        }
    }

    #[tokio::test]
    async fn test_search_sources_label() {
        let fts_only = SearchSources {
            fts_rank: Some(0),
            ..Default::default()
        };
        assert_eq!(fts_only.label(), "[FTS]");

        let sem_only = SearchSources {
            semantic_distance: Some(0.3),
            ..Default::default()
        };
        assert_eq!(sem_only.label(), "[SEM]");

        let both = SearchSources {
            fts_rank: Some(2),
            semantic_distance: Some(0.1),
            ..Default::default()
        };
        assert_eq!(both.label(), "[FTS+SEM]");

        let all_three = SearchSources {
            fts_rank: Some(0),
            semantic_distance: Some(0.05),
            rerank_score: Some(0.98),
            ..Default::default()
        };
        assert_eq!(all_three.label(), "[FTS+SEM+RR]");

        let all_four = SearchSources {
            fts_rank: Some(0),
            semantic_distance: Some(0.05),
            hq_semantic_distance: Some(0.03),
            rerank_score: Some(0.98),
        };
        assert_eq!(all_four.label(), "[FTS+SEM+HQ+RR]");

        let empty = SearchSources::default();
        assert_eq!(empty.label(), "[?]");
    }

    #[tokio::test]
    async fn test_config_defaults() {
        let c = HybridSearchConfig::default();
        assert_eq!(c.alpha, 0.5);
        assert_eq!(c.fts_limit, 20);
        assert_eq!(c.semantic_limit, 20);
        assert!(c.rerank);
        assert_eq!(c.limit, 3);
    }

    #[tokio::test]
    async fn test_node_result_total_tokens() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 3,
            ..Default::default()
        };

        let results = search_hybrid(&graph, &queue, None, "wizard", &config)
            .await
            .unwrap();

        for r in &results {
            let expected: usize = r.chunks.iter().map(|c| c.token_count).sum();
            assert_eq!(
                r.total_tokens(),
                expected,
                "total_tokens() should equal sum of chunk token_counts"
            );
        }
    }

    /// Natural-language queries with punctuation must not cause an FTS5 syntax
    /// error — this is the regression test for the "fts5: syntax error near '?'"
    /// bug that was triggered by the hybrid search demo queries.
    #[tokio::test]
    async fn test_hybrid_natural_language_query_does_not_error() {
        let (graph, _tmp) = make_graph_with_data();
        let queue = make_embed_queue();

        let config = HybridSearchConfig {
            rerank: false,
            limit: 3,
            ..Default::default()
        };

        // These queries all contain characters that FTS5 rejects as syntax errors
        // when passed verbatim. The sanitiser must handle them gracefully.
        let queries = [
            "Who founded the Foundation and why?",
            "What happened to the Galactic Empire?",
            "psychohistory and mathematical prediction!",
            "robotic civilizations (machine intelligence)",
            "Hari Seldon's plan for humanity",
            "??? pure punctuation only ???",
        ];

        for query in &queries {
            let result = search_hybrid(&graph, &queue, None, query, &config).await;
            assert!(
                result.is_ok(),
                "search_hybrid returned an error for query {query:?}: {:?}",
                result.err()
            );
        }
    }
}
