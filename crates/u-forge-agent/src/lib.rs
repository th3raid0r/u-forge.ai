//! Rig-based agent tools for the u-forge knowledge graph.
//!
//! Exposes three search tools and two write tools that can be registered with a [`rig`] agent:
//! - [`FtsSearchTool`] — SQLite FTS5 keyword search.
//! - [`SemanticSearchTool`] — Embedding-based approximate nearest-neighbour search.
//! - [`HybridSearchTool`] — Combined FTS5 + semantic + optional reranking.
//! - [`UpsertNodeTool`] — Create or update a node in the knowledge graph.
//! - [`UpsertEdgeTool`] — Create or update an edge (relationship) between two nodes.
//!
//! Each tool holds a shared [`KnowledgeGraph`] handle (and [`InferenceQueue`]
//! where inference is required) and formats results as human-readable text
//! suited for LLM consumption.

use std::collections::HashMap;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use u_forge_core::ingest::rechunk_and_embed_with_cancellation;
use u_forge_core::search::{
    HybridSearchConfig, NodeSearchResult, SearchStageOutcomes, SearchStageStatus, fts5_sanitize,
    search_hybrid_response_with_cancellation,
};
use u_forge_core::types::ObjectMetadata;
use u_forge_core::{
    EffectiveAgentBudget, KnowledgeGraph,
    queue::{CancellationToken, InferenceQueue},
    types::ObjectId,
};

mod budget;
pub use budget::{
    BoundedSchemaSummary, SchemaPriorityContext, TokenEstimate, bounded_schema_summary,
    count_tokens, estimate_tokens,
};

// ── History and token counting ────────────────────────────────────────────────

/// A single prior conversation turn for context injection.
///
/// `role` is `"user"` or `"assistant"`.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// Return the subset of `history` that fits inside the available token budget.
///
/// The active model context is the only ceiling. If older messages do not fit,
/// they are replaced by an explicit notice that the visible messages are the
/// most recent portion of a longer conversation.
///
/// Messages are evaluated newest-first; the returned `Vec` is in chronological
/// order (oldest first), ready to pass directly to `history()`.
pub fn select_history_window(
    history: &[HistoryMessage],
    system_prompt: &str,
    current_msg: &str,
    max_context_tokens: usize,
) -> Vec<HistoryMessage> {
    budget::select_history_window(history, system_prompt, current_msg, 0, max_context_tokens)
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Error returned by all agent tools (search and write).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(String);

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        Self(format!("{e:#}"))
    }
}

// ── Tool argument validation ───────────────────────────────────────────────────

pub(crate) mod tool_validation {
    use schemars::schema_for;
    use std::sync::LazyLock;

    use super::{
        FtsSearchArgs, HybridSearchArgs, SemanticSearchArgs, ToolError, UpsertEdgeArgs,
        UpsertNodeArgs,
    };

    // Schema values are compiled once and cached for the lifetime of the process.
    // Validators reference these statics so they don't borrow from local values.
    static FTS_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
        serde_json::to_value(schema_for!(FtsSearchArgs))
            .expect("FtsSearchArgs schema is valid JSON")
    });
    static FTS_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        jsonschema::validator_for(&FTS_SCHEMA).expect("FtsSearchArgs validator compiles")
    });

    static SEMANTIC_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
        serde_json::to_value(schema_for!(SemanticSearchArgs))
            .expect("SemanticSearchArgs schema is valid JSON")
    });
    static SEMANTIC_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        jsonschema::validator_for(&SEMANTIC_SCHEMA).expect("SemanticSearchArgs validator compiles")
    });

    static HYBRID_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
        serde_json::to_value(schema_for!(HybridSearchArgs))
            .expect("HybridSearchArgs schema is valid JSON")
    });
    static HYBRID_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        jsonschema::validator_for(&HYBRID_SCHEMA).expect("HybridSearchArgs validator compiles")
    });

    static UPSERT_NODE_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
        serde_json::to_value(schema_for!(UpsertNodeArgs))
            .expect("UpsertNodeArgs schema is valid JSON")
    });
    static UPSERT_NODE_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        jsonschema::validator_for(&UPSERT_NODE_SCHEMA).expect("UpsertNodeArgs validator compiles")
    });

    static UPSERT_EDGE_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
        serde_json::to_value(schema_for!(UpsertEdgeArgs))
            .expect("UpsertEdgeArgs schema is valid JSON")
    });
    static UPSERT_EDGE_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        jsonschema::validator_for(&UPSERT_EDGE_SCHEMA).expect("UpsertEdgeArgs validator compiles")
    });

    /// Validate `raw` JSON args against the tool's declared JSON Schema.
    ///
    /// Returns `Ok(())` when valid. On failure, returns a `ToolError` whose
    /// message names up to three offending field paths so the LLM can self-correct.
    pub(crate) fn validate_tool_args(
        tool_name: &'static str,
        raw: &serde_json::Value,
    ) -> Result<(), ToolError> {
        let validator = match tool_name {
            "search_fts" => &*FTS_VALIDATOR,
            "search_semantic" => &*SEMANTIC_VALIDATOR,
            "search_hybrid" => &*HYBRID_VALIDATOR,
            "upsert_node" => &*UPSERT_NODE_VALIDATOR,
            "upsert_edge" => &*UPSERT_EDGE_VALIDATOR,
            other => {
                return Err(ToolError(format!(
                    "no validator registered for tool '{other}'"
                )));
            }
        };

        if validator.is_valid(raw) {
            return Ok(());
        }

        let formatted: String = validator
            .iter_errors(raw)
            .take(3)
            .map(|err| format!("{} — {}", err.instance_path(), err))
            .collect::<Vec<_>>()
            .join("; ");

        tracing::warn!(tool = tool_name, errors = %formatted, "tool arg validation failed");

        Err(ToolError(format!(
            "Tool args invalid for {tool_name}: {formatted}"
        )))
    }
}

// ── Shared output formatter ───────────────────────────────────────────────────

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
fn first_matched_search_content(chunks: &[&str]) -> Option<String> {
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
    const NAME: &'static str = "search_fts";

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        "Full-text keyword search over the knowledge graph using SQLite FTS5. \
                 Fast and exact — good for specific names, terms, or known phrases. \
                 Returns nodes that contain matching text, with the matching snippets."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(FtsSearchArgs))
            .expect("FtsSearchArgs schema is always valid JSON")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tool_validation::validate_tool_args(Self::NAME, &raw)?;
        let args: FtsSearchArgs = serde_json::from_value(raw).map_err(|e| {
            ToolError(format!(
                "deserialization failed after validation (bug): {e}"
            ))
        })?;
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

    fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

