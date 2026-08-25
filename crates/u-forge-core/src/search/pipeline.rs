//! Internal staged hybrid-search implementation.
//!
//! Each retrieval lane produces the same ranked chunk-evidence shape and owns
//! its stage outcome. Fusion and node aggregation are pure transformations;
//! hydration performs graph reads; reranking owns document construction,
//! response validation, and score application. The coordinator at the bottom
//! only sequences those stages and assembles the public response.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::Result;
use tracing::{debug, info, warn};

use super::{
    ConnectedNode, HybridSearchConfig, MatchedChunk, NodeSearchResult, SearchResponse,
    SearchSources, SearchStageOutcome, SearchStageOutcomes, fts5_sanitize,
};
use crate::KnowledgeGraph;
use crate::ingest::EmbeddingTarget;
use crate::lemonade::RerankDocument;
use crate::queue::{CancellationToken, InferenceQueue};
use crate::types::{ChunkId, ObjectId};

const RRF_K: f32 = 60.0;

/// Borrowed search dependencies plus the normalized, operation-owned inputs.
pub(super) struct SearchRequest<'a> {
    graph: &'a KnowledgeGraph,
    queue: &'a InferenceQueue,
    hq_queue: Option<&'a InferenceQueue>,
    query: &'a str,
    config: NormalizedSearchConfig,
    cancellation: CancellationToken,
}

