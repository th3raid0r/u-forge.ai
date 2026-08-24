use std::collections::HashMap;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use schemars::JsonSchema;
use serde::Deserialize;
use u_forge_core::search::{
    HybridSearchConfig, NodeSearchResult, SearchStageOutcomes, SearchStageStatus, fts5_sanitize,
    search_hybrid_response_with_cancellation,
};
use u_forge_core::{
    KnowledgeGraph,
    queue::{CancellationToken, InferenceQueue},
    types::ObjectId,
};

use super::{ToolError, validation};

/// Format a single [`NodeSearchResult`] into LLM-readable text.
fn format_node_result(result: &NodeSearchResult, index: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "[{}] {} ({}) [id: {}] — score: {:.4} {}\n",
        index + 1,
        result.node.name,
        result.node.object_type,
        result.node.id,
        result.score,
        result.sources.label()
    ));
    if let Some(desc) = result.node.get_property("description") {
        s.push_str(&format!("  Description: {desc}\n"));
    }
    let tags: Vec<&str> = result
        .node
        .get_json_property("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !tags.is_empty() {
        s.push_str(&format!("  Tags: {}\n", tags.join(", ")));
    }
    let content_chunks: Vec<&str> = if result.matched_chunks.is_empty() {
        result
            .chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect()
    } else {
        result
            .matched_chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect()
    };

    if let Some(summary) = first_matched_search_content(&content_chunks) {
        s.push_str(&format!("  Matched content: {summary}\n"));
    }
    s
}

/// Return the best complete matching chunk for a search result.
///
/// Tool results remain intact after formatting. The request fitter makes room
/// by evicting older conversation history, so imposing a separate cap here
/// would discard the very content the search was asked to retrieve.
pub(crate) fn first_matched_search_content(chunks: &[&str]) -> Option<String> {
    chunks.first().map(|first| (*first).to_string())
}

// ── FtsSearchTool ─────────────────────────────────────────────────────────────

/// Arguments for [`FtsSearchTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FtsSearchArgs {
    /// Keywords or phrase to search for. Natural language is fine — punctuation
    /// is automatically stripped before the FTS5 query is executed.
    pub query: String,
    /// Maximum number of nodes to return. Defaults to 5.
    pub limit: Option<usize>,
}

/// Rig tool: full-text keyword search over the knowledge graph (SQLite FTS5).
///
/// Fast, exact keyword matching. Good for specific names, terms, and phrases.
/// Results are grouped by node and returned with matching text snippets.
#[derive(Clone)]
pub struct FtsSearchTool {
    graph: Arc<KnowledgeGraph>,
}

impl FtsSearchTool {
    pub fn new(graph: Arc<KnowledgeGraph>) -> Self {
        Self { graph }
    }
}

impl Tool for FtsSearchTool {
    const NAME: &'static str = validation::FTS_NAME;

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        validation::find(Self::NAME)
            .expect("FTS tool is present in the catalog")
            .description
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        validation::find(Self::NAME)
            .expect("FTS tool is present in the catalog")
            .parameters()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let args: FtsSearchArgs = validation::decode(Self::NAME, raw)?;
        let limit = args.limit.unwrap_or(5);
        let sanitized = fts5_sanitize(&args.query).ok_or_else(|| {
            ToolError("Query contains no searchable terms after removing punctuation.".to_string())
        })?;

        // Retrieve more chunks than nodes wanted so groups fill up meaningfully.
        let chunks = self
            .graph
            .search_chunks_fts(&sanitized, limit * 4)
            .map_err(|e| ToolError(format!("FTS search failed: {e:#}")))?;

        // Group chunks by node, preserving FTS5 relevance order (first appearance = best rank).
        let mut node_order: Vec<ObjectId> = Vec::new();
        let mut node_chunks: HashMap<ObjectId, Vec<String>> = HashMap::new();
        for (_chunk_id, obj_id, content) in chunks {
            if !node_chunks.contains_key(&obj_id) {
                node_order.push(obj_id);
            }
            node_chunks.entry(obj_id).or_default().push(content);
        }

        if node_order.is_empty() {
            return Ok(format!(
                "FTS search found no results for \"{}\". Try different keywords.",
                args.query
            ));
        }