impl Tool for SemanticSearchTool {
    const NAME: &'static str = "search_semantic";

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        "Semantic (embedding-based) search over the knowledge graph. \
                 Finds conceptually related nodes even when exact keywords don't match. \
                 Use for exploratory queries, related concepts, or when FTS returns nothing."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(SemanticSearchArgs))
            .expect("SemanticSearchArgs schema is always valid JSON")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tool_validation::validate_tool_args(Self::NAME, &raw)?;
        let args: SemanticSearchArgs = serde_json::from_value(raw).map_err(|e| {
            ToolError(format!(
                "deserialization failed after validation (bug): {e}"
            ))
        })?;
        let config = HybridSearchConfig {
            alpha: 1.0,
            semantic_limit: args.limit.unwrap_or(5).saturating_mul(4),
            rerank: false,
            limit: args.limit.unwrap_or(5),
            ..HybridSearchConfig::default()
        };
        let response = search_hybrid_response_with_cancellation(
            &self.graph,
            &self.queue,
            self.hq_queue.as_deref(),
            &args.query,
            &config,
            self.cancellation.clone(),
        )
        .await
        .map_err(|error| {
            tracing::error!(tool = Self::NAME, error = %error, "semantic search tool failed");
            ToolError(format!("Semantic search failed: {error:#}"))
        })?;

        format_search_response(
            "Semantic",
            &args.query,
            response.results,
            &response.outcomes,
        )
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

    fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

impl Tool for HybridSearchTool {
    const NAME: &'static str = "search_hybrid";

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        "Hybrid search over the knowledge graph: combines FTS5 keyword matching \
                 with semantic embedding search using Reciprocal Rank Fusion, then \
                 optionally reranks results with a cross-encoder. Returns fully hydrated \
                 node results with metadata, relationships, and content. \
                 Recommended as the default search tool."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(HybridSearchArgs))
            .expect("HybridSearchArgs schema is always valid JSON")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tool_validation::validate_tool_args(Self::NAME, &raw)?;
        let args: HybridSearchArgs = serde_json::from_value(raw).map_err(|e| {
            ToolError(format!(
                "deserialization failed after validation (bug): {e}"
            ))
        })?;
        let config = HybridSearchConfig {
            limit: args.limit.unwrap_or(3),
            alpha: args.alpha.unwrap_or(0.5).clamp(0.0, 1.0),
            rerank: args.rerank.unwrap_or(true),
            ..HybridSearchConfig::default()
        };

        let response = search_hybrid_response_with_cancellation(
            &self.graph,
            &self.queue,
            self.hq_queue.as_deref(),
            &args.query,
            &config,
            self.cancellation.clone(),
        )
        .await
        .map_err(ToolError::from)?;

        format_search_response("Hybrid", &args.query, response.results, &response.outcomes)
    }
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

fn format_search_response(
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

// ── UpsertNodeTool ────────────────────────────────────────────────────────────

/// Arguments for [`UpsertNodeTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertNodeArgs {
    /// UUID of an existing node to update. Omit when creating.
    pub node_id: Option<String>,
    /// Display name, e.g. "Gandalf" or "Neverwinter".
    pub name: String,
    /// Must be a type from the schema (see system prompt). Examples: "character", "location", "faction", "item", "event", "session".
    pub object_type: String,
    /// Flat JSON object of properties for this node type — see the system prompt for valid keys per object_type.
    /// Example for a character: {"description": "An ancient wizard", "tags": ["wizard", "NPC"], "species": "Human", "age": "342"}.
    /// On update: omitted/null keys are kept, "" deletes a key.
    pub properties: Option<serde_json::Value>,
}

/// Rig tool: create or update a node in the knowledge graph.
///
/// When `node_id` is provided the existing node is updated in place;
/// otherwise a brand-new node is created. After the DB write the tool
/// re-chunks the node and computes embeddings (standard + HQ when
/// available) before returning, so the node is immediately searchable.
#[derive(Clone)]
pub struct UpsertNodeTool {
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    cancellation: CancellationToken,
}

impl UpsertNodeTool {
    pub fn new(
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
    ) -> Self {
        Self {
            graph,
            queue,
            hq_queue,
            cancellation: CancellationToken::new(),
        }
    }

    fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

fn preflight_node_properties(
    graph: &KnowledgeGraph,
    metadata: &mut ObjectMetadata,
) -> Result<(), ToolError> {
    let schema_manager = graph.get_schema_manager();
    if !schema_manager.is_valid_object_type(&metadata.object_type) {
        let valid = schema_manager.all_object_type_names().join(", ");
        return Err(ToolError(format!(
            "Unknown object_type \"{}\". Valid types: {valid}",
            metadata.object_type
        )));
    }

    let property_issues = if let serde_json::Value::Object(properties) = &mut metadata.properties {
        graph.validate_and_coerce_properties(&metadata.object_type, properties)
    } else {
        vec![]
    };
    if property_issues.is_empty() {
        return Ok(());
    }

    let details = property_issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    Err(ToolError(format!(
        "Node properties do not satisfy the loaded schema: {details}"
    )))
}

impl Tool for UpsertNodeTool {
    const NAME: &'static str = "upsert_node";

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        "Create or update a knowledge graph node. Always search first to avoid duplicates. \
                 Populate name, object_type, and all known properties in one call. \
                 On update (node_id set), only changed keys are needed — omitted keys are kept."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(UpsertNodeArgs))
            .expect("UpsertNodeArgs schema is always valid JSON")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tool_validation::validate_tool_args(Self::NAME, &raw)?;
        let args: UpsertNodeArgs = serde_json::from_value(raw).map_err(|e| {
            ToolError(format!(
                "deserialization failed after validation (bug): {e}"
            ))
        })?;
        // Single DB read: verify existence and load metadata in one step.
        let (object_id, is_update, mut meta) = if let Some(ref id_str) = args.node_id {
            let oid = ObjectId::parse_str(id_str)
                .map_err(|e| ToolError(format!("Invalid node_id UUID: {e}")))?;
            let existing = self
                .graph
                .get_object(oid)
                .map_err(|e| ToolError(format!("Failed to look up node: {e:#}")))?
                .ok_or_else(|| ToolError(format!("Node {id_str} not found")))?;
            (oid, true, existing)
        } else {
            let oid = ObjectId::new_v4();
            let mut new_meta = ObjectMetadata::new(args.object_type.clone(), args.name.clone());
            new_meta.id = oid;
            (oid, false, new_meta)
        };

