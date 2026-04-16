//! Rig-based agent tools for the u-forge knowledge graph.
//!
//! Exposes three search tools that can be registered with a [`rig`] agent:
//! - [`FtsSearchTool`] — SQLite FTS5 keyword search.
//! - [`SemanticSearchTool`] — Embedding-based approximate nearest-neighbour search.
//! - [`HybridSearchTool`] — Combined FTS5 + semantic + optional reranking.
//!
//! Each tool holds a shared [`KnowledgeGraph`] handle (and [`InferenceQueue`]
//! where inference is required) and formats results as human-readable text
//! suited for LLM consumption.

use std::collections::HashMap;
use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;

use u_forge_core::search::{search_hybrid, HybridSearchConfig, NodeSearchResult};
use u_forge_core::{queue::InferenceQueue, types::ObjectId, KnowledgeGraph};

// ── Error type ────────────────────────────────────────────────────────────────

/// Error returned by all search tools.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SearchToolError(String);

impl From<anyhow::Error> for SearchToolError {
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
        "[{}] {} ({}) — score: {:.4} {}\n",
        index + 1,
        result.node.name,
        result.node.object_type,
        result.score,
        result.sources.label()
    ));
    if let Some(desc) = &result.node.description {
        s.push_str(&format!("  Description: {desc}\n"));
    }
    if !result.node.tags.is_empty() {
        s.push_str(&format!("  Tags: {}\n", result.node.tags.join(", ")));
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
            s.push_str(&format!("    (… {} more chunks)\n", result.chunks.len() - 3));
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

    type Error = SearchToolError;
    type Args = FtsSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Full-text keyword search over the knowledge graph using SQLite FTS5. \
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
            SearchToolError(
                "Query contains no searchable terms after removing punctuation.".to_string(),
            )
        })?;

        // Retrieve more chunks than nodes wanted so groups fill up meaningfully.
        let chunks = self
            .graph
            .search_chunks_fts(&sanitized, limit * 4)
            .map_err(|e| SearchToolError(format!("FTS search failed: {e:#}")))?;

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
                .map_err(|e| SearchToolError(format!("Node hydration failed: {e:#}")))?
            {
                Some(meta) => {
                    output.push_str(&format!(
                        "[{}] {} ({})\n",
                        i + 1,
                        meta.name,
                        meta.object_type
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

    type Error = SearchToolError;
    type Args = SemanticSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Semantic (embedding-based) search over the knowledge graph. \
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
            .map_err(|e| SearchToolError(format!("Embedding failed: {e:#}")))?;

        let chunks = self
            .graph
            .search_chunks_semantic(&query_vec, limit * 4)
            .map_err(|e| SearchToolError(format!("Semantic ANN search failed: {e:#}")))?;

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
            let best_dist = chunks
                .iter()
                .map(|(_, d)| *d)
                .fold(f32::INFINITY, f32::min);
            match self
                .graph
                .get_object(obj_id)
                .map_err(|e| SearchToolError(format!("Node hydration failed: {e:#}")))?
            {
                Some(meta) => {
                    output.push_str(&format!(
                        "[{}] {} ({}) — distance: {:.4}\n",
                        i + 1,
                        meta.name,
                        meta.object_type,
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

    type Error = SearchToolError;
    type Args = HybridSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Hybrid search over the knowledge graph: combines FTS5 keyword matching \
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
            .map_err(SearchToolError::from)?;

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

// ── GraphAgent ────────────────────────────────────────────────────────────────

use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::client::CompletionClient;
use rig::completion::{Prompt, PromptError, message::ToolResultContent};
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
    system_prompt: String,
}

impl GraphAgent {
    /// Build a `GraphAgent` connected to the given Lemonade base URL,
    /// e.g. `http://localhost:13305/api/v1`.
    pub fn new(
        lemonade_url: &str,
        graph: Arc<KnowledgeGraph>,
        queue: Arc<InferenceQueue>,
        system_prompt: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let client = CompletionsClient::builder()
            .api_key("lemonade")
            .base_url(lemonade_url)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build rig client: {e}"))?;
        Ok(Self {
            client,
            graph,
            queue,
            system_prompt: system_prompt.into(),
        })
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
        max_turns: usize,
    ) -> mpsc::Receiver<AgentStreamEvent> {
        let (tx, rx) = mpsc::channel(64);

        let agent = self
            .client
            .agent(model_id)
            .preamble(&self.system_prompt)
            .tool(HybridSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .tool(FtsSearchTool::new(self.graph.clone()))
            .tool(SemanticSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .build();

        let user_message = user_message.to_string();

        tokio::spawn(async move {
            let mut stream = agent
                .stream_prompt(&user_message)
                .multi_turn(max_turns)
                .await;

            let mut final_text = String::new();

            while let Some(item) = stream.next().await {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => match content {
                        StreamedAssistantContent::Text(t) => {
                            final_text.push_str(&t.text);
                            let _ = tx.send(AgentStreamEvent::TextDelta(t.text)).await;
                        }
                        StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        } => {
                            let args_display = serde_json::to_string_pretty(
                                &tool_call.function.arguments,
                            )
                            .unwrap_or_else(|_| tool_call.function.arguments.to_string());
                            let _ = tx
                                .send(AgentStreamEvent::ToolCallStart {
                                    internal_id: internal_call_id,
                                    name: tool_call.function.name,
                                    args_display,
                                })
                                .await;
                        }
                        StreamedAssistantContent::Reasoning(r) => {
                            // Full reasoning block (some providers emit this instead of deltas).
                            for chunk in &r.content {
                                if let rig::completion::message::ReasoningContent::Text { text, .. } = chunk {
                                    let _ = tx.send(AgentStreamEvent::ReasoningDelta(text.clone())).await;
                                }
                            }
                        }
                        StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                            let _ = tx.send(AgentStreamEvent::ReasoningDelta(reasoning)).await;
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
                            let _ = tx
                                .send(AgentStreamEvent::ToolResult {
                                    internal_id: internal_call_id,
                                    content: result_text,
                                })
                                .await;
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
                        break;
                    }
                    Ok(_) => {
                        // Non-exhaustive: ignore any new MultiTurnStreamItem variants.
                    }
                    Err(e) => {
                        let _ = tx.send(AgentStreamEvent::Error(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        rx
    }

    /// Run the agent tool loop for a single user message.
    ///
    /// Builds an agent with [`HybridSearchTool`], [`FtsSearchTool`], and
    /// [`SemanticSearchTool`], then calls the LLM using `model_id` up to
    /// `max_turns` times (each turn may trigger tool calls). Returns the
    /// model's final text response.
    pub async fn prompt(
        &self,
        model_id: &str,
        user_message: &str,
        max_turns: usize,
    ) -> Result<String, String> {
        let agent = self
            .client
            .agent(model_id)
            .preamble(&self.system_prompt)
            .tool(HybridSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .tool(FtsSearchTool::new(self.graph.clone()))
            .tool(SemanticSearchTool::new(
                self.graph.clone(),
                self.queue.clone(),
            ))
            .build();
        agent
            .prompt(user_message)
            .max_turns(max_turns)
            .await
            .map_err(|e: PromptError| e.to_string())
    }
}

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use rig;