impl<'a> SearchRequest<'a> {
    pub(super) fn new(
        graph: &'a KnowledgeGraph,
        queue: &'a InferenceQueue,
        hq_queue: Option<&'a InferenceQueue>,
        query: &'a str,
        config: &HybridSearchConfig,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            graph,
            queue,
            hq_queue,
            query,
            config: NormalizedSearchConfig::from(config),
            cancellation,
        }
    }

    fn check_cancelled(&self) -> Result<()> {
        self.cancellation.check_cancelled()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct NormalizedSearchConfig {
    alpha: f32,
    fts_limit: usize,
    semantic_limit: usize,
    rerank: bool,
    limit: usize,
    hq_semantic_boost: f32,
}

impl From<&HybridSearchConfig> for NormalizedSearchConfig {
    fn from(config: &HybridSearchConfig) -> Self {
        Self {
            alpha: config.alpha.clamp(0.0, 1.0),
            fts_limit: config.fts_limit,
            semantic_limit: config.semantic_limit,
            rerank: config.rerank,
            limit: config.limit,
            hq_semantic_boost: config.hq_semantic_boost,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetrievalLane {
    Fts,
    StandardSemantic,
    HighQualitySemantic,
}

impl RetrievalLane {
    fn tag(self) -> &'static str {
        match self {
            Self::Fts => "FTS",
            Self::StandardSemantic => "SEM",
            Self::HighQualitySemantic => "HQ",
        }
    }

    fn contribution_weight(self, config: NormalizedSearchConfig) -> f32 {
        match self {
            Self::Fts => 1.0 - config.alpha,
            Self::StandardSemantic => config.alpha,
            Self::HighQualitySemantic => config.alpha * config.hq_semantic_boost,
        }
    }
}

/// One normalized ranked chunk candidate from any retrieval lane.
#[derive(Debug)]
struct RankedChunkEvidence {
    lane: RetrievalLane,
    rank: usize,
    chunk_id: ChunkId,
    object_id: ObjectId,
    content: String,
    distance: Option<f32>,
}

/// Complete output of one independently degradable retrieval stage.
struct RetrievalStageResult {
    lane: RetrievalLane,
    evidence: Vec<RankedChunkEvidence>,
    outcome: SearchStageOutcome,
}

impl RetrievalStageResult {
    fn empty(lane: RetrievalLane, outcome: SearchStageOutcome) -> Self {
        Self {
            lane,
            evidence: Vec::new(),
            outcome,
        }
    }

    fn applied(lane: RetrievalLane, evidence: Vec<RankedChunkEvidence>) -> Self {
        Self {
            lane,
            evidence,
            outcome: SearchStageOutcome::applied(),
        }
    }

    /// Keep verbose candidate details behind the normalized lane boundary.
    fn log_candidates(&self) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }

        let mut buffer = format!(
            "── HYBRID RETRIEVAL {} ({} chunks) ──\n",
            self.lane.tag(),
            self.evidence.len()
        );
        for evidence in &self.evidence {
            let snippet: String = evidence.content.chars().take(80).collect();
            let _ = writeln!(
                buffer,
                "  {}[{}] chunk={} obj={} distance={:?} content={snippet:?}…",
                self.lane.tag(),
                evidence.rank,
                evidence.chunk_id,
                evidence.object_id,
                evidence.distance
            );
        }
        debug!("{buffer}");
    }
}

#[derive(Debug, Clone, Copy)]
enum SemanticLane {
    Standard,
    HighQuality,
}

impl SemanticLane {
    fn retrieval_lane(self) -> RetrievalLane {
        match self {
            Self::Standard => RetrievalLane::StandardSemantic,
            Self::HighQuality => RetrievalLane::HighQualitySemantic,
        }
    }

    fn queue<'a>(self, request: &'a SearchRequest<'_>) -> Option<&'a InferenceQueue> {
        match self {
            Self::Standard => Some(request.queue),
            Self::HighQuality => request.hq_queue,
        }
    }

    fn target(self) -> EmbeddingTarget {
        match self {
            Self::Standard => EmbeddingTarget::Standard,
            Self::HighQuality => EmbeddingTarget::HighQuality,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Standard => "standard semantic",
            Self::HighQuality => "HQ semantic",
        }
    }

    fn disabled_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::skipped(format!(
            "{} search disabled by FTS-only mode",
            self.display_name()
        ))
    }

    fn missing_lane_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::unavailable(format!(
            "{} search unavailable because no compatible embedding lane exists",
            self.display_name()
        ))
    }

    fn missing_worker_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::unavailable(format!(
            "{} search unavailable because no compatible embedding worker exists",
            self.display_name()
        ))
    }

    fn unknown_identity_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::unavailable(format!(
            "{} search unavailable because its embedding identity is unknown",
            self.display_name()
        ))
    }

    fn incompatible_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::unavailable(format!(
            "{} search unavailable because its embedding space is incompatible",
            self.display_name()
        ))
    }

    fn embedding_failed_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::failed(format!(
            "{} query embedding failed; other available results were retained",
            self.display_name()
        ))
    }

    fn retrieval_failed_outcome(self) -> SearchStageOutcome {
        SearchStageOutcome::failed(format!(
            "{} retrieval failed; other available results were retained",
            self.display_name()
        ))
    }

    fn search(
        self,
        graph: &KnowledgeGraph,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(ChunkId, ObjectId, String, f32)>> {
        match self {
            Self::Standard => graph.search_chunks_semantic(query_embedding, limit),
            Self::HighQuality => graph.search_chunks_semantic_hq(query_embedding, limit),
        }
    }
}

fn retrieve_fts(request: &SearchRequest<'_>) -> RetrievalStageResult {
    let lane = RetrievalLane::Fts;
    if request.config.alpha == 1.0 {
        debug!("FTS5 stage skipped (alpha = 1.0)");
        return RetrievalStageResult::empty(
            lane,
            SearchStageOutcome::skipped("FTS5 disabled by pure-semantic mode"),
        );
    }

    let Some(fts_query) = fts5_sanitize(request.query) else {
        debug!("FTS5 stage skipped — query contained no FTS5-safe tokens");
        return RetrievalStageResult::empty(
            lane,
            SearchStageOutcome::skipped(
                "FTS5 skipped because the query contained no searchable terms",
            ),
        );
    };

    debug!(
        query = %fts_query,
        limit = request.config.fts_limit,
        "Running FTS5 search"
    );
    match request
        .graph
        .search_chunks_fts(&fts_query, request.config.fts_limit)
    {
        Ok(results) => RetrievalStageResult::applied(
            lane,
            results
                .into_iter()
                .enumerate()
                .map(
                    |(rank, (chunk_id, object_id, content))| RankedChunkEvidence {
                        lane,
                        rank,
                        chunk_id,
                        object_id,
                        content,
                        distance: None,
                    },
                )
                .collect(),
        ),
        Err(error) => {
            warn!(%error, "FTS5 search failed; retaining other search paths");
            RetrievalStageResult::empty(
                lane,
                SearchStageOutcome::failed(
                    "FTS5 retrieval failed; other available results were retained",
                ),
            )
        }
    }
}