        // Apply caller-provided fields.
        meta.name = args.name;
        meta.object_type = args.object_type;
        if let Some(props) = args.properties
            && let (serde_json::Value::Object(incoming), serde_json::Value::Object(existing)) =
                (props, &mut meta.properties)
        {
            // Merge: caller-supplied keys win; null/omitted keys are preserved.
            // An empty string removes the key.
            for (k, v) in incoming {
                if v.is_null() {
                    // null means "keep existing" — skip.
                    continue;
                } else if v == serde_json::Value::String(String::new()) {
                    existing.remove(&k);
                } else {
                    existing.insert(k, v);
                }
            }
        }

        // Validate and normalize before persistence so the tool can return
        // actionable nested/rule diagnostics without relying on the final guard.
        preflight_node_properties(&self.graph, &mut meta)?;

        // Persist the node.
        if self.cancellation.is_cancelled() {
            return Err(ToolError(self.cancellation.error().to_string()));
        }
        if is_update {
            self.graph
                .update_object(meta.clone())
                .map_err(|e| ToolError(format!("Failed to update node: {e:#}")))?;
        } else {
            self.graph
                .add_object(meta.clone())
                .map_err(|e| ToolError(format!("Failed to create node: {e:#}")))?;
        }

        // Re-chunk and embed (standard + HQ). This blocks until all embeddings are stored.
        let hq_ref = self.hq_queue.as_deref();
        let chunks = rechunk_and_embed_with_cancellation(
            &self.graph,
            &self.queue,
            hq_ref,
            object_id,
            self.cancellation.clone(),
        )
        .await
        .map_err(|e| ToolError(format!("Embedding failed: {e:#}")))?;

        let action = if is_update { "Updated" } else { "Created" };
        let output = format!(
            "{action} node \"{name}\" ({object_type}). node_id: {object_id}. chunks_embedded: {chunks}.",
            name = meta.name,
            object_type = meta.object_type,
        );
        Ok(output)
    }
}

// ── UpsertEdgeTool ────────────────────────────────────────────────────────────

/// Arguments for [`UpsertEdgeTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertEdgeArgs {
    /// Exact name or UUID of the source node.
    pub source: String,
    /// Exact name or UUID of the target node.
    pub target: String,
    /// Freeform relationship label, e.g. "led_by", "located_in", "member_of".
    pub edge_type: String,
    /// Optional weight (0.0–1.0). Defaults to 1.0.
    pub weight: Option<f32>,
}

/// Rig tool: create or update an edge (relationship) between two nodes.
///
/// Nodes can be referenced by UUID or by exact name. After the edge is
/// persisted, both endpoint nodes are re-chunked and re-embedded so that
/// the new relationship is reflected in semantic search results.
#[derive(Clone)]
pub struct UpsertEdgeTool {
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    cancellation: CancellationToken,
}

impl UpsertEdgeTool {
    pub fn new(
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
    ) -> Self {
        Self {
            graph,
            queue,
            hq_queue,
            cancellation: CancellationToken::new(),
        }
    }

    fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Try to parse `input` as a UUID; if that fails, do an exact name lookup.
fn resolve_node(graph: &KnowledgeGraph, input: &str) -> Result<ObjectId, ToolError> {
    // Try UUID first.
    if let Ok(oid) = ObjectId::parse_str(input)
        && graph.get_object(oid).ok().flatten().is_some()
    {
        return Ok(oid);
    }
    // Fall back to name lookup.
    let matches = graph
        .find_by_name_only(input)
        .map_err(|e| ToolError(format!("Name lookup failed: {e:#}")))?;
    match matches.len() {
        0 => Err(ToolError(format!(
            "No node found matching \"{input}\". Check the name or provide a UUID."
        ))),
        1 => Ok(matches[0].id),
        n => {
            let mut grouped = std::collections::BTreeMap::<&str, Vec<&ObjectMetadata>>::new();
            for candidate in &matches {
                grouped
                    .entry(candidate.object_type.as_str())
                    .or_default()
                    .push(candidate);
            }
            let mut lines = Vec::new();
            for (object_type, mut candidates) in grouped {
                candidates
                    .sort_by_key(|candidate| (candidate.name.as_str(), candidate.id.to_string()));
                lines.push(format!("  {object_type} ({}):", candidates.len()));
                lines.extend(
                    candidates
                        .into_iter()
                        .map(|candidate| format!("    • {} [{}]", candidate.name, candidate.id)),
                );
            }
            Err(ToolError(format!(
                "\"{input}\" matched {n} nodes, grouped by object type:\n{}\nProvide one complete UUID in the source or target field to disambiguate.",
                lines.join("\n")
            )))
        }
    }
}

impl Tool for UpsertEdgeTool {
    const NAME: &'static str = "upsert_edge";

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        "Create or update a relationship (edge) between two nodes in the knowledge graph. \
                 Nodes can be specified by exact name or UUID. \
                 Both endpoint nodes are re-indexed after the edge is saved."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(UpsertEdgeArgs))
            .expect("UpsertEdgeArgs schema is always valid JSON")
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tool_validation::validate_tool_args(Self::NAME, &raw)?;
        let args: UpsertEdgeArgs = serde_json::from_value(raw).map_err(|e| {
            ToolError(format!(
                "deserialization failed after validation (bug): {e}"
            ))
        })?;
        let source_id = resolve_node(&self.graph, &args.source)?;
        let target_id = resolve_node(&self.graph, &args.target)?;

        let weight = args.weight.unwrap_or(1.0);
        if self.cancellation.is_cancelled() {
            return Err(ToolError(self.cancellation.error().to_string()));
        }
        self.graph
            .connect_objects_weighted_str(source_id, target_id, &args.edge_type, weight)
            .map_err(|e| ToolError(format!("Failed to upsert edge: {e:#}")))?;

