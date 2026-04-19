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
use std::sync::{Arc, LazyLock};

use tiktoken_rs::CoreBPE;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

use u_forge_core::ingest::rechunk_and_embed;
use u_forge_core::search::{search_hybrid, HybridSearchConfig, NodeSearchResult};
use u_forge_core::types::ObjectMetadata;
use u_forge_core::{queue::InferenceQueue, types::ObjectId, KnowledgeGraph};

// ── History and token counting ────────────────────────────────────────────────

/// A single prior conversation turn for context injection.
///
/// `role` is `"user"` or `"assistant"`.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// Token overhead added per message in the OpenAI chat format
/// (role markers + separator bytes).
const TOKENS_PER_MESSAGE: usize = 4;

/// Cached o200k_harmony BPE tokenizer — constructed once, reused forever.
///
/// `o200k_harmony()` parses a ~200 k-entry vocabulary on every call; caching
/// it here keeps repeated `count_tokens` invocations (e.g. inside
/// [`select_history_window`]) from rebuilding the tokenizer each time.
static O200K_BPE: LazyLock<CoreBPE> =
    LazyLock::new(|| tiktoken_rs::o200k_harmony().expect("o200k_harmony is always available"));

/// Count BPE tokens in `text` using the o200k_harmony encoding.
///
/// o200k_harmony is used by GPT-4o and is becoming the standard for local
/// open-weight models. Close enough for context-window budgeting.
pub fn count_tokens(text: &str) -> usize {
    O200K_BPE.encode_with_special_tokens(text).len()
}