async fn retrieve_semantic(
    request: &SearchRequest<'_>,
    semantic_lane: SemanticLane,
) -> Result<RetrievalStageResult> {
    let lane = semantic_lane.retrieval_lane();
    if request.config.alpha == 0.0 {
        debug!(lane = semantic_lane.display_name(), "Semantic lane skipped");
        return Ok(RetrievalStageResult::empty(
            lane,
            semantic_lane.disabled_outcome(),
        ));
    }

    let Some(queue) = semantic_lane.queue(request) else {
        return Ok(RetrievalStageResult::empty(
            lane,
            semantic_lane.missing_lane_outcome(),
        ));
    };
    if !queue.has_embedding() {
        info!(
            lane = semantic_lane.display_name(),
            "Semantic lane has no embedding worker"
        );
        return Ok(RetrievalStageResult::empty(
            lane,
            semantic_lane.missing_worker_outcome(),
        ));
    }

    let Some(fingerprint) = queue.embedding_space_fingerprint() else {
        return Ok(RetrievalStageResult::empty(
            lane,
            semantic_lane.unknown_identity_outcome(),
        ));
    };
    if let Err(error) = request
        .graph
        .ensure_embedding_space(semantic_lane.target(), fingerprint)
    {
        warn!(
            %error,
            lane = semantic_lane.display_name(),
            "Semantic lane disabled for this search"
        );
        return Ok(RetrievalStageResult::empty(
            lane,
            semantic_lane.incompatible_outcome(),
        ));
    }

    request.check_cancelled()?;
    debug!(
        lane = semantic_lane.display_name(),
        "Embedding query for semantic ANN search"
    );
    let query_embedding = match queue
        .submit_embed_with_cancellation(request.query, request.cancellation.clone())
        .await
    {
        Ok(embedding) => embedding,
        Err(_error) if request.cancellation.is_cancelled() => {
            return Err(request.cancellation.error().into());
        }
        Err(error) => {
            warn!(
                %error,
                lane = semantic_lane.display_name(),
                "Query embedding failed; retaining other search paths"
            );
            return Ok(RetrievalStageResult::empty(
                lane,
                semantic_lane.embedding_failed_outcome(),
            ));
        }
    };

    request.check_cancelled()?;
    debug!(
        lane = semantic_lane.display_name(),
        limit = request.config.semantic_limit,
        "Running semantic ANN search"
    );
    let results = match semantic_lane.search(
        request.graph,
        &query_embedding,
        request.config.semantic_limit,
    ) {
        Ok(results) => results,
        Err(_error) if request.cancellation.is_cancelled() => {
            return Err(request.cancellation.error().into());
        }
        Err(error) => {
            warn!(
                %error,
                lane = semantic_lane.display_name(),
                "Semantic ANN search failed; retaining other search paths"
            );
            return Ok(RetrievalStageResult::empty(
                lane,
                semantic_lane.retrieval_failed_outcome(),
            ));
        }
    };

    Ok(RetrievalStageResult::applied(
        lane,
        results
            .into_iter()
            .enumerate()
            .map(
                |(rank, (chunk_id, object_id, content, distance))| RankedChunkEvidence {
                    lane,
                    rank,
                    chunk_id,
                    object_id,
                    content,
                    distance: Some(distance),
                },
            )
            .collect(),
    ))
}