        let mut output = format!(
            "FTS search results for \"{}\" ({} nodes):\n\n",
            args.query,
            node_order.len().min(limit)
        );

        for (i, obj_id) in node_order.into_iter().take(limit).enumerate() {
            let chunks = node_chunks.remove(&obj_id).unwrap_or_default();
            match self
                .graph
                .get_object(obj_id)
                .map_err(|e| ToolError(format!("Node hydration failed: {e:#}")))?
            {
                Some(meta) => {
                    output.push_str(&format!(
                        "[{}] {} ({}) [id: {}]\n",
                        i + 1,
                        meta.name,
                        meta.object_type,
                        obj_id
                    ));
                    let chunk_refs = chunks.iter().map(String::as_str).collect::<Vec<_>>();
                    if let Some(summary) = first_matched_search_content(&chunk_refs) {
                        output.push_str(&format!("  Matched content: {summary}\n"));
                    }
                    output.push('\n');
                }
                None => continue,
            }
        }

        Ok(output)
    }
}

// ── SemanticSearchTool ────────────────────────────────────────────────────────

/// Arguments for [`SemanticSearchTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticSearchArgs {
    /// Natural-language query. The query is embedded and used for
    /// approximate nearest-neighbour search over stored chunk vectors.
    pub query: String,
    /// Maximum number of nodes to return. Defaults to 5.
    pub limit: Option<usize>,
}

/// Rig tool: embedding-based semantic search over the knowledge graph.
///
/// Embeds the query then runs ANN search over stored chunk vectors.
/// Finds conceptually related content even when keywords don't match.
/// Requires an embedding-capable [`InferenceQueue`].
#[derive(Clone)]
pub struct SemanticSearchTool {
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    cancellation: CancellationToken,
}