/// Return the subset of `history` that fits inside the available token budget.
///
/// Budget is computed as:
/// ```text
/// max_context_tokens - response_reserve - tokens(system_prompt) - tokens(current_msg) - 3
/// ```
/// (The trailing 3 accounts for the assistant reply-priming tokens in OpenAI format.)
///
/// Messages are evaluated newest-first; the returned `Vec` is in chronological
/// order (oldest first), ready to pass directly to `with_history()`.
pub fn select_history_window(
    history: &[HistoryMessage],
    system_prompt: &str,
    current_msg: &str,
    max_context_tokens: usize,
    response_reserve: usize,
) -> Vec<HistoryMessage> {
    let fixed = count_tokens(system_prompt)
        + TOKENS_PER_MESSAGE
        + count_tokens(current_msg)
        + TOKENS_PER_MESSAGE
        + 3; // reply-priming
    let budget = max_context_tokens
        .saturating_sub(response_reserve)
        .saturating_sub(fixed);

    let mut selected: Vec<&HistoryMessage> = Vec::new();
    let mut used = 0usize;
    for msg in history.iter().rev() {
        let cost = count_tokens(&msg.content) + TOKENS_PER_MESSAGE;
        if used + cost > budget {
            break;
        }
        used += cost;
        selected.push(msg);
    }

    selected.into_iter().rev().cloned().collect()
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

// ── FTS5 sanitisation (mirrors search/sanitize.rs — not exported from core) ──

/// Strip characters that cause FTS5 syntax errors from a free-text query.
/// Returns `None` when no searchable tokens remain.
fn fts5_sanitize(query: &str) -> Option<String> {
    let sanitized: String = query
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
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
    if !result.edges.is_empty() {
        s.push_str("  Relationships:\n");
        for edge in &result.edges {
            let from_name = if edge.from == result.node.id {
                result.node.name.clone()
            } else {
                result
                    .connected_node_names
                    .get(&edge.from)
                    .map(|cn| cn.name.clone())
                    .unwrap_or_else(|| edge.from.hyphenated().to_string())
            };
            let to_name = if edge.to == result.node.id {
                result.node.name.clone()
            } else {
                result
                    .connected_node_names
                    .get(&edge.to)
                    .map(|cn| cn.name.clone())
                    .unwrap_or_else(|| edge.to.hyphenated().to_string())
            };
            s.push_str(&format!(
                "    • {from_name} -[{}]-> {to_name}\n",
                edge.edge_type.as_str()
            ));
        }
    }
    if !result.chunks.is_empty() {
        s.push_str("  Content:\n");
        for chunk in result.chunks.iter().take(3) {
            s.push_str(&format!("    • {}\n", chunk.content));
        }
        if result.chunks.len() > 3 {
            s.push_str(&format!(
                "    (… {} more chunks)\n",
                result.chunks.len() - 3
            ));
        }
    }
    s
}

// ── FtsSearchTool ─────────────────────────────────────────────────────────────

/// Arguments for [`FtsSearchTool`].
#[derive(Debug, Deserialize, JsonSchema)]
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
    type Args = FtsSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Full-text keyword search over the knowledge graph using SQLite FTS5. \
                 Fast and exact — good for specific names, terms, or known phrases. \
                 Returns nodes that contain matching text, with the matching snippets."
                .to_string(),
            parameters: serde_json::to_value(schema_for!(FtsSearchArgs))
                .expect("FtsSearchArgs schema is always valid JSON"),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
                    for chunk in chunks.iter().take(3) {
                        output.push_str(&format!("  • {chunk}\n"));
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
}

impl SemanticSearchTool {
    pub fn new(graph: Arc<KnowledgeGraph>, queue: Arc<InferenceQueue>) -> Self {
        Self { graph, queue }
    }
}

impl Tool for SemanticSearchTool {
    const NAME: &'static str = "search_semantic";

    type Error = ToolError;
    type Args = SemanticSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Semantic (embedding-based) search over the knowledge graph. \
                 Finds conceptually related nodes even when exact keywords don't match. \
                 Use for exploratory queries, related concepts, or when FTS returns nothing."
                .to_string(),
            parameters: serde_json::to_value(schema_for!(SemanticSearchArgs))
                .expect("SemanticSearchArgs schema is always valid JSON"),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let limit = args.limit.unwrap_or(5);

        let query_vec = self
            .queue
            .embed(&args.query)
            .await
            .map_err(|e| ToolError(format!("Embedding failed: {e:#}")))?;

        let chunks = self
            .graph
            .search_chunks_semantic(&query_vec, limit * 4)
            .map_err(|e| ToolError(format!("Semantic ANN search failed: {e:#}")))?;

        // Group chunks by node, preserving ANN distance order (closest = first).
        let mut node_order: Vec<ObjectId> = Vec::new();
        let mut node_chunks: HashMap<ObjectId, Vec<(String, f32)>> = HashMap::new();
        for (_chunk_id, obj_id, content, distance) in chunks {
            if !node_chunks.contains_key(&obj_id) {
                node_order.push(obj_id);
            }
            node_chunks
                .entry(obj_id)
                .or_default()
                .push((content, distance));
        }

        if node_order.is_empty() {
            return Ok(format!(
                "Semantic search found no results for \"{}\". \
                 The graph may not have embeddings yet.",
                args.query
            ));
        }

        let mut output = format!(
            "Semantic search results for \"{}\" ({} nodes):\n\n",
            args.query,
            node_order.len().min(limit)
        );

        for (i, obj_id) in node_order.into_iter().take(limit).enumerate() {
            let chunks = node_chunks.remove(&obj_id).unwrap_or_default();
            let best_dist = chunks.iter().map(|(_, d)| *d).fold(f32::INFINITY, f32::min);
            match self
                .graph
                .get_object(obj_id)
                .map_err(|e| ToolError(format!("Node hydration failed: {e:#}")))?
            {
                Some(meta) => {
                    output.push_str(&format!(
                        "[{}] {} ({}) [id: {}] — distance: {:.4}\n",
                        i + 1,
                        meta.name,
                        meta.object_type,
                        obj_id,
                        best_dist
                    ));
                    for (chunk, _dist) in chunks.iter().take(3) {
                        output.push_str(&format!("  • {chunk}\n"));
                    }
                    output.push('\n');
                }
                None => continue,
            }
        }

        Ok(output)
    }
}

// ── HybridSearchTool ──────────────────────────────────────────────────────────

/// Arguments for [`HybridSearchTool`].
#[derive(Debug, Deserialize, JsonSchema)]
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
}

impl HybridSearchTool {
    pub fn new(graph: Arc<KnowledgeGraph>, queue: Arc<InferenceQueue>) -> Self {
        Self { graph, queue }
    }
}

impl Tool for HybridSearchTool {
    const NAME: &'static str = "search_hybrid";