/// Per-chunk state after all retrieval contributions have been fused.
struct ChunkMerge {
    object_id: ObjectId,
    chunk_id: ChunkId,
    content: String,
    rrf_score: f32,
    fts_rank: Option<usize>,
    semantic_distance: Option<f32>,
    hq_semantic_distance: Option<f32>,
}

impl ChunkMerge {
    fn from_evidence(evidence: &RankedChunkEvidence) -> Self {
        Self {
            object_id: evidence.object_id,
            chunk_id: evidence.chunk_id,
            content: evidence.content.clone(),
            rrf_score: 0.0,
            fts_rank: None,
            semantic_distance: None,
            hq_semantic_distance: None,
        }
    }

    fn merge(&mut self, evidence: &RankedChunkEvidence, contribution: f32) {
        if self.content.is_empty() {
            self.content.clone_from(&evidence.content);
        }
        self.rrf_score += contribution;
        match evidence.lane {
            RetrievalLane::Fts => self.fts_rank = Some(evidence.rank),
            RetrievalLane::StandardSemantic => self.semantic_distance = evidence.distance,
            RetrievalLane::HighQualitySemantic => self.hq_semantic_distance = evidence.distance,
        }
    }
}

/// Merge every ranked lane through one RRF insertion implementation.
fn fuse_ranked_evidence(
    stages: [&RetrievalStageResult; 3],
    config: NormalizedSearchConfig,
) -> Vec<ChunkMerge> {
    let mut chunks: HashMap<ChunkId, ChunkMerge> = HashMap::new();

    for stage in stages {
        let weight = stage.lane.contribution_weight(config);
        for evidence in &stage.evidence {
            let contribution = weight / (RRF_K + evidence.rank as f32);
            chunks
                .entry(evidence.chunk_id)
                .or_insert_with(|| ChunkMerge::from_evidence(evidence))
                .merge(evidence, contribution);
        }
    }

    let mut fused: Vec<_> = chunks.into_values().collect();
    fused.sort_by(|left, right| {
        descending_score(left.rrf_score, right.rrf_score)
            .then_with(|| left.chunk_id.0.cmp(&right.chunk_id.0))
    });
    log_fused_chunks(&fused);
    fused
}

/// Per-node accumulator produced by grouping chunk-level RRF scores.
#[derive(Default)]
struct NodeAccumulator {
    total_score: f32,
    best_fts_rank: Option<usize>,
    best_semantic_distance: Option<f32>,
    best_hq_semantic_distance: Option<f32>,
    matching_chunk_count: usize,
    matched_chunks: Vec<MatchedChunk>,
}

fn aggregate_nodes(
    fused_chunks: Vec<ChunkMerge>,
    limit: usize,
) -> Vec<(ObjectId, NodeAccumulator)> {
    let mut nodes: HashMap<ObjectId, NodeAccumulator> = HashMap::new();

    for chunk in fused_chunks {
        let accumulator = nodes.entry(chunk.object_id).or_default();
        accumulator.total_score += chunk.rrf_score;
        accumulator.matching_chunk_count += 1;
        accumulator.matched_chunks.push(MatchedChunk {
            id: chunk.chunk_id,
            token_count: crate::text::count_tokens(&chunk.content).max(1),
            content: chunk.content,
            score: chunk.rrf_score,
            fts_rank: chunk.fts_rank,
            semantic_distance: chunk.semantic_distance,
            hq_semantic_distance: chunk.hq_semantic_distance,
        });
        if let Some(rank) = chunk.fts_rank {
            accumulator.best_fts_rank = Some(
                accumulator
                    .best_fts_rank
                    .map_or(rank, |previous| previous.min(rank)),
            );
        }
        if let Some(distance) = chunk.semantic_distance {
            accumulator.best_semantic_distance = Some(
                accumulator
                    .best_semantic_distance
                    .map_or(distance, |previous| previous.min(distance)),
            );
        }
        if let Some(distance) = chunk.hq_semantic_distance {
            accumulator.best_hq_semantic_distance = Some(
                accumulator
                    .best_hq_semantic_distance
                    .map_or(distance, |previous| previous.min(distance)),
            );
        }
    }

    let mut ranked: Vec<_> = nodes.into_iter().collect();
    ranked.sort_by(|left, right| {
        descending_score(left.1.total_score, right.1.total_score)
            .then_with(|| left.0.0.cmp(&right.0.0))
    });
    ranked.truncate(limit);
    log_aggregated_nodes(&ranked, limit);
    ranked
}