impl SemanticSearchTool {
    pub fn new(graph: Arc<KnowledgeGraph>, queue: Arc<InferenceQueue>) -> Self {
        Self {
            graph,
            queue,
            hq_queue: None,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn with_hq_queue(mut self, hq_queue: Option<Arc<InferenceQueue>>) -> Self {
        self.hq_queue = hq_queue;
        self
    }

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

impl Tool for SemanticSearchTool {
    const NAME: &'static str = validation::SEMANTIC_NAME;

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        validation::find(Self::NAME)
            .expect("semantic tool is present in the catalog")
            .description
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        validation::find(Self::NAME)
            .expect("semantic tool is present in the catalog")
            .parameters()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let args: SemanticSearchArgs = validation::decode(Self::NAME, raw)?;
        let config = HybridSearchConfig {
            alpha: 1.0,
            semantic_limit: args.limit.unwrap_or(5).saturating_mul(4),
            rerank: false,
            limit: args.limit.unwrap_or(5),
            ..HybridSearchConfig::default()
        };
        execute_ranked_search(
            "Semantic",
            Self::NAME,
            &self.graph,
            &self.queue,
            self.hq_queue.as_deref(),
            &args.query,
            &config,
            self.cancellation.clone(),
        )
        .await
    }
}

// ── HybridSearchTool ──────────────────────────────────────────────────────────

/// Arguments for [`HybridSearchTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HybridSearchArgs {
    /// Natural-language query. Searched via both FTS5 keyword matching and
    /// semantic embedding ANN, then results are merged and optionally reranked.
    pub query: String,
    /// Maximum number of nodes to return. Defaults to 3.
    pub limit: Option<usize>,
    /// Blend between FTS5 (0.0) and semantic search (1.0). Defaults to 0.5.
    /// Use 0.0 for pure keyword search, 1.0 for pure semantic search.
    pub alpha: Option<f32>,
    /// Whether to apply cross-encoder reranking. Defaults to true when a
    /// reranker is available; silently skipped when none is registered.
    pub rerank: Option<bool>,
}

/// Rig tool: hybrid search combining FTS5, semantic ANN, and optional reranking.
///
/// Returns fully hydrated node results including description, tags,
/// relationships, and content. Best general-purpose search tool.
#[derive(Clone)]
pub struct HybridSearchTool {
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    cancellation: CancellationToken,
}

impl HybridSearchTool {
    pub fn new(graph: Arc<KnowledgeGraph>, queue: Arc<InferenceQueue>) -> Self {
        Self {
            graph,
            queue,
            hq_queue: None,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn with_hq_queue(mut self, hq_queue: Option<Arc<InferenceQueue>>) -> Self {
        self.hq_queue = hq_queue;
        self
    }

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

impl Tool for HybridSearchTool {
    const NAME: &'static str = validation::HYBRID_NAME;

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        validation::find(Self::NAME)
            .expect("hybrid tool is present in the catalog")
            .description
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        validation::find(Self::NAME)
            .expect("hybrid tool is present in the catalog")
            .parameters()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let args: HybridSearchArgs = validation::decode(Self::NAME, raw)?;
        let config = HybridSearchConfig {
            limit: args.limit.unwrap_or(3),
            alpha: args.alpha.unwrap_or(0.5).clamp(0.0, 1.0),
            rerank: args.rerank.unwrap_or(true),
            ..HybridSearchConfig::default()
        };

        execute_ranked_search(
            "Hybrid",
            Self::NAME,
            &self.graph,
            &self.queue,
            self.hq_queue.as_deref(),
            &args.query,
            &config,
            self.cancellation.clone(),
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_ranked_search(
    label: &str,
    tool_name: &'static str,
    graph: &KnowledgeGraph,
    queue: &InferenceQueue,
    hq_queue: Option<&InferenceQueue>,
    query: &str,
    config: &HybridSearchConfig,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    let response = search_hybrid_response_with_cancellation(
        graph,
        queue,
        hq_queue,
        query,
        config,
        cancellation,
    )
    .await
    .map_err(|error| {
        tracing::error!(tool = tool_name, %error, "ranked search tool failed");
        ToolError(format!("{label} search failed: {error:#}"))
    })?;
    format_search_response(label, query, response.results, &response.outcomes)
}

fn search_stage_diagnostics(outcomes: &SearchStageOutcomes) -> Vec<String> {
    [
        ("FTS", &outcomes.fts),
        ("standard semantic", &outcomes.standard_semantic),
        ("HQ semantic", &outcomes.high_quality_semantic),
        ("reranking", &outcomes.reranking),
    ]
    .into_iter()
    .filter(|(_, outcome)| {
        matches!(
            outcome.status,
            SearchStageStatus::Unavailable | SearchStageStatus::Failed
        )
    })
    .map(|(label, outcome)| {
        format!(
            "{label}: {}",
            outcome
                .diagnostic
                .as_deref()
                .unwrap_or("could not complete")
        )
    })
    .collect()
}

pub(crate) fn format_search_response(
    label: &str,
    query: &str,
    results: Vec<NodeSearchResult>,
    outcomes: &SearchStageOutcomes,
) -> Result<String, ToolError> {
    let diagnostics = search_stage_diagnostics(outcomes);
    let semantic_applied = outcomes.standard_semantic.status == SearchStageStatus::Applied
        || outcomes.high_quality_semantic.status == SearchStageStatus::Applied;
    if results.is_empty() {
        let mut output = if label == "Semantic" && !semantic_applied {
            format!("Semantic search is unavailable for \"{query}\".")
        } else {
            format!("{label} search found no results for \"{query}\".")
        };
        if !diagnostics.is_empty() {
            output.push_str(" Reasons: ");
            output.push_str(&diagnostics.join("; "));
            output.push('.');
        }
        if label == "Semantic" && !semantic_applied {
            output.push_str(
                " Try keyword search. If the embedding space is incompatible or unidentified, rebuild the semantic index from Settings.",
            );
        } else {
            output.push_str(" Try rephrasing the query or verify that graph content is indexed.");
        }
        return Ok(output);
    }

    let mut output = format!(
        "{label} search results for \"{query}\" ({} nodes):\n\n",
        results.len()
    );
    for (index, result) in results.iter().enumerate() {
        output.push_str(&format_node_result(result, index));
        output.push('\n');
    }
    if !diagnostics.is_empty() {
        output.push_str("Search completed with reduced capabilities: ");
        output.push_str(&diagnostics.join("; "));
        output.push('\n');
    }
    Ok(output)
}