    type Error = ToolError;
    type Args = HybridSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Hybrid search over the knowledge graph: combines FTS5 keyword matching \
                 with semantic embedding search using Reciprocal Rank Fusion, then \
                 optionally reranks results with a cross-encoder. Returns fully hydrated \
                 node results with metadata, relationships, and content. \
                 Recommended as the default search tool."
                .to_string(),
            parameters: serde_json::to_value(schema_for!(HybridSearchArgs))
                .expect("HybridSearchArgs schema is always valid JSON"),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let config = HybridSearchConfig {
            limit: args.limit.unwrap_or(3),
            alpha: args.alpha.unwrap_or(0.5).clamp(0.0, 1.0),
            rerank: args.rerank.unwrap_or(true),
            ..HybridSearchConfig::default()
        };

        let results = search_hybrid(&self.graph, &self.queue, None, &args.query, &config)
            .await
            .map_err(ToolError::from)?;

        if results.is_empty() {
            return Ok(format!(
                "Hybrid search found no results for \"{}\". \
                 Try rephrasing, or the graph may be empty.",
                args.query
            ));
        }

        let mut output = format!(
            "Hybrid search results for \"{}\" ({} nodes):\n\n",
            args.query,
            results.len()
        );
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format_node_result(result, i));
            output.push('\n');
        }

        Ok(output)
    }
}

// ── UpsertNodeTool ────────────────────────────────────────────────────────────

/// Arguments for [`UpsertNodeTool`].
#[derive(Debug, Deserialize, JsonSchema)]
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
        }
    }
}

impl Tool for UpsertNodeTool {
    const NAME: &'static str = "upsert_node";

    type Error = ToolError;
    type Args = UpsertNodeArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Create or update a knowledge graph node. Always search first to avoid duplicates. \
                 Populate name, object_type, and all known properties in one call. \
                 On update (node_id set), only changed keys are needed — omitted keys are kept."
                    .to_string(),
            parameters: serde_json::to_value(schema_for!(UpsertNodeArgs))
                .expect("UpsertNodeArgs schema is always valid JSON"),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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

        // Validate object_type against schema.
        let schema_mgr = self.graph.get_schema_manager();
        if !schema_mgr.is_valid_object_type(&args.object_type) {
            let valid = schema_mgr.all_object_type_names().join(", ");
            return Err(ToolError(format!(
                "Unknown object_type \"{}\". Valid types: {valid}",
                args.object_type
            )));
        }

        // Apply caller-provided fields.
        meta.name = args.name;
        meta.object_type = args.object_type;
        if let Some(props) = args.properties {
            if let (serde_json::Value::Object(incoming), serde_json::Value::Object(ref mut existing)) =
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
        }

        // Persist the node.
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
        let chunks = rechunk_and_embed(&self.graph, &self.queue, hq_ref, object_id)
            .await
            .map_err(|e| ToolError(format!("Embedding failed: {e:#}")))?;

        let action = if is_update { "Updated" } else { "Created" };
        Ok(format!(
            "{action} node \"{name}\" ({object_type}). node_id: {object_id}. chunks_embedded: {chunks}.",
            name = meta.name,
            object_type = meta.object_type,
        ))
    }
}

// ── UpsertEdgeTool ────────────────────────────────────────────────────────────

/// Arguments for [`UpsertEdgeTool`].
#[derive(Debug, Deserialize, JsonSchema)]
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
        }
    }
}

/// Try to parse `input` as a UUID; if that fails, do an exact name lookup.
fn resolve_node(graph: &KnowledgeGraph, input: &str) -> Result<ObjectId, ToolError> {
    // Try UUID first.
    if let Ok(oid) = ObjectId::parse_str(input) {
        if graph.get_object(oid).ok().flatten().is_some() {
            return Ok(oid);
        }
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
            let list: Vec<String> = matches
                .iter()
                .take(5)
                .map(|m| format!("  • {} ({}) [{}]", m.name, m.object_type, m.id))
                .collect();
            Err(ToolError(format!(
                "\"{input}\" matched {n} nodes — provide the UUID to disambiguate:\n{}",
                list.join("\n")
            )))
        }
    }
}

impl Tool for UpsertEdgeTool {
    const NAME: &'static str = "upsert_edge";