/// Convert ranked node accumulators into complete public results using graph reads only.
fn hydrate_nodes(
    graph: &KnowledgeGraph,
    ranked_nodes: Vec<(ObjectId, NodeAccumulator)>,
) -> Result<Vec<NodeSearchResult>> {
    let mut results = Vec::with_capacity(ranked_nodes.len());

    for (object_id, accumulator) in ranked_nodes {
        let node = match graph.get_object(object_id)? {
            Some(metadata) => metadata,
            None => {
                warn!(
                    id = %object_id,
                    "Winning node disappeared during hydration; skipping"
                );
                continue;
            }
        };
        let chunks = graph.get_text_chunks(object_id)?;
        let edges = graph.get_relationships(object_id)?;

        let mut connected_node_names = HashMap::new();
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
                Some(other) => {
                    connected_node_names.insert(
                        other_id,
                        ConnectedNode {
                            name: other.name,
                            object_type: other.object_type,
                        },
                    );
                }
                None => {
                    warn!(
                        id = %other_id,
                        "Edge endpoint node not found; omitting connected-node summary"
                    );
                }
            }
        }

        results.push(NodeSearchResult {
            node,
            chunks,
            matched_chunks: accumulator.matched_chunks,
            edges,
            connected_node_names,
            score: accumulator.total_score,
            sources: SearchSources {
                fts_rank: accumulator.best_fts_rank,
                semantic_distance: accumulator.best_semantic_distance,
                hq_semantic_distance: accumulator.best_hq_semantic_distance,
                rerank_score: None,
            },
        });
    }

    log_hydrated_nodes(&results);
    Ok(results)
}

struct RerankStageResult {
    results: Vec<NodeSearchResult>,
    outcome: SearchStageOutcome,
}

async fn rerank_nodes(
    request: &SearchRequest<'_>,
    mut results: Vec<NodeSearchResult>,
) -> Result<RerankStageResult> {
    if !request.config.rerank {
        return Ok(RerankStageResult {
            results,
            outcome: SearchStageOutcome::skipped("reranking disabled by configuration"),
        });
    }
    if results.is_empty() {
        return Ok(RerankStageResult {
            results,
            outcome: SearchStageOutcome::skipped(
                "reranking skipped because retrieval returned no nodes",
            ),
        });
    }
    if !request.queue.has_reranking() {
        info!(
            "Reranking was requested but no reranking worker is registered; retaining RRF scores"
        );
        return Ok(RerankStageResult {
            results,
            outcome: SearchStageOutcome::unavailable(
                "reranking unavailable; reciprocal-rank scores were retained",
            ),
        });
    }

    request.check_cancelled()?;
    let documents = build_rerank_documents(&results);
    log_rerank_input(&documents);
    let ranked = match request
        .queue
        .submit_rerank_with_cancellation(
            request.query,
            documents,
            Some(results.len()),
            request.cancellation.clone(),
        )
        .await
    {
        Ok(ranked) => ranked,
        Err(_error) if request.cancellation.is_cancelled() => {
            return Err(request.cancellation.error().into());
        }
        Err(error) => {
            warn!(%error, "Reranking failed; retaining RRF scores");
            return Ok(RerankStageResult {
                results,
                outcome: SearchStageOutcome::failed(
                    "reranking failed; reciprocal-rank scores were retained",
                ),
            });
        }
    };

    if let Err(reason) = apply_rerank_scores(&mut results, &ranked) {
        warn!(
            reason,
            "Reranking response was invalid; retaining RRF scores"
        );
        return Ok(RerankStageResult {
            results,
            outcome: SearchStageOutcome::failed(
                "reranking returned an invalid response; reciprocal-rank scores were retained",
            ),
        });
    }

    log_rerank_output(&results);
    Ok(RerankStageResult {
        results,
        outcome: SearchStageOutcome::applied(),
    })
}

