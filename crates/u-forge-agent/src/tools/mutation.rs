use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use schemars::JsonSchema;
use serde::Deserialize;
use u_forge_core::ingest::rechunk_and_embed_with_cancellation;
use u_forge_core::types::ObjectMetadata;
use u_forge_core::{
    KnowledgeGraph,
    queue::{CancellationToken, InferenceQueue},
    types::ObjectId,
};

use super::{ToolError, validation};

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

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

pub(crate) fn preflight_node_properties(
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
    const NAME: &'static str = validation::UPSERT_NODE_NAME;

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        validation::find(Self::NAME)
            .expect("node mutation tool is present in the catalog")
            .description
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        validation::find(Self::NAME)
            .expect("node mutation tool is present in the catalog")
            .parameters()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let args: UpsertNodeArgs = validation::decode(Self::NAME, raw)?;
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
    /// Must be an edge type from the active schema, and the resolved source/target node types must be a permitted endpoint pair.
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

    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// Try to parse `input` as a UUID; if that fails, do an exact name lookup.
pub(crate) fn resolve_node(graph: &KnowledgeGraph, input: &str) -> Result<ObjectId, ToolError> {
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

pub(crate) fn preflight_edge_contract(
    graph: &KnowledgeGraph,
    source_id: ObjectId,
    target_id: ObjectId,
    edge_type: &str,
) -> Result<(ObjectMetadata, ObjectMetadata), ToolError> {
    let schema_manager = graph.get_schema_manager();
    if !schema_manager.is_valid_edge_type(edge_type) {
        let valid = schema_manager.all_edge_type_names();
        let choices = if valid.is_empty() {
            "no edge types are loaded".to_string()
        } else {
            valid.join(", ")
        };
        return Err(ToolError(format!(
            "Unknown edge_type \"{edge_type}\". Valid edge types: {choices}"
        )));
    }

    let source = graph
        .get_object(source_id)
        .map_err(|error| ToolError(format!("Failed to load source node: {error:#}")))?
        .ok_or_else(|| ToolError(format!("Source node {source_id} no longer exists")))?;
    let target = graph
        .get_object(target_id)
        .map_err(|error| ToolError(format!("Failed to load target node: {error:#}")))?
        .ok_or_else(|| ToolError(format!("Target node {target_id} no longer exists")))?;
    let candidate =
        u_forge_core::Edge::new(source_id, target_id, u_forge_core::EdgeType::new(edge_type));
    schema_manager
        .validate_edge_cached_strict(&candidate, &source, &target)
        .map_err(|error| {
            ToolError(format!(
                "Edge type \"{edge_type}\" does not permit endpoint pair {} -> {}: {error:#}",
                source.object_type, target.object_type
            ))
        })?;

    Ok((source, target))
}

impl Tool for UpsertEdgeTool {
    const NAME: &'static str = validation::UPSERT_EDGE_NAME;

    type Error = ToolError;
    type Args = serde_json::Value;
    type Output = String;

    fn description(&self) -> String {
        validation::find(Self::NAME)
            .expect("edge mutation tool is present in the catalog")
            .description
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        validation::find(Self::NAME)
            .expect("edge mutation tool is present in the catalog")
            .parameters()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        raw: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let args: UpsertEdgeArgs = validation::decode(Self::NAME, raw)?;
        let source_id = resolve_node(&self.graph, &args.source)?;
        let target_id = resolve_node(&self.graph, &args.target)?;
        let (source, target) =
            preflight_edge_contract(&self.graph, source_id, target_id, &args.edge_type)?;

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

        let mut output = format!(
            "Edge created: {} -[{}]-> {} (weight: {weight:.2})",
            source.name, args.edge_type, target.name,
        );
        for w in &reembed_warnings {
            output.push('\n');
            output.push_str(w);
        }
        Ok(output)
    }
}