    type Error = ToolError;
    type Args = UpsertEdgeArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Create or update a relationship (edge) between two nodes in the knowledge graph. \
                 Nodes can be specified by exact name or UUID. \
                 Both endpoint nodes are re-indexed after the edge is saved."
                    .to_string(),
            parameters: serde_json::to_value(schema_for!(UpsertEdgeArgs))
                .expect("UpsertEdgeArgs schema is always valid JSON"),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let source_id = resolve_node(&self.graph, &args.source)?;
        let target_id = resolve_node(&self.graph, &args.target)?;

        let weight = args.weight.unwrap_or(1.0);
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
            if let Err(e) = rechunk_and_embed(&self.graph, &self.queue, hq_ref, oid).await {
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
use rig::client::CompletionClient;
use rig::completion::{message::ToolResultContent, Prompt, PromptError};
use rig::providers::openai::CompletionsClient;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use tokio::sync::mpsc;

// ── Stream event type ─────────────────────────────────────────────────────────

/// An event emitted by [`GraphAgent::prompt_stream`] as the agent loop runs.
#[derive(Debug, Clone)]
pub enum AgentStreamEvent {
    /// Partial reasoning/thinking token (streamed before the final response).
    ReasoningDelta(String),
    /// Partial text token streaming from the LLM.
    TextDelta(String),
    /// The LLM has decided to call a tool. Args are pretty-printed JSON.
    ToolCallStart {
        /// Stable identifier correlating this call with its [`ToolResult`].
        internal_id: String,
        /// Tool name, e.g. `"search_hybrid"`.
        name: String,
        /// Human-readable JSON arguments.
        args_display: String,
    },
    /// A tool has returned its result.
    ToolResult {
        /// Matches the `internal_id` from the preceding [`ToolCallStart`].
        internal_id: String,
        content: String,
    },
    /// The agent loop is done; this is the complete final response text.
    Done(String),
    /// A fatal error terminated the loop.
    Error(String),
}

/// Sampling and generation parameters for the agent, built from
/// [`ChatDeviceConfig`] + [`ChatConfig`].
#[derive(Debug, Clone)]
pub struct AgentParams {
    /// Sampling temperature (default 0.3 for reliable tool use).
    pub temperature: f64,
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
}

impl Default for AgentParams {
    fn default() -> Self {
        Self {
            temperature: 0.3,
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
    system_prompt: String,
    params: AgentParams,
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
        let client = CompletionsClient::builder()
            .api_key("lemonade")
            .base_url(lemonade_url)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build rig client: {e}"))?;
        let base_prompt: String = system_prompt.into();
        let schema_summary = graph.schema_prompt_summary_all();

        let tool_guidance = "\
## Tool-use guidelines

1. **Search before writing.** Before creating a node, search to check it doesn't already exist.
2. **One call per node.** Include name, object_type, and all known properties in a single \
   upsert_node call. Never create a blank node and fill properties afterwards.
3. **Refer to the schema below** for valid object_type values and their properties. \
   Use the property names and types exactly as listed.
4. **Stop when done.** After a successful tool call, report the result to the user. \
   Do not re-call a tool for the same node unless asked.";

        let full_prompt = if schema_summary.is_empty() {
            format!("{base_prompt}\n\n{tool_guidance}")
        } else {
            format!("{base_prompt}\n\n{tool_guidance}\n\n{schema_summary}")
        };

        Ok(Self {
            client,
            graph,
            queue,
            hq_queue,
            system_prompt: full_prompt,
            params,
        })
    }

    /// Build `additional_params` JSON from the non-standard sampling knobs.
    ///
    /// Rig's OpenAI provider flattens this into the request body, so keys like
    /// `frequency_penalty`, `top_p`, `seed`, etc. end up as top-level fields
    /// in the OpenAI-compatible `/chat/completions` request.
    fn additional_params(&self) -> Option<serde_json::Value> {
        let mut map = serde_json::Map::new();
        if let Some(v) = self.params.top_p {
            map.insert("top_p".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.top_k {
            map.insert("top_k".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.min_p {
            map.insert("min_p".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.frequency_penalty {
            map.insert("frequency_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.presence_penalty {
            map.insert("presence_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.repetition_penalty {
            map.insert("repetition_penalty".into(), serde_json::json!(v));
        }
        if let Some(v) = self.params.seed {
            map.insert("seed".into(), serde_json::json!(v));
        }
        if let Some(ref v) = self.params.stop {
            map.insert("stop".into(), serde_json::json!(v));
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    }

    /// Build a rig agent configured with all sampling params and tools.
    fn build_agent(&self, model_id: &str) -> rig::agent::Agent<rig::providers::openai::CompletionModel> {
        let mut builder = self
            .client
            .agent(model_id)
            .preamble(&self.system_prompt)
            .temperature(self.params.temperature);

        if let Some(max_tokens) = self.params.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }
        if let Some(additional) = self.additional_params() {
            builder = builder.additional_params(additional);
        }

        builder
            .tool(HybridSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .tool(FtsSearchTool::new(self.graph.clone()))
            .tool(SemanticSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .tool(UpsertNodeTool::new(
                self.graph.clone(),
                self.queue.clone(),
                self.hq_queue.clone(),
            ))
            .tool(UpsertEdgeTool::new(
                self.graph.clone(),
                self.queue.clone(),
                self.hq_queue.clone(),
            ))
            .build()
    }

    /// Run the agent loop with streaming output.
    ///
    /// Returns a [`mpsc::Receiver`] that yields [`AgentStreamEvent`]s as the
    /// agent streams text, calls tools, and receives tool results. The channel
    /// closes after a [`AgentStreamEvent::Done`] or [`AgentStreamEvent::Error`].
    pub async fn prompt_stream(
        &self,
        model_id: &str,
        user_message: &str,
        history: &[HistoryMessage],
    ) -> mpsc::Receiver<AgentStreamEvent> {
        let (tx, rx) = mpsc::channel(64);

        let agent = self.build_agent(model_id);
        let max_turns = self.params.max_tool_turns;

        let user_message = user_message.to_string();
        // Convert HistoryMessage → rig::completion::message::Message.
        let rig_history: Vec<rig::completion::message::Message> = history
            .iter()
            .map(|m| {
                if m.role == "assistant" {
                    rig::completion::message::Message::assistant(&m.content)
                } else {
                    rig::completion::message::Message::user(&m.content)
                }
            })
            .collect();

        tokio::spawn(async move {
            let mut stream = agent
                .stream_prompt(&user_message)
                .with_history(rig_history)
                .multi_turn(max_turns)
                .await;

            let mut final_text = String::new();

            // `'stream` labels the outer loop so every send-on-error can break it directly.
            // If the receiver is dropped (user closed the chat panel, app exited) we stop
            // driving the rig stream instead of burning LLM inference and potentially
            // running write tools the caller will never observe.
            'stream: while let Some(item) = stream.next().await {
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
                        }
                        StreamedAssistantContent::Reasoning(r) => {
                            // Full reasoning block (some providers emit this instead of deltas).
                            for chunk in &r.content {
                                if let rig::completion::message::ReasoningContent::Text {
                                    text,
                                    ..
                                } = chunk
                                {
                                    if tx
                                        .send(AgentStreamEvent::ReasoningDelta(text.clone()))
                                        .await
                                        .is_err()
                                    {
                                        break 'stream;
                                    }
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
                        }
                    },
                    Ok(MultiTurnStreamItem::FinalResponse(resp)) => {
                        // FinalResponse carries the full aggregated text for the
                        // last turn. Use it if we didn't accumulate via TextDelta.
                        let text = if final_text.is_empty() {
                            resp.response().to_string()
                        } else {
                            final_text.clone()
                        };
                        let _ = tx.send(AgentStreamEvent::Done(text)).await;
                        break 'stream;
                    }
                    Ok(_) => {
                        // Non-exhaustive: ignore any new MultiTurnStreamItem variants.
                    }
                    Err(e) => {
                        let _ = tx.send(AgentStreamEvent::Error(e.to_string())).await;
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
        let agent = self.build_agent(model_id);
        let rig_history: Vec<rig::completion::message::Message> = history
            .iter()
            .map(|m| {
                if m.role == "assistant" {
                    rig::completion::message::Message::assistant(&m.content)
                } else {
                    rig::completion::message::Message::user(&m.content)
                }
            })
            .collect();
        agent
            .prompt(user_message)
            .with_history(rig_history)
            .max_turns(self.params.max_tool_turns)
            .await
            .map_err(|e: PromptError| e.to_string())
    }
}

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use rig;