fn build_rerank_documents(results: &[NodeSearchResult]) -> Vec<String> {
    results
        .iter()
        .map(|result| {
            let edge_lines: Vec<String> = result
                .edges
                .iter()
                .filter_map(|edge| {
                    let from_name = if edge.from == result.node.id {
                        result.node.name.clone()
                    } else {
                        result.connected_node_names.get(&edge.from)?.name.clone()
                    };
                    let to_name = if edge.to == result.node.id {
                        result.node.name.clone()
                    } else {
                        result.connected_node_names.get(&edge.to)?.name.clone()
                    };
                    Some(format!(
                        "{} {} {}",
                        from_name,
                        edge.edge_type.as_str(),
                        to_name
                    ))
                })
                .collect();
            let mut document = result.node.flatten_for_embedding(&edge_lines);
            if !result.matched_chunks.is_empty() {
                document.push_str("\nMatched content:");
                for chunk in &result.matched_chunks {
                    document.push('\n');
                    document.push_str(&chunk.content);
                }
            }
            document
        })
        .collect()
}

fn apply_rerank_scores(
    results: &mut [NodeSearchResult],
    ranked: &[RerankDocument],
) -> std::result::Result<(), &'static str> {
    if ranked.len() != results.len() {
        return Err("reranker returned an incomplete result set");
    }

    let mut scores = vec![None; results.len()];
    for document in ranked {
        if document.index >= results.len() {
            return Err("reranker returned an out-of-bounds index");
        }
        if !document.score.is_finite() {
            return Err("reranker returned a non-finite score");
        }
        if scores[document.index].replace(document.score).is_some() {
            return Err("reranker returned a duplicate index");
        }
    }

    for (result, score) in results.iter_mut().zip(scores) {
        let score = score.expect("validated reranking response covers every result");
        result.sources.rerank_score = Some(score);
        result.score = score;
    }
    results.sort_by(|left, right| {
        descending_score(left.score, right.score).then_with(|| left.node.id.0.cmp(&right.node.id.0))
    });
    Ok(())
}

fn descending_score(left: f32, right: f32) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

fn log_fused_chunks(chunks: &[ChunkMerge]) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut buffer = format!("── HYBRID FUSION ({} unique chunks) ──\n", chunks.len());
    for chunk in chunks {
        let _ = writeln!(
            buffer,
            "  chunk={} obj={} score={:.6} fts_rank={:?} sem_dist={:?} hq_dist={:?}",
            chunk.chunk_id,
            chunk.object_id,
            chunk.rrf_score,
            chunk.fts_rank,
            chunk.semantic_distance,
            chunk.hq_semantic_distance
        );
    }
    debug!("{buffer}");
}

fn log_aggregated_nodes(nodes: &[(ObjectId, NodeAccumulator)], limit: usize) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut buffer = format!(
        "── HYBRID NODE AGGREGATION ({} nodes, limit {limit}) ──\n",
        nodes.len()
    );
    for (object_id, accumulator) in nodes {
        let _ = writeln!(
            buffer,
            "  obj={object_id} score={:.6} chunks={} fts_rank={:?} sem_dist={:?} hq_dist={:?}",
            accumulator.total_score,
            accumulator.matching_chunk_count,
            accumulator.best_fts_rank,
            accumulator.best_semantic_distance,
            accumulator.best_hq_semantic_distance
        );
    }
    debug!("{buffer}");
}

