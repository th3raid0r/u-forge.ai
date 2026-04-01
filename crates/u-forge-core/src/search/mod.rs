//! Node-centric hybrid search pipeline combining FTS5 keyword search,
//! sqlite-vec ANN semantic search, and optional cross-encoder reranking.
//!
//! # Overview
//!
//! [`search_hybrid`] is the main entry point. It uses chunk-level search
//! signals to identify the most relevant knowledge graph **nodes**, then
//! returns each winning node with its full content (all text chunks), attached
//! edges, and lightweight summaries of connected nodes.
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
//! 5. For each winning node, load full metadata, all text chunks, all edges,
//!    and connected node summaries.
//! 6. Optionally rerank the winning nodes using concatenated chunk content.
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

mod sanitize;

use std::collections::HashMap;

use anyhow::Result;
use tracing::{debug, info, instrument, warn};

use crate::queue::InferenceQueue;
use crate::types::{Edge, ObjectId, ObjectMetadata, TextChunk};
use crate::KnowledgeGraph;

use sanitize::fts5_sanitize;

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
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: true,
            limit: 3,
        }
    }
}

// ── Public result types ───────────────────────────────────────────────────────

/// A single node result from [`search_hybrid`].
///
/// Contains the complete context for one knowledge graph node: its metadata,
/// all text chunks, all incident edges, and lightweight summaries of the
/// nodes on the other end of those edges.  This gives downstream consumers
/// (LLMs, UI search results) everything they need about the node in one shot.
#[derive(Debug, Clone)]
pub struct NodeSearchResult {
    /// Full metadata for the matched knowledge graph node.
    pub node: ObjectMetadata,