        // Re-embed both endpoints so the new relationship appears in semantic search.
        // Deduplicate when source == target (self-loop) to avoid embedding the same node twice.
        // Collect failures so the LLM sees partial success in the tool Output rather than
        // believing the edge is fully indexed.
        let hq_ref = self.hq_queue.as_deref();
        let mut to_reembed = vec![source_id];
        if target_id != source_id {
            to_reembed.push(target_id);
        }
        let mut reembed_warnings: Vec<String> = Vec::new();
        for &oid in &to_reembed {
            if let Err(e) = rechunk_and_embed_with_cancellation(
                &self.graph,
                &self.queue,
                hq_ref,
                oid,
                self.cancellation.clone(),
            )
            .await
            {
                tracing::warn!(object_id = %oid, %e, "Re-embed after edge upsert failed");
                reembed_warnings.push(format!("[warning] endpoint {oid} re-embed failed: {e:#}"));
            }
        }

        // Resolve names for the confirmation message.
        let source_name = self
            .graph
            .get_object(source_id)
            .ok()
            .flatten()
            .map(|m| m.name)
            .unwrap_or_else(|| source_id.to_string());
        let target_name = self
            .graph
            .get_object(target_id)
            .ok()
            .flatten()
            .map(|m| m.name)
            .unwrap_or_else(|| target_id.to_string());

        let mut output = format!(
            "Edge created: {source_name} -[{}]-> {target_name} (weight: {weight:.2})",
            args.edge_type,
        );
        for w in &reembed_warnings {
            output.push('\n');
            output.push_str(w);
        }
        Ok(output)
    }
}

// ── GraphAgent ────────────────────────────────────────────────────────────────

use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::client::AgentClientExt;
use rig::completion::{Prompt, PromptError, message::ToolResultContent};
use rig::providers::openai::CompletionsClient;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use tokio::sync::mpsc;

// ── Stream event type ─────────────────────────────────────────────────────────

/// Compatibility name for the shared event contract consumed above direct
/// HTTP and Rig adapters.
pub type AgentStreamEvent = u_forge_core::lemonade::ChatEvent;

/// Sampling and generation parameters for the agent, built from
/// [`ChatDeviceConfig`] + [`ChatConfig`].
#[derive(Debug, Clone)]
pub struct AgentParams {
    /// Sampling temperature. `None` defers to the server/model default.
    pub temperature: Option<f64>,
    /// Maximum generation tokens.
    pub max_tokens: Option<u64>,
    /// Nucleus sampling threshold (0.0–1.0).
    pub top_p: Option<f64>,
    /// Top-k sampling (llama.cpp).
    pub top_k: Option<u32>,
    /// Min-p sampling (llama.cpp).
    pub min_p: Option<f64>,
    /// Penalise repeated tokens by frequency (-2.0–2.0).
    pub frequency_penalty: Option<f64>,
    /// Penalise tokens that appeared at all (-2.0–2.0).
    pub presence_penalty: Option<f64>,
    /// Repetition penalty (llama.cpp, typically 1.0–1.5).
    pub repetition_penalty: Option<f64>,
    /// RNG seed for reproducible generation.
    pub seed: Option<u64>,
    /// Stop sequences.
    pub stop: Option<Vec<String>>,
    /// Maximum tool-call round-trips per user message.
    pub max_tool_turns: usize,
    /// Active-model-safe context, cumulative, output, and repeat limits.
    pub budget: EffectiveAgentBudget,
}

impl Default for AgentParams {
    fn default() -> Self {
        Self {
            temperature: None,
            max_tokens: None,
            top_p: None,
            top_k: None,
            min_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            repetition_penalty: None,
            seed: None,
            stop: None,
            max_tool_turns: 5,
            budget: EffectiveAgentBudget::default(),
        }
    }
}

/// A configured agent backed by the three graph search tools.
///
/// Wraps a rig `CompletionsClient` pointed at Lemonade's OpenAI-compatible
/// endpoint. Each call to [`GraphAgent::prompt`] builds a fresh rig agent,
/// runs the multi-turn tool loop (search ↔ LLM), and returns the final text.
///
/// `Clone` is cheap — the inner client and Arc handles are reference-counted.
#[derive(Clone)]
pub struct GraphAgent {
    client: CompletionsClient,
    graph: Arc<KnowledgeGraph>,
    queue: Arc<InferenceQueue>,
    hq_queue: Option<Arc<InferenceQueue>>,
    base_prompt: String,
    tool_guidance: String,
    schema: Option<u_forge_core::SchemaDefinition>,
    tool_definition_tokens: TokenEstimate,
    params: AgentParams,
    gpu: Option<Arc<u_forge_core::GpuResourceManager>>,
}

impl GraphAgent {
    /// Build a `GraphAgent` connected to the given Lemonade base URL,
    /// e.g. `http://localhost:13305/api/v1`.
    pub fn new(
        lemonade_url: &str,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
        params: AgentParams,
    ) -> anyhow::Result<Self> {
        let connection = Arc::new(u_forge_core::lemonade::LemonadeConnection::external(
            lemonade_url,
        )?);
        Self::new_with_connection(connection, graph, queue, hq_queue, system_prompt, params)
    }

    pub fn new_with_connection(
        connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
        params: AgentParams,
    ) -> anyhow::Result<Self> {
        Self::new_with_connection_and_gpu(
            connection,
            graph,
            queue,
            hq_queue,
            system_prompt,
            params,
            None,
        )
    }