fn log_hydrated_nodes(results: &[NodeSearchResult]) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut buffer = format!("── HYBRID HYDRATION ({} nodes) ──\n", results.len());
    for (index, result) in results.iter().enumerate() {
        let _ = writeln!(
            buffer,
            "  [{index}] name={:?} type={:?} score={:.6}",
            result.node.name, result.node.object_type, result.score
        );
    }
    debug!("{buffer}");
}

fn log_rerank_input(documents: &[String]) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut buffer = format!("── HYBRID RERANK INPUT ({} docs) ──\n", documents.len());
    for (index, document) in documents.iter().enumerate() {
        let snippet: String = document.chars().take(120).collect();
        let _ = writeln!(
            buffer,
            "  [{index}] ({} chars) {snippet:?}…",
            document.len()
        );
    }
    debug!("{buffer}");
}

fn log_rerank_output(results: &[NodeSearchResult]) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut buffer = String::from("── HYBRID RERANK OUTPUT ──\n");
    for (index, result) in results.iter().enumerate() {
        let _ = writeln!(
            buffer,
            "  [{index}] name={:?} score={:.6} rerank_score={:?}",
            result.node.name, result.score, result.sources.rerank_score
        );
    }
    debug!("{buffer}");
}

/// Sequence the concrete stages and assemble one public response.
pub(super) async fn execute(request: SearchRequest<'_>) -> Result<SearchResponse> {
    request.check_cancelled()?;

    let fts = retrieve_fts(&request);
    let standard = retrieve_semantic(&request, SemanticLane::Standard).await?;
    let high_quality = retrieve_semantic(&request, SemanticLane::HighQuality).await?;
    fts.log_candidates();
    standard.log_candidates();
    high_quality.log_candidates();

    request.check_cancelled()?;
    let fused = fuse_ranked_evidence([&fts, &standard, &high_quality], request.config);
    let ranked_nodes = aggregate_nodes(fused, request.config.limit);

    request.check_cancelled()?;
    let hydrated = hydrate_nodes(request.graph, ranked_nodes)?;

    request.check_cancelled()?;
    let reranked = rerank_nodes(&request, hydrated).await?;
    request.check_cancelled()?;

    debug!(
        results = reranked.results.len(),
        "Returning staged hybrid-search response"
    );
    Ok(SearchResponse {
        query: request.query.to_string(),
        results: reranked.results,
        outcomes: SearchStageOutcomes {
            fts: fts.outcome,
            standard_semantic: standard.outcome,
            high_quality_semantic: high_quality.outcome,
            reranking: reranked.outcome,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectBuilder;
    use tempfile::TempDir;

    fn evidence(
        lane: RetrievalLane,
        rank: usize,
        chunk_id: ChunkId,
        object_id: ObjectId,
        distance: Option<f32>,
    ) -> RankedChunkEvidence {
        RankedChunkEvidence {
            lane,
            rank,
            chunk_id,
            object_id,
            content: format!("content-{chunk_id}"),
            distance,
        }
    }

    #[test]
    fn fusion_merges_lane_evidence_once_before_node_aggregation() {
        let shared_node = ObjectId::new_v4();
        let other_node = ObjectId::new_v4();
        let shared_chunk = ChunkId::new_v4();
        let other_chunk = ChunkId::new_v4();
        let fts = RetrievalStageResult::applied(
            RetrievalLane::Fts,
            vec![
                evidence(RetrievalLane::Fts, 0, shared_chunk, shared_node, None),
                evidence(RetrievalLane::Fts, 1, other_chunk, other_node, None),
            ],
        );
        let standard = RetrievalStageResult::applied(
            RetrievalLane::StandardSemantic,
            vec![evidence(
                RetrievalLane::StandardSemantic,
                0,
                shared_chunk,
                shared_node,
                Some(0.2),
            )],
        );
        let high_quality = RetrievalStageResult::applied(
            RetrievalLane::HighQualitySemantic,
            vec![evidence(
                RetrievalLane::HighQualitySemantic,
                0,
                shared_chunk,
                shared_node,
                Some(0.1),
            )],
        );
        let config = NormalizedSearchConfig {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: false,
            limit: 3,
            hq_semantic_boost: 3.0,
        };

        let fused = fuse_ranked_evidence([&fts, &standard, &high_quality], config);
        assert_eq!(fused.len(), 2);
        let shared = fused
            .iter()
            .find(|chunk| chunk.chunk_id == shared_chunk)
            .unwrap();
        let expected = (0.5 + 0.5 + 1.5) / RRF_K;
        assert!((shared.rrf_score - expected).abs() < f32::EPSILON);
        assert_eq!(shared.fts_rank, Some(0));
        assert_eq!(shared.semantic_distance, Some(0.2));
        assert_eq!(shared.hq_semantic_distance, Some(0.1));

        let nodes = aggregate_nodes(fused, 3);
        assert_eq!(nodes[0].0, shared_node);
        assert_eq!(nodes[0].1.matched_chunks.len(), 1);
    }

    #[test]
    fn equal_scores_use_stable_identifier_tie_breakers() {
        let low_object = ObjectId::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let high_object = ObjectId::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let low_chunk = ChunkId::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let high_chunk = ChunkId::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let fts = RetrievalStageResult::applied(
            RetrievalLane::Fts,
            vec![evidence(
                RetrievalLane::Fts,
                0,
                high_chunk,
                high_object,
                None,
            )],
        );
        let standard = RetrievalStageResult::applied(
            RetrievalLane::StandardSemantic,
            vec![evidence(
                RetrievalLane::StandardSemantic,
                0,
                low_chunk,
                low_object,
                Some(0.1),
            )],
        );
        let high_quality =
            RetrievalStageResult::applied(RetrievalLane::HighQualitySemantic, Vec::new());
        let config = NormalizedSearchConfig {
            alpha: 0.5,
            fts_limit: 20,
            semantic_limit: 20,
            rerank: false,
            limit: 3,
            hq_semantic_boost: 3.0,
        };

        let fused = fuse_ranked_evidence([&fts, &standard, &high_quality], config);
        assert_eq!(
            fused.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
            vec![low_chunk, high_chunk]
        );
        let nodes = aggregate_nodes(fused, 3);
        assert_eq!(
            nodes
                .iter()
                .map(|(object_id, _)| *object_id)
                .collect::<Vec<_>>(),
            vec![low_object, high_object]
        );
    }

    #[test]
    fn equal_rerank_scores_use_stable_node_identifier_tie_breaker() {
        let temp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp.path()).unwrap();
        let first = ObjectBuilder::character("First".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let second = ObjectBuilder::character("Second".to_string())
            .add_to_graph(&graph)
            .unwrap();
        let ranked = vec![
            (
                second,
                NodeAccumulator {
                    total_score: 1.0,
                    ..Default::default()
                },
            ),
            (
                first,
                NodeAccumulator {
                    total_score: 1.0,
                    ..Default::default()
                },
            ),
        ];
        let mut results = hydrate_nodes(&graph, ranked).unwrap();
        let scores = vec![
            RerankDocument {
                index: 1,
                score: 0.5,
                document: None,
            },
            RerankDocument {
                index: 0,
                score: 0.5,
                document: None,
            },
        ];

        apply_rerank_scores(&mut results, &scores).unwrap();
        let mut expected = vec![first, second];
        expected.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            results
                .iter()
                .map(|result| result.node.id)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn hydration_skips_a_node_deleted_after_retrieval() {
        let temp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp.path()).unwrap();
        let ranked = vec![(
            ObjectId::new_v4(),
            NodeAccumulator {
                total_score: 1.0,
                ..Default::default()
            },
        )];

        assert!(hydrate_nodes(&graph, ranked).unwrap().is_empty());
    }
}