    /// All text chunks belonging to this node, in storage order.
    pub chunks: Vec<TextChunk>,

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

// ── Internal accumulators ─────────────────────────────────────────────────────

/// Per-chunk state during the RRF merge pass (before node aggregation).
struct ChunkMerge {
    /// Object (node) this chunk belongs to, as a UUID string.
    object_id_str: String,
    /// Accumulated RRF score for this chunk.
    rrf_score: f32,
    /// FTS5 rank position, if this chunk was found by FTS.
    fts_rank: Option<usize>,
    /// Cosine distance, if this chunk was found by 768-dim semantic ANN.
    semantic_distance: Option<f32>,
    /// Cosine distance, if this chunk was found by 4096-dim HQ semantic ANN.
    hq_semantic_distance: Option<f32>,
}

/// Per-node accumulator produced by grouping chunk-level RRF scores.
#[derive(Default)]
struct NodeAccumulator {
    /// Sum of RRF scores across all matching chunks for this node.
    total_score: f32,
    /// Best (lowest) FTS rank among the node's matching chunks.
    best_fts_rank: Option<usize>,
    /// Best (lowest) 768-dim semantic distance among the node's matching chunks.
    best_semantic_distance: Option<f32>,
    /// Best (lowest) 4096-dim HQ semantic distance among the node's matching chunks.
    best_hq_semantic_distance: Option<f32>,
    /// Number of distinct chunks that contributed to this node's score.
    matching_chunk_count: usize,
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
pub async fn search_hybrid(
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
) -> Result<Vec<NodeSearchResult>> {
    tracing::Span::current().record("query", query);

    let alpha = config.alpha.clamp(0.0, 1.0);

    // ── Stage 1: FTS5 search (sync, sub-millisecond) ──────────────────────────
    // Always run first — it is instant and does not need the embedding RTT.
    // Skip only when alpha == 1.0 (pure semantic requested).

    let fts_results = if alpha < 1.0 {
        match fts5_sanitize(query) {
            None => {
                debug!("FTS5 stage skipped — query contained no FTS5-safe tokens");
                Vec::new()
            }
            Some(fts_query) => {
                debug!(
                    "Running FTS5 search (sanitised query: {:?}, limit {})",
                    fts_query, config.fts_limit
                );
                graph.search_chunks_fts(&fts_query, config.fts_limit)?
            }
        }
    } else {
        debug!("FTS5 stage skipped (alpha = 1.0)");
        Vec::new()
    };

    // ── Stage 2+3: Embed query then ANN search ────────────────────────────────
    // Skip when alpha == 0.0 (pure FTS) or when no embedding worker exists.

    let semantic_results = if alpha > 0.0 && queue.has_embedding() {
        debug!("Embedding query for semantic ANN search");
        match queue.embed(query).await {
            Err(e) => {
                warn!("Query embedding failed — falling back to FTS-only results: {e}");
                Vec::new()
            }
            Ok(query_vec) => {
                debug!(
                    "Running semantic ANN search (limit {})",
                    config.semantic_limit
                );
                match graph.search_chunks_semantic(&query_vec, config.semantic_limit) {
                    Ok(results) => results,
                    Err(e) => {
                        warn!("Semantic ANN search failed — falling back to FTS results: {e}");
                        Vec::new()
                    }
                }
            }
        }
    } else {
        if alpha > 0.0 {
            // alpha > 0 but no worker — degrade gracefully.
            info!(
                "Semantic search skipped — no embedding workers registered in this \
                 InferenceQueue. Returning FTS-only results."
            );
        } else {
            debug!("Semantic stage skipped (alpha = 0.0)");
        }
        Vec::new()
    };

    // ── Stage 2b+3b: HQ embed query then HQ ANN search ───────────────────────
    // Uses the 4096-dim model when an hq_queue is provided.  Contributes its
    // own independent RRF signal alongside the standard 768-dim path.

    let hq_semantic_results = if alpha > 0.0 {
        match hq_queue {
            None => Vec::new(),
            Some(hq_q) if !hq_q.has_embedding() => {
                info!(
                    "HQ semantic search skipped — no embedding workers in hq_queue."
                );
                Vec::new()
            }
            Some(hq_q) => {
                debug!("Embedding query for HQ semantic ANN search");
                match hq_q.embed(query).await {
                    Err(e) => {
                        warn!("HQ query embedding failed — skipping HQ path: {e}");
                        Vec::new()
                    }
                    Ok(query_vec) => {
                        debug!(
                            "Running HQ semantic ANN search (limit {})",
                            config.semantic_limit
                        );
                        match graph.search_chunks_semantic_hq(&query_vec, config.semantic_limit) {
                            Ok(results) => results,
                            Err(e) => {
                                warn!("HQ semantic ANN search failed — skipping HQ path: {e}");
                                Vec::new()
                            }
                        }
                    }
                }
            }
        }
    } else {
        Vec::new()
    };

    debug!(
        "Candidate pool: {} FTS chunks, {} semantic chunks, {} HQ semantic chunks",
        fts_results.len(),
        semantic_results.len(),
        hq_semantic_results.len()
    );

    // ── Diagnostic: Stage 1 (FTS5) results ──────────────────────────────────
    {
        use std::fmt::Write as _;
        let mut buf = format!("── HYBRID STAGE 1: FTS5 ({} chunks) ──\n", fts_results.len());
        for (rank, (chunk_id, obj_id, content)) in fts_results.iter().enumerate() {
            let snippet: String = content.chars().take(80).collect();
            let _ = writeln!(buf, "  FTS[{rank}] chunk={} obj={} content={snippet:?}…",
                chunk_id.hyphenated(), obj_id.hyphenated());
        }
        info!("{buf}");
    }

    // ── Diagnostic: Stage 2+3 (Semantic ANN) results ────────────────────────
    {
        use std::fmt::Write as _;
        let mut buf = format!("── HYBRID STAGE 2+3: Semantic ANN ({} chunks) ──\n", semantic_results.len());
        for (rank, (chunk_id, obj_id, content, distance)) in semantic_results.iter().enumerate() {
            let snippet: String = content.chars().take(80).collect();
            let _ = writeln!(buf, "  SEM[{rank}] chunk={} obj={} dist={distance:.4} content={snippet:?}…",
                chunk_id.hyphenated(), obj_id.hyphenated());
        }
        info!("{buf}");
    }

    // ── Diagnostic: Stage 2b+3b (HQ Semantic ANN) results ───────────────────
    {
        use std::fmt::Write as _;
        let mut buf = format!("── HYBRID STAGE 2b+3b: HQ Semantic ANN ({} chunks) ──\n", hq_semantic_results.len());
        for (rank, (chunk_id, obj_id, content, distance)) in hq_semantic_results.iter().enumerate() {
            let snippet: String = content.chars().take(80).collect();
            let _ = writeln!(buf, "  HQ[{rank}] chunk={} obj={} dist={distance:.4} content={snippet:?}…",
                chunk_id.hyphenated(), obj_id.hyphenated());
        }
        info!("{buf}");
    }

    // ── Stage 4: Reciprocal Rank Fusion merge (chunk level) ───────────────────
    //
    // Deduplicate chunks by chunk_id and accumulate RRF scores from both paths.
    // k = 60 is the standard RRF constant (Cormack & Clarke, SIGIR 2009).
    const K: f32 = 60.0;

    let mut chunk_merge: HashMap<String, ChunkMerge> = HashMap::new();

    for (rank, (chunk_id, obj_id, _content)) in fts_results.into_iter().enumerate() {
        let score = (1.0 - alpha) / (K + rank as f32);
        let entry = chunk_merge
            .entry(chunk_id.hyphenated().to_string())
            .or_insert_with(|| ChunkMerge {
                object_id_str: obj_id.hyphenated().to_string(),
                rrf_score: 0.0,
                fts_rank: None,
                semantic_distance: None,
                hq_semantic_distance: None,
            });
        entry.rrf_score += score;
        entry.fts_rank = Some(rank);
    }

    for (rank, (chunk_id, obj_id, _content, distance)) in semantic_results.into_iter().enumerate() {
        let score = alpha / (K + rank as f32);
        let entry = chunk_merge
            .entry(chunk_id.hyphenated().to_string())
            .or_insert_with(|| ChunkMerge {
                object_id_str: obj_id.hyphenated().to_string(),
                rrf_score: 0.0,
                fts_rank: None,
                semantic_distance: None,
                hq_semantic_distance: None,
            });
        entry.rrf_score += score;
        entry.semantic_distance = Some(distance);
    }

    for (rank, (chunk_id, obj_id, _content, distance)) in hq_semantic_results.into_iter().enumerate() {
        let score = alpha / (K + rank as f32);
        let entry = chunk_merge
            .entry(chunk_id.hyphenated().to_string())
            .or_insert_with(|| ChunkMerge {
                object_id_str: obj_id.hyphenated().to_string(),
                rrf_score: 0.0,
                fts_rank: None,
                semantic_distance: None,
                hq_semantic_distance: None,
            });
        entry.rrf_score += score;
        entry.hq_semantic_distance = Some(distance);
    }

    // ── Diagnostic: Stage 4 (RRF merge) results ──────────────────────────────
    {
        use std::fmt::Write as _;
        let mut sorted_chunks: Vec<_> = chunk_merge.iter().collect();
        sorted_chunks.sort_by(|a, b| b.1.rrf_score.partial_cmp(&a.1.rrf_score).unwrap_or(std::cmp::Ordering::Equal));
        let mut buf = format!("── HYBRID STAGE 4: RRF chunk merge ({} unique chunks) ──\n", chunk_merge.len());
        for (chunk_key, cm) in &sorted_chunks {
            let _ = writeln!(buf, "  RRF chunk={chunk_key} obj={} score={:.6} fts_rank={:?} sem_dist={:?}",
                cm.object_id_str, cm.rrf_score, cm.fts_rank, cm.semantic_distance);
        }
        info!("{buf}");
    }

    // ── Stage 5: Node-level aggregation ───────────────────────────────────────
    //
    // Group chunk RRF scores by parent node.  A node's total score is the sum
    // of all its matching chunks' RRF scores.  This naturally promotes nodes
    // that have more matching content or content found by both search paths.

    let mut node_accum: HashMap<String, NodeAccumulator> = HashMap::new();

    for (_chunk_key, cm) in chunk_merge {
        let acc = node_accum.entry(cm.object_id_str).or_default();
        acc.total_score += cm.rrf_score;
        acc.matching_chunk_count += 1;
        if let Some(rank) = cm.fts_rank {
            acc.best_fts_rank = Some(acc.best_fts_rank.map_or(rank, |prev| prev.min(rank)));
        }
        if let Some(dist) = cm.semantic_distance {
            acc.best_semantic_distance = Some(
                acc.best_semantic_distance
                    .map_or(dist, |prev| prev.min(dist)),
            );
        }
        if let Some(dist) = cm.hq_semantic_distance {
            acc.best_hq_semantic_distance = Some(
                acc.best_hq_semantic_distance
                    .map_or(dist, |prev| prev.min(dist)),
            );
        }
    }

    // ── Diagnostic: Stage 5 (Node aggregation) before sort ─────────────────
    {
        use std::fmt::Write as _;
        let mut sorted_nodes: Vec<_> = node_accum.iter().collect();
        sorted_nodes.sort_by(|a, b| b.1.total_score.partial_cmp(&a.1.total_score).unwrap_or(std::cmp::Ordering::Equal));
        let mut buf = format!("── HYBRID STAGE 5: Node aggregation ({} nodes, before sort) ──\n", node_accum.len());
        for (obj_id, acc) in &sorted_nodes {
            let _ = writeln!(buf, "  NODE obj={obj_id} score={:.6} chunks={} best_fts_rank={:?} best_sem_dist={:?} best_hq_dist={:?}",
                acc.total_score, acc.matching_chunk_count, acc.best_fts_rank, acc.best_semantic_distance, acc.best_hq_semantic_distance);
        }
        info!("{buf}");
    }

    // Sort nodes by descending aggregated score and cap at config.limit.
    let mut ranked_nodes: Vec<(String, NodeAccumulator)> = node_accum.into_iter().collect();
    ranked_nodes.sort_by(|a, b| {
        b.1.total_score
            .partial_cmp(&a.1.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked_nodes.truncate(config.limit);

    debug!(
        "{} nodes after aggregation (capped at {}), matching chunks per node: [{}]",
        ranked_nodes.len(),
        config.limit,
        ranked_nodes
            .iter()
            .map(|(_, acc)| acc.matching_chunk_count.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // ── Diagnostic: Stage 5 (after sort + truncate) ───────────────────────────
    {
        use std::fmt::Write as _;
        let mut buf = format!("── HYBRID STAGE 5: After sort + truncate to {} ──\n", config.limit);
        for (obj_id, acc) in &ranked_nodes {
            let _ = writeln!(buf, "  KEPT obj={obj_id} score={:.6} chunks={} best_fts_rank={:?} best_sem_dist={:?} best_hq_dist={:?}",
                acc.total_score, acc.matching_chunk_count, acc.best_fts_rank, acc.best_semantic_distance, acc.best_hq_semantic_distance);
        }
        info!("{buf}");
    }

    // ── Stage 6: Node hydration ───────────────────────────────────────────────
    //
    // For each winning node, load full metadata, all text chunks, all edges,
    // and resolve connected node names.

    let mut results: Vec<NodeSearchResult> = Vec::with_capacity(ranked_nodes.len());

    for (obj_id_str, acc) in ranked_nodes {
        let object_id = parse_uuid(&obj_id_str, "object")?;

        let node = match graph.get_object(object_id)? {
            Some(meta) => meta,
            None => {
                warn!(
                    id = %object_id,
                    "Winning node disappeared during hydration \
                     (deleted between search and fetch?); skipping"
                );
                continue;
            }
        };

        let chunks = graph.get_text_chunks(object_id)?;
        let edges = graph.get_relationships(object_id)?;

        // Resolve connected node names for display context.
        let mut connected_node_names: HashMap<ObjectId, ConnectedNode> = HashMap::new();
        for edge in &edges {
            let other_id = if edge.from == object_id {
                edge.to
            } else {
                edge.from
            };
            if connected_node_names.contains_key(&other_id) {
                continue;
            }
            match graph.get_object(other_id)? {
                Some(other_meta) => {
                    connected_node_names.insert(
                        other_id,
                        ConnectedNode {
                            name: other_meta.name,
                            object_type: other_meta.object_type,
                        },
                    );
                }
                None => {
                    warn!(
                        id = %other_id,
                        "Edge endpoint node not found; omitting from connected_node_names"
                    );
                }
            }
        }

        results.push(NodeSearchResult {
            node,
            chunks,
            edges,
            connected_node_names,
            score: acc.total_score,
            sources: SearchSources {
                fts_rank: acc.best_fts_rank,
                semantic_distance: acc.best_semantic_distance,
                hq_semantic_distance: acc.best_hq_semantic_distance,
                rerank_score: None,
            },
        });
    }

    // ── Diagnostic: Stage 6 (Hydrated nodes) ──────────────────────────────────
    {
        use std::fmt::Write as _;
        let mut buf = format!("── HYBRID STAGE 6: Hydrated nodes ({}) ──\n", results.len());
        for (i, r) in results.iter().enumerate() {
            let _ = writeln!(buf, "  HYDRATED[{i}] name={:?} type={:?} score={:.6} fts_rank={:?} sem_dist={:?}",
                r.node.name, r.node.object_type, r.score,
                r.sources.fts_rank, r.sources.semantic_distance);
        }
        info!("{buf}");
    }

    // ── Stage 7: Optional reranking ───────────────────────────────────────────

    let do_rerank = config.rerank && queue.has_reranking() && !results.is_empty();

    if config.rerank && !queue.has_reranking() {
        info!(
            "Reranking was requested but no reranking worker is registered — \
             returning RRF-scored results."
        );
    }

    if do_rerank {
        debug!("Submitting {} nodes to reranker", results.len());

        // Build one document per node using the full flattened node representation.
        // This gives the cross-encoder the complete node context (name, type,
        // description, all properties, tags, and edges) rather than isolated chunk snippets.
        let documents: Vec<String> = results
            .iter()
            .map(|r| {
                let edge_lines: Vec<String> = r
                    .edges
                    .iter()
                    .filter_map(|e| {
                        let from_name = if e.from == r.node.id {
                            r.node.name.clone()
                        } else {
                            r.connected_node_names.get(&e.from)?.name.clone()
                        };
                        let to_name = if e.to == r.node.id {
                            r.node.name.clone()
                        } else {
                            r.connected_node_names.get(&e.to)?.name.clone()
                        };
                        Some(format!("{} {} {}", from_name, e.edge_type.as_str(), to_name))
                    })
                    .collect();
                r.node.flatten_for_embedding(&edge_lines)
            })
            .collect();

        // ── Diagnostic: Stage 7 (Rerank input) ─────────────────────────────
        {
            use std::fmt::Write as _;
            let mut buf = format!("── HYBRID STAGE 7: Rerank input ({} docs) ──\n", documents.len());
            for (i, doc) in documents.iter().enumerate() {
                let snippet: String = doc.chars().take(120).collect();
                let _ = writeln!(buf, "  RERANK_IN[{i}] ({} chars) {snippet:?}…", doc.len());
            }
            info!("{buf}");
        }

        match queue.rerank(query, documents, Some(results.len())).await {
            Err(e) => {
                warn!("Reranking failed — returning RRF-scored results instead: {e}");
                // Fall through — results already in RRF order.
            }
            Ok(ranked) => {
                // Apply rerank scores and re-sort.
                for rd in &ranked {
                    if rd.index < results.len() {
                        results[rd.index].sources.rerank_score = Some(rd.score);
                        results[rd.index].score = rd.score;
                    } else {
                        warn!(
                            "Reranker returned out-of-bounds index {} (pool size {}), skipping",
                            rd.index,
                            results.len()
                        );
                    }
                }
                results.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // ── Diagnostic: Stage 7 (Rerank output) ─────────────────────
                {
                    use std::fmt::Write as _;
                    let mut buf = String::from("── HYBRID STAGE 7: Rerank output ──\n");
                    for (i, r) in results.iter().enumerate() {
                        let _ = writeln!(buf, "  RERANK_OUT[{i}] name={:?} type={:?} score={:.6} rerank_score={:?} fts_rank={:?} sem_dist={:?}",
                            r.node.name, r.node.object_type,
                            r.score, r.sources.rerank_score,
                            r.sources.fts_rank, r.sources.semantic_distance);
                    }
                    info!("{buf}");
                }

                debug!("Returning {} reranked node results", results.len());
                return Ok(results);
            }
        }
    }

    debug!("Returning {} RRF-scored node results", results.len());
    Ok(results)
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn parse_uuid(s: &str, label: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(s)
        .map_err(|e| anyhow::anyhow!("Invalid {label} UUID '{s}' in hybrid search result: {e}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::ai::embeddings::{EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType};
    use crate::queue::InferenceQueueBuilder;
    use crate::types::ChunkType;
    use crate::{KnowledgeGraph, ObjectBuilder};

    // ── Mock embedding provider ───────────────────────────────────────────────
    //
    // Produces a deterministic 768-dim vector that varies by text content.
    // No Lemonade Server required.

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
        for oid in [wizard_id, hobbit_id, shire_id, city_id] {
            for chunk in graph.get_text_chunks(oid).unwrap() {
                let seed = chunk.content.len() as f32
                    + chunk.content.chars().next().unwrap_or('a') as u32 as f32;
                let embedding: Vec<f32> = (0..768)
                    .map(|i| ((seed + i as f32) % 1000.0) / 1000.0)
                    .collect();
                graph.upsert_chunk_embedding(chunk.id, &embedding).unwrap();
            }
        }

        (graph, tmp)
    }

    fn make_embed_queue() -> InferenceQueue {
        InferenceQueueBuilder::new()
            .with_embedding_provider(Arc::new(MockEmbeddingProvider), "mock-embed".to_string())
            .build()
    }

    fn make_queue_no_workers() -> InferenceQueue {
        InferenceQueueBuilder::new().build()
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

        let results = search_hybrid(&graph, &queue, None, "hobbit homeland hills peaceful", &config)
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
            rerank: true, // requested but will be silently skipped
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

            let results = search_hybrid(&graph, &queue, None, "the", &config).await.unwrap();

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