    pub fn new_with_connection_and_gpu(
        connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        hq_queue: Option<Arc<InferenceQueue>>,
        system_prompt: impl Into<String>,
        params: AgentParams,
        gpu: Option<Arc<u_forge_core::GpuResourceManager>>,
    ) -> anyhow::Result<Self> {
        let client = CompletionsClient::builder()
            .api_key(connection.api_credential().unwrap_or("lemonade"))
            .base_url(connection.api_base())
            .http_client(connection.completion_http_client())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build rig client: {e}"))?;
        let base_prompt: String = system_prompt.into();
        let schema = graph.merged_schema_definition()?;

        let tool_guidance = "\
## Tool-use guidelines

1. **Search before writing.** Before creating a node, search to check it doesn't already exist.
2. **One call per node.** Include name, object_type, and all known properties in a single \
   upsert_node call. Never create a blank node and fill properties afterwards.
3. **Refer to the schema below** for valid object_type values and their properties. \
   Use the property names and types exactly as listed.
4. **Stop when done.** After a successful tool call, report the result to the user. \
   Do not re-call a tool for the same node unless asked."
            .to_string();

        let definition = |name: &str, description: String, parameters: serde_json::Value| {
            serde_json::to_string(&serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
            }))
        };
        let tool_definitions = vec![
            definition(
                FtsSearchTool::NAME,
                FtsSearchTool::new(graph.clone()).description(),
                FtsSearchTool::new(graph.clone()).parameters(),
            )?,
            definition(
                SemanticSearchTool::NAME,
                SemanticSearchTool::new(graph.clone(), queue.clone())
                    .with_hq_queue(hq_queue.clone())
                    .description(),
                SemanticSearchTool::new(graph.clone(), queue.clone())
                    .with_hq_queue(hq_queue.clone())
                    .parameters(),
            )?,
            definition(
                HybridSearchTool::NAME,
                HybridSearchTool::new(graph.clone(), queue.clone())
                    .with_hq_queue(hq_queue.clone())
                    .description(),
                HybridSearchTool::new(graph.clone(), queue.clone())
                    .with_hq_queue(hq_queue.clone())
                    .parameters(),
            )?,
            definition(
                UpsertNodeTool::NAME,
                UpsertNodeTool::new(graph.clone(), queue.clone(), hq_queue.clone()).description(),
                UpsertNodeTool::new(graph.clone(), queue.clone(), hq_queue.clone()).parameters(),
            )?,
            definition(
                UpsertEdgeTool::NAME,
                UpsertEdgeTool::new(graph.clone(), queue.clone(), hq_queue.clone()).description(),
                UpsertEdgeTool::new(graph.clone(), queue.clone(), hq_queue.clone()).parameters(),
            )?,
        ];

        Ok(Self {
            client,
            graph,
            queue,
            hq_queue,
            base_prompt,
            tool_guidance,
            schema,
            tool_definition_tokens: budget::estimate_tool_definitions(&tool_definitions),
            params,
            gpu,
        })
    }

    /// Compute Rig's flattened `additional_params` JSON from sampling knobs.
    ///
    /// Rig's OpenAI provider flattens this into the request body, so keys like
    /// `frequency_penalty`, `top_p`, `seed`, etc. end up as top-level fields
    /// in the OpenAI-compatible `/chat/completions` request.
    fn build_additional_params(params: &AgentParams) -> Option<serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(v) = params.top_p {
            map.insert("top_p".into(), serde_json::json!(v));
        }
        if let Some(v) = params.top_k {
            map.insert("top_k".into(), serde_json::json!(v));
        }
        if let Some(v) = params.min_p {
            map.insert("min_p".into(), serde_json::json!(v));
        }
        if let Some(v) = params.frequency_penalty {
            map.insert("frequency_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.presence_penalty {
            map.insert("presence_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.repetition_penalty {
            map.insert("repeat_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = params.seed {
            map.insert("seed".into(), serde_json::json!(v));
        }
        if let Some(ref v) = params.stop {
            map.insert("stop".into(), serde_json::json!(v));
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    }

    fn build_request_additional_params(
        params: &AgentParams,
        reasoning: u_forge_core::ReasoningPolicy,
    ) -> serde_json::Value {
        let mut value = Self::build_additional_params(params)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        if let (serde_json::Value::Object(values), Some(enabled)) =
            (&mut value, reasoning.request_hint())
        {
            values.insert("enable_thinking".into(), serde_json::json!(enabled));
        }
        value
    }

    fn prepare_budget(
        &self,
        user_message: &str,
        history: &[HistoryMessage],
        params: &AgentParams,
    ) -> (budget::BudgetController, Vec<HistoryMessage>) {
        let history_text = history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let controller = budget::BudgetController::new(
            params.budget.clone(),
            self.schema.clone(),
            self.base_prompt.clone(),
            self.tool_guidance.clone(),
            user_message.to_string(),
            history_text,
            self.tool_definition_tokens,
        );
        (controller, history.to_vec())
    }

    fn build_agent_with_params(
        &self,
        model_id: &str,
        reasoning: u_forge_core::ReasoningPolicy,
        params: &AgentParams,
        cancellation: CancellationToken,
        budget: budget::BudgetController,
    ) -> rig::agent::Agent<rig::providers::openai::CompletionModel> {
        let initial_preamble = budget.initial_preamble();
        let mut builder = self
            .client
            .agent(model_id)
            .preamble(&initial_preamble)
            .add_hook(budget);
        if let Some(temp) = params.temperature {
            builder = builder.temperature(temp);
        }

        if let Some(max_tokens) = params.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }
        let additional = Self::build_request_additional_params(params, reasoning);
        if !additional.as_object().is_none_or(serde_json::Map::is_empty) {
            builder = builder.additional_params(additional);
        }

        builder
            .tool(
                HybridSearchTool::new(self.graph.clone(), self.queue.clone())
                    .with_hq_queue(self.hq_queue.clone())
                    .with_cancellation(cancellation.clone()),
            )
            .tool(FtsSearchTool::new(self.graph.clone()))
            .tool(
                SemanticSearchTool::new(self.graph.clone(), self.queue.clone())
                    .with_hq_queue(self.hq_queue.clone())
                    .with_cancellation(cancellation.clone()),
            )
            .tool(
                UpsertNodeTool::new(
                    self.graph.clone(),
                    self.queue.clone(),
                    self.hq_queue.clone(),
                )
                .with_cancellation(cancellation.clone()),
            )
            .tool(
                UpsertEdgeTool::new(
                    self.graph.clone(),
                    self.queue.clone(),
                    self.hq_queue.clone(),
                )
                .with_cancellation(cancellation),
            )
            .build()
    }

    /// Run the agent loop with streaming output.
    ///
    /// Returns a [`mpsc::Receiver`] that yields [`AgentStreamEvent`]s as the
    /// agent streams text, calls tools, and receives tool results. The channel
    /// closes after a terminal `Finished` or `FatalError` event.
    pub async fn prompt_stream(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning_enabled: bool,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_params(
            model_id,
            user_message,
            history,
            reasoning_enabled,
            self.params.clone(),
        )
        .await
    }

    /// Stream using the complete effective profile for the selected model.
    ///
    /// This keeps picker changes coherent: model, context/generation limits,
    /// sampling controls, reasoning, and tool-loop ceiling change together.
    pub async fn prompt_stream_with_params(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning_enabled: bool,
        params: AgentParams,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_profile(
            model_id,
            user_message,
            history,
            if reasoning_enabled {
                u_forge_core::ReasoningPolicy::Enabled
            } else {
                u_forge_core::ReasoningPolicy::Disabled
            },
            params,
            None,
            false,
        )
        .await
    }

    /// Stream while retaining runtime and device coordination in the producer.
    #[allow(clippy::too_many_arguments)]
    pub async fn prompt_stream_with_profile(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning: u_forge_core::ReasoningPolicy,
        params: AgentParams,
        runtime_lease: Option<u_forge_core::LemonadeRuntimeLease>,
        uses_gpu: bool,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        self.prompt_stream_with_profile_and_cancellation(
            model_id,
            user_message,
            history,
            reasoning,
            params,
            runtime_lease,
            uses_gpu,
            CancellationToken::new(),
        )
        .await
    }

    /// Stream a complete agent/tool operation under one parent token.
    #[allow(clippy::too_many_arguments)]
    pub async fn prompt_stream_with_profile_and_cancellation(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
        reasoning: u_forge_core::ReasoningPolicy,
        params: AgentParams,
        runtime_lease: Option<u_forge_core::LemonadeRuntimeLease>,
        uses_gpu: bool,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        let (tx, rx) = mpsc::channel(64);

        let (budget, selected_history) = self.prepare_budget(user_message, history, &params);
        let agent = self.build_agent_with_params(
            model_id,
            reasoning,
            &params,
            cancellation.clone(),
            budget.clone(),
        );
        let max_turns = params.max_tool_turns;
        let gpu = uses_gpu.then(|| self.gpu.clone()).flatten();

        let user_message = user_message.to_string();
        // Convert HistoryMessage → rig::completion::message::Message.
        let rig_history: Vec<rig::completion::message::Message> = selected_history
            .iter()
            .map(|m| match m.role.as_str() {
                "assistant" => rig::completion::message::Message::assistant(&m.content),
                "system" => rig::completion::message::Message::system(&m.content),
                _ => rig::completion::message::Message::user(&m.content),
            })
            .collect();

        tokio::spawn(async move {
            let _runtime_lease = runtime_lease;
            let mut gpu_guard = match &gpu {
                Some(gpu) => tokio::select! {
                    _ = cancellation.cancelled() => return,
                    guard = gpu.begin_llm() => Some(guard),
                },
                None => None,
            };
            let mut stream = agent
                .stream_prompt(&user_message)
                .history(rig_history)
                .max_turns(max_turns)
                .await;

            let mut final_text = String::new();

            // `'stream` labels the outer loop so every send-on-error can break it directly.
            // If the receiver is dropped (user closed the chat panel, app exited) we stop
            // driving the rig stream instead of burning LLM inference and potentially
            // running write tools the caller will never observe.
            'stream: loop {
                let item = tokio::select! {
                    _ = cancellation.cancelled() => break 'stream,
                    item = stream.next() => item,
                };
                let Some(item) = item else { break 'stream };
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => match content {
                        StreamedAssistantContent::Text(t) => {
                            final_text.push_str(&t.text);
                            if tx.send(AgentStreamEvent::TextDelta(t.text)).await.is_err() {
                                break 'stream;
                            }
                        }
                        StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        } => {
                            // Rig executes the tool on the next poll. Release
                            // the device now so embedding tools cannot deadlock
                            // behind this LLM turn's GPU guard.
                            gpu_guard.take();
                            let args_display =
                                serde_json::to_string_pretty(&tool_call.function.arguments)
                                    .unwrap_or_else(|_| tool_call.function.arguments.to_string());
                            if tx
                                .send(AgentStreamEvent::ToolCallStart {
                                    internal_id: internal_call_id,
                                    name: tool_call.function.name,
                                    args_display,
                                })
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                            // FinalResponse is scoped to the post-tool turn.
                            // Keep the fallback buffer scoped the same way so
                            // terminal-only text never repeats pre-tool prose.
                            final_text.clear();
                        }
                        StreamedAssistantContent::Reasoning(r) => {
                            // Full reasoning block (some providers emit this instead of deltas).
                            for chunk in &r.content {
                                if let rig::completion::message::ReasoningContent::Text {
                                    text, ..
                                } = chunk
                                    && tx
                                        .send(AgentStreamEvent::ReasoningDelta(text.clone()))
                                        .await
                                        .is_err()
                                {
                                    break 'stream;
                                }
                            }
                        }
                        StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                            if tx
                                .send(AgentStreamEvent::ReasoningDelta(reasoning))
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                        }
                        // Final(R) and ToolCallDelta are ignored — text arrives via TextDelta.
                        _ => {}
                    },
                    Ok(MultiTurnStreamItem::StreamUserItem(content)) => match content {
                        StreamedUserContent::ToolResult {
                            tool_result,
                            internal_call_id,
                        } => {
                            let result_text = tool_result
                                .content
                                .iter()
                                .filter_map(|c| {
                                    if let ToolResultContent::Text(t) = c {
                                        Some(t.text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if tx
                                .send(AgentStreamEvent::ToolResult {
                                    internal_id: internal_call_id,
                                    content: result_text,
                                })
                                .await
                                .is_err()
                            {
                                break 'stream;
                            }
                            // The next poll begins the following LLM turn.
                            if let Some(gpu) = &gpu {
                                gpu_guard = tokio::select! {
                                    _ = cancellation.cancelled() => break 'stream,
                                    guard = gpu.begin_llm() => Some(guard),
                                };
                            }
                        }
                    },
                    Ok(MultiTurnStreamItem::FinalResponse(resp)) => {
                        // FinalResponse carries the full aggregated text for the
                        // last turn. Use it if we didn't accumulate via TextDelta.
                        let text = if final_text.is_empty() {
                            resp.output().to_string()
                        } else {
                            final_text.clone()
                        };
                        let usage = resp.usage();
                        if usage.total_tokens > 0
                            && tx
                                .send(AgentStreamEvent::Usage(u_forge_core::lemonade::ChatUsage {
                                    prompt_tokens: usage.input_tokens.min(u32::MAX as u64) as u32,
                                    completion_tokens: usage.output_tokens.min(u32::MAX as u64)
                                        as u32,
                                    total_tokens: usage.total_tokens.min(u32::MAX as u64) as u32,
                                }))
                                .await
                                .is_err()
                        {
                            break 'stream;
                        }
                        let diagnostics = budget.diagnostics();
                        tracing::info!(
                            model_calls = diagnostics.model_calls,
                            request_tokens = diagnostics.request_tokens,
                            assistant_output_tokens = diagnostics.assistant_output_tokens,
                            tool_argument_tokens = diagnostics.tool_argument_tokens,
                            tool_output_tokens = diagnostics.tool_output_tokens,
                            estimation_fallback = diagnostics.estimation_fallback,
                            "Agent request completed"
                        );
                        if tx
                            .send(AgentStreamEvent::AgentDiagnostics(diagnostics))
                            .await
                            .is_err()
                        {
                            break 'stream;
                        }
                        let _ = tx
                            .send(AgentStreamEvent::Finished {
                                reason: u_forge_core::lemonade::ChatTerminalReason::AgentComplete,
                                full_text: Some(text),
                            })
                            .await;
                        break 'stream;
                    }
                    Ok(_) => {
                        // Non-exhaustive: ignore any new MultiTurnStreamItem variants.
                    }
                    Err(e) => {
                        let diagnostics = budget.diagnostics();
                        match budget.termination() {
                            Some(budget::BudgetTermination::Budget(reason)) => {
                                tracing::warn!(
                                    %reason,
                                    model_calls = diagnostics.model_calls,
                                    request_tokens = diagnostics.request_tokens,
                                    tool_output_tokens = diagnostics.tool_output_tokens,
                                    estimation_fallback = diagnostics.estimation_fallback,
                                    "Agent request stopped by budget"
                                );
                                let _ = tx
                                    .send(AgentStreamEvent::BudgetTerminated {
                                        reason,
                                        diagnostics,
                                    })
                                    .await;
                            }
                            Some(budget::BudgetTermination::Repeat(reason)) => {
                                tracing::warn!(
                                    %reason,
                                    model_calls = diagnostics.model_calls,
                                    request_tokens = diagnostics.request_tokens,
                                    tool_output_tokens = diagnostics.tool_output_tokens,
                                    "Agent request stopped by repeat guard"
                                );
                                let _ = tx
                                    .send(AgentStreamEvent::RepeatTerminated {
                                        reason,
                                        diagnostics,
                                    })
                                    .await;
                            }
                            None => {
                                let _ = tx.send(AgentStreamEvent::FatalError(e.to_string())).await;
                            }
                        }
                        break 'stream;
                    }
                }
            }
        });

        rx
    }

    /// Run the agent tool loop for a single user message (non-streaming).
    ///
    /// Uses the same tools and sampling parameters as [`prompt_stream`].
    pub async fn prompt(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
    ) -> Result<String, String> {
        let (budget, selected_history) = self.prepare_budget(user_message, history, &self.params);
        let agent = self.build_agent_with_params(
            model_id,
            u_forge_core::ReasoningPolicy::Enabled,
            &self.params,
            CancellationToken::new(),
            budget.clone(),
        );
        let rig_history: Vec<rig::completion::message::Message> = selected_history
            .iter()
            .map(|m| match m.role.as_str() {
                "assistant" => rig::completion::message::Message::assistant(&m.content),
                "system" => rig::completion::message::Message::system(&m.content),
                _ => rig::completion::message::Message::user(&m.content),
            })
            .collect();
        agent
            .prompt(user_message)
            .history(rig_history)
            .max_turns(self.params.max_tool_turns)
            .await
            .map_err(|e: PromptError| match budget.termination() {
                Some(budget::BudgetTermination::Budget(reason))
                | Some(budget::BudgetTermination::Repeat(reason)) => reason,
                None => e.to_string(),
            })
    }
}

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use rig;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::tool_validation::validate_tool_args;
    use super::{
        AgentParams, GraphAgent, first_matched_search_content, format_search_response,
        preflight_node_properties, resolve_node,
    };
    use serde_json::json;
    use tempfile::TempDir;
    use u_forge_core::schema::ValidationRule;
    use u_forge_core::search::{SearchStageOutcome, SearchStageOutcomes, SearchStageStatus};
    use u_forge_core::{
        KnowledgeGraph, ObjectMetadata, ObjectTypeSchema, PropertySchema, SchemaDefinition,
    };

    fn stage(status: SearchStageStatus, diagnostic: Option<&str>) -> SearchStageOutcome {
        SearchStageOutcome {
            status,
            diagnostic: diagnostic.map(str::to_string),
        }
    }

    #[test]
    fn semantic_search_failure_explains_unavailable_lanes_and_recovery() {
        let outcomes = SearchStageOutcomes {
            fts: stage(SearchStageStatus::IntentionallySkipped, None),
            standard_semantic: stage(
                SearchStageStatus::Failed,
                Some("embedding provider rejected the request"),
            ),
            high_quality_semantic: stage(
                SearchStageStatus::Unavailable,
                Some("HQ embedding queue is not configured"),
            ),
            reranking: stage(SearchStageStatus::IntentionallySkipped, None),
        };

        let message = format_search_response("Semantic", "Z-Rho", Vec::new(), &outcomes)
            .expect("expected search unavailability is a normal tool response");
        assert!(message.contains("Semantic search is unavailable"));
        assert!(message.contains("embedding provider rejected the request"));
        assert!(message.contains("HQ embedding queue is not configured"));
        assert!(message.contains("keyword search"));
        assert!(message.contains("rebuild the semantic index from Settings"));
    }

    #[test]
    fn search_content_is_not_discarded_at_128_tokens() {
        let content = "Z-Rho lore detail ".repeat(180);
        assert!(super::count_tokens(&content) > 128);
        assert_eq!(
            first_matched_search_content(&[content.as_str()]),
            Some(content)
        );
    }

    #[test]
    fn agent_sampling_uses_current_lemonade_wire_names() {
        let params = AgentParams {
            top_p: Some(0.8),
            top_k: Some(40),
            min_p: Some(0.05),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            repetition_penalty: Some(1.1),
            seed: Some(7),
            stop: Some(vec!["END".into()]),
            ..AgentParams::default()
        };
        let value = GraphAgent::build_additional_params(&params).unwrap();
        assert_eq!(value["repeat_penalty"], 1.1);
        assert!(value.get("repetition_penalty").is_none());
        assert_eq!(value["top_p"], 0.8);
        assert_eq!(value["top_k"], 40);
        assert_eq!(value["stop"], json!(["END"]));
    }

    #[test]
    fn agent_reasoning_policy_omits_default_and_sends_explicit_states() {
        let params = AgentParams::default();
        let default = GraphAgent::build_request_additional_params(
            &params,
            u_forge_core::ReasoningPolicy::Default,
        );
        assert!(default.get("enable_thinking").is_none());
        let enabled = GraphAgent::build_request_additional_params(
            &params,
            u_forge_core::ReasoningPolicy::Enabled,
        );
        assert_eq!(enabled["enable_thinking"], true);
        let disabled = GraphAgent::build_request_additional_params(
            &params,
            u_forge_core::ReasoningPolicy::Disabled,
        );
        assert_eq!(disabled["enable_thinking"], false);
    }

    // FtsSearchTool validation

    #[test]
    fn fts_rejects_type_mismatch() {
        let raw = json!({"query": "test", "limit": "ten"});
        let err = validate_tool_args("search_fts", &raw)
            .expect_err("should reject string for numeric field");
        let msg = err.to_string();
        assert!(
            msg.contains("limit") || msg.contains("/limit"),
            "error should name the offending field: {msg}"
        );
    }

    #[test]
    fn fts_rejects_missing_required() {
        let raw = json!({"limit": 5});
        let err = validate_tool_args("search_fts", &raw)
            .expect_err("should reject missing required 'query' field");
        let msg = err.to_string();
        assert!(
            msg.contains("query"),
            "error should name missing field: {msg}"
        );
    }

    #[test]
    fn fts_rejects_unknown_field() {
        let raw = json!({"query": "test", "qury": "typo"});
        let err = validate_tool_args("search_fts", &raw)
            .expect_err("should reject unknown field with additionalProperties: false");
        let msg = err.to_string();
        assert!(
            msg.contains("qury") || msg.to_lowercase().contains("additional"),
            "error should signal unknown field: {msg}"
        );
    }

    #[test]
    fn fts_accepts_valid_args() {
        validate_tool_args("search_fts", &json!({"query": "Gandalf", "limit": 5}))
            .expect("valid args should pass");
        validate_tool_args("search_fts", &json!({"query": "Aragorn"}))
            .expect("optional limit omitted should pass");
    }

    // UpsertNodeTool validation (write path with the most complex schema)

    #[test]
    fn upsert_node_rejects_missing_required() {
        // both `name` and `object_type` are required
        let raw = json!({"object_type": "character"});
        let err = validate_tool_args("upsert_node", &raw)
            .expect_err("should reject missing required 'name'");
        let msg = err.to_string();
        assert!(
            msg.contains("name"),
            "error should name missing field: {msg}"
        );
    }

    #[test]
    fn upsert_node_rejects_unknown_field() {
        let raw = json!({"name": "Gandalf", "object_type": "character", "typo_field": "oops"});
        let err = validate_tool_args("upsert_node", &raw).expect_err("should reject unknown field");
        let msg = err.to_string();
        assert!(
            msg.contains("typo_field") || msg.to_lowercase().contains("additional"),
            "error should signal unknown field: {msg}"
        );
    }

    #[test]
    fn upsert_node_accepts_valid_args() {
        validate_tool_args(
            "upsert_node",
            &json!({"name": "Gandalf", "object_type": "character"}),
        )
        .expect("minimal valid args should pass");

        validate_tool_args(
            "upsert_node",
            &json!({
                "name": "Gandalf",
                "object_type": "character",
                "node_id": "00000000-0000-0000-0000-000000000001",
                "properties": {"description": "A wizard"}
            }),
        )
        .expect("full valid args should pass");
    }

    #[test]
    fn upsert_node_preflight_reports_rules_and_applies_coercion() {
        let temp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp.path()).unwrap();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "spell".to_string(),
            ObjectTypeSchema::new("spell".to_string(), "Spell".to_string())
                .with_property(
                    "level".to_string(),
                    PropertySchema::number("level").with_validation(
                        ValidationRule::new().with_value_range(Some(1.0), Some(5.0)),
                    ),
                )
                .with_required_property("level".to_string()),
        );
        graph.get_schema_manager().save_schema(&schema).unwrap();

        let mut invalid = ObjectMetadata::new("spell".to_string(), "Impossible".to_string())
            .with_property("level".to_string(), "9".to_string());
        let error = preflight_node_properties(&graph, &mut invalid)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("property 'level': maximum value is 5"),
            "{error}"
        );

        let mut valid = ObjectMetadata::new("spell".to_string(), "Shield".to_string())
            .with_property("level".to_string(), "3".to_string());
        preflight_node_properties(&graph, &mut valid).unwrap();
        assert_eq!(valid.get_json_property("level"), Some(&json!(3.0)));
    }

    #[test]
    fn unknown_tool_name_returns_error() {
        let err = validate_tool_args("nonexistent_tool", &json!({}))
            .expect_err("unregistered tool name should return error");
        assert!(err.to_string().contains("nonexistent_tool"));
    }

    #[test]
    fn ambiguous_node_diagnostic_groups_every_candidate_by_type() {
        let temp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp.path()).unwrap();
        let candidates = (0..7)
            .map(|index| {
                ObjectMetadata::new(
                    if index % 2 == 0 { "npc" } else { "location" }.to_string(),
                    "Echo".to_string(),
                )
            })
            .collect::<Vec<_>>();
        for candidate in &candidates {
            graph.add_object(candidate.clone()).unwrap();
        }

        let error = resolve_node(&graph, "Echo").expect_err("name should be ambiguous");
        let message = error.to_string();
        assert!(message.contains("location (3)"));
        assert!(message.contains("npc (4)"));
        assert!(message.contains("complete UUID"));
        for candidate in candidates {
            assert!(
                message.contains(&candidate.id.to_string()),
                "diagnostic omitted {}: {message}",
                candidate.id
            );
        }
    }
}
