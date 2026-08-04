//! Data ingestion module for u-forge.ai
//!
//! Imports the canonical JSONL export format produced by `convert_memorymesh`:
//!
//! ```json
//! {"entitytype":"node","id":"<uuid>","nodetype":"faction",
//!  "properties":{"name":"Galactic Empire","goals":["..."],...}}
//! {"entitytype":"edge","from":"Mayor Salvor Hardin","to":"Terminus","edgeType":"located_in"}
//! ```
//!
//! Field mapping:
//! - `entitytype`   — discriminant tag ("node" | "edge")
//! - `id`           — source UUID (stored as `_source_id` property; used for dedup)
//! - `nodetype`     — schema type name (e.g. "npc", "faction", "location")
//! - `properties`   — typed JSON object; arrays stay arrays, strings stay strings
//!
//! Dedup: nodes are matched first by `_source_id`, then by `(nodetype, name)`.

use crate::KnowledgeGraph;
use crate::schema::{EdgeTypeSchema, ObjectTypeSchema};
use crate::types::*;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "entitytype")]
pub enum JsonEntry {
    #[serde(rename = "node")]
    Node {
        id: String,
        #[serde(rename = "nodetype")]
        node_type: String,
        properties: Map<String, Value>,
    },
    #[serde(rename = "edge")]
    Edge {
        from: String,
        to: String,
        #[serde(rename = "edgeType")]
        edge_type: String,
    },
}

#[derive(Debug)]
pub struct IngestionStats {
    pub nodes_parsed: usize,
    pub edges_parsed: usize,
    pub objects_created: usize,
    pub objects_reused: usize,
    pub relationships_created: usize,
    pub parse_errors: usize,
    pub object_records_skipped: usize,
    pub edge_records_skipped: usize,
    pub validation_errors: usize,
    pub dropped_properties: usize,
    pub diagnostics_path: Option<PathBuf>,
}

pub struct DataIngestion<'a> {
    graph: &'a KnowledgeGraph,
    stats: IngestionStats,
    validation_diagnostics: BTreeMap<String, ImportDiagnosticSummary>,
    dropped_property_diagnostics: BTreeMap<String, ImportDiagnosticSummary>,
    reused_object_diagnostics: BTreeMap<String, ImportDiagnosticSummary>,
    diagnostic_records: Vec<ImportDiagnosticRecord>,
    schema_cache: Option<ImportSchemaCache>,
}

#[derive(Debug, Default)]
struct ImportDiagnosticSummary {
    count: usize,
    items: Vec<String>,
}

struct ImportSchemaCache {
    schemas: Vec<(String, Arc<crate::schema::SchemaDefinition>)>,
}

#[derive(Default)]
struct NameResolutionCache {
    results: HashMap<String, Option<ObjectId>>,
    storage_lookups: usize,
    cache_hits: usize,
}

struct PendingImportObject {
    source_id: String,
    node_type: String,
    name: String,
    properties_json: String,
    metadata: ObjectMetadata,
}

struct PendingImportEdge {
    from_name: String,
    to_name: String,
    edge_type: String,
    edge: Edge,
}

#[derive(Debug, Clone, Serialize)]
struct ImportDiagnosticRecord {
    category: String,
    constraint: String,
    item: String,
}

impl<'a> DataIngestion<'a> {
    pub fn new(graph: &'a KnowledgeGraph) -> Self {
        Self {
            graph,
            stats: IngestionStats {
                nodes_parsed: 0,
                edges_parsed: 0,
                objects_created: 0,
                objects_reused: 0,
                relationships_created: 0,
                parse_errors: 0,
                object_records_skipped: 0,
                edge_records_skipped: 0,
                validation_errors: 0,
                dropped_properties: 0,
                diagnostics_path: None,
            },
            validation_diagnostics: BTreeMap::new(),
            dropped_property_diagnostics: BTreeMap::new(),
            reused_object_diagnostics: BTreeMap::new(),
            diagnostic_records: Vec::new(),
            schema_cache: None,
        }
    }

    /// Import JSONL data from a file into the knowledge graph.
    pub async fn import_json_data<P: AsRef<Path>>(&mut self, data_file: P) -> Result<()> {
        let data_file = data_file.as_ref();
        let total_start = Instant::now();
        info!("Loading JSON data from: {:?}", data_file);

        let read_start = Instant::now();
        let file_content = fs::read_to_string(data_file)
            .with_context(|| format!("Failed to read file: {:?}", data_file))?;
        let read_duration_ms = read_start.elapsed().as_millis() as u64;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        let parse_start = Instant::now();
        for (line_num, line) in file_content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonEntry>(line) {
                Ok(entry) => match entry {
                    JsonEntry::Node { .. } => nodes.push(entry),
                    JsonEntry::Edge { .. } => edges.push(entry),
                },
                Err(e) => {
                    self.stats.parse_errors += 1;
                    error!("Line {}: Failed to parse JSON: {}", line_num + 1, e);
                    if line.len() > 100 {
                        error!("   Content preview: {}...", &line[..100]);
                    } else {
                        error!("   Content: {}", line);
                    }
                }
            }
        }
        let parse_duration_ms = parse_start.elapsed().as_millis() as u64;

        if self.stats.parse_errors > 0 {
            warn!("Total parse errors: {}", self.stats.parse_errors);
        }

        info!(
            "Parsed {} nodes and {} edges from JSON",
            nodes.len(),
            edges.len()
        );
        self.stats.nodes_parsed = nodes.len();
        self.stats.edges_parsed = edges.len();

        let mut name_to_id = HashMap::new();
        let mut id_to_type = HashMap::new();
        let mut name_resolution_cache = NameResolutionCache::default();
        let object_phase_start = Instant::now();
        self.create_objects(nodes, &mut name_to_id, &mut id_to_type)
            .await?;
        let object_phase_duration_ms = object_phase_start.elapsed().as_millis() as u64;
        let relationship_phase_start = Instant::now();
        self.create_relationships(edges, &name_to_id, &id_to_type, &mut name_resolution_cache)
            .await?;
        let relationship_phase_duration_ms = relationship_phase_start.elapsed().as_millis() as u64;
        let diagnostics_start = Instant::now();
        self.write_import_diagnostics(data_file)?;
        self.log_import_diagnostics();
        let diagnostics_duration_ms = diagnostics_start.elapsed().as_millis() as u64;

        info!(
            nodes_parsed = self.stats.nodes_parsed,
            edges_parsed = self.stats.edges_parsed,
            read_duration_ms,
            parse_duration_ms,
            object_phase_duration_ms,
            relationship_phase_duration_ms,
            diagnostics_duration_ms,
            duration_ms = total_start.elapsed().as_millis() as u64,
            "JSON data import phases finished"
        );

        Ok(())
    }

    pub fn get_stats(&self) -> &IngestionStats {
        &self.stats
    }

    async fn create_objects(
        &mut self,
        nodes: Vec<JsonEntry>,
        name_to_id: &mut HashMap<String, ObjectId>,
        id_to_type: &mut HashMap<ObjectId, String>,
    ) -> Result<()> {
        let start = Instant::now();
        info!("Creating {} objects...", nodes.len());
        let existing_load_start = Instant::now();
        let mut existing_by_type_name: HashMap<(String, String), ObjectId> = self
            .graph
            .get_all_objects()?
            .into_iter()
            .map(|object| ((object.object_type, object.name), object.id))
            .collect();
        let existing_load_duration_ms = existing_load_start.elapsed().as_millis() as u64;
        let mut pending = Vec::new();

        let prepare_start = Instant::now();
        for entry in nodes {
            if let JsonEntry::Node {
                id: source_id,
                node_type,
                properties,
            } = entry
            {
                let name = match properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                {
                    Some(n) => n,
                    None => {
                        let properties_json = compact_json_object(&properties);
                        self.record_object_validation_drop(
                            "object:<unknown>:property:name".to_string(),
                            format!("source_id={source_id}; properties={properties_json}"),
                        );
                        warn!(
                            source_id,
                            object_type = node_type,
                            properties = %properties_json,
                            constraint = "required_property:name",
                            "Skipping node with no 'name' property"
                        );
                        continue;
                    }
                };

                // Dedup: check by source_id first, then by (type, name).
                let existing_id = self.find_existing(
                    &source_id,
                    &node_type,
                    &name,
                    name_to_id,
                    &existing_by_type_name,
                );
                if let Some(existing) = existing_id {
                    self.record_reused_object(
                        format!("object:{node_type}"),
                        format!("{name}: source_id={source_id}; existing_id={existing}"),
                    );
                    warn!(
                        "Skipping duplicate '{}' (type: '{}'), reusing existing id {}",
                        name, node_type, existing
                    );
                    name_to_id.insert(name, existing);
                    id_to_type.insert(existing, node_type);
                    continue;
                }

                let properties_json = compact_json_object(&properties);
                let object_metadata = match self
                    .create_object_by_type(&source_id, &node_type, &properties)
                    .await
                {
                    Ok(metadata) => metadata,
                    Err(e) => {
                        let error = e.to_string();
                        self.record_object_validation_drop(
                            format!("object:{node_type}"),
                            format!("{name}: {error}; properties={properties_json}"),
                        );
                        warn!(
                            source_id,
                            object_type = node_type,
                            object_name = name,
                            error,
                            properties = %properties_json,
                            "Skipping object that failed schema validation"
                        );
                        continue;
                    }
                };

                existing_by_type_name.insert((node_type.clone(), name.clone()), object_metadata.id);
                pending.push(PendingImportObject {
                    source_id,
                    node_type,
                    name,
                    properties_json,
                    metadata: object_metadata,
                });
            }
        }
        let prepare_duration_ms = prepare_start.elapsed().as_millis() as u64;

        let pending_objects = pending.len();
        let persist_start = Instant::now();
        self.persist_import_objects(pending, name_to_id, id_to_type)?;
        let persist_duration_ms = persist_start.elapsed().as_millis() as u64;

        info!("Created {} objects total", self.stats.objects_created);
        info!(
            objects_created = self.stats.objects_created,
            objects_reused = self.stats.objects_reused,
            object_records_skipped = self.stats.object_records_skipped,
            pending_objects,
            existing_load_duration_ms,
            prepare_duration_ms,
            persist_duration_ms,
            duration_ms = start.elapsed().as_millis() as u64,
            "Import object phase finished"
        );
        Ok(())
    }

    /// Check for a pre-existing object by (type, name).
    ///
    /// The `source_id` parameter is accepted for forward-compatibility but is not yet
    /// queryable — a property-index lookup can be added once `find_by_property` exists
    /// on `KnowledgeGraph`.
    fn find_existing(
        &self,
        _source_id: &str,
        node_type: &str,
        name: &str,
        name_to_id: &HashMap<String, ObjectId>,
        existing_by_type_name: &HashMap<(String, String), ObjectId>,
    ) -> Option<ObjectId> {
        name_to_id.get(name).copied().or_else(|| {
            existing_by_type_name
                .get(&(node_type.to_string(), name.to_string()))
                .copied()
        })
    }

    fn persist_import_objects(
        &mut self,
        pending: Vec<PendingImportObject>,
        name_to_id: &mut HashMap<String, ObjectId>,
        id_to_type: &mut HashMap<ObjectId, String>,
    ) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }

        let metadata = pending
            .iter()
            .map(|pending| pending.metadata.clone())
            .collect::<Vec<_>>();
        if self.graph.add_objects(metadata).is_ok() {
            for pending in pending {
                name_to_id.insert(pending.name, pending.metadata.id);
                id_to_type.insert(pending.metadata.id, pending.node_type);
                self.stats.objects_created += 1;
            }
            return Ok(());
        }

        for pending in pending {
            match self.graph.add_object(pending.metadata.clone()) {
                Ok(id) => {
                    name_to_id.insert(pending.name, id);
                    id_to_type.insert(id, pending.node_type);
                    self.stats.objects_created += 1;
                }
                Err(e) => {
                    let error = e.to_string();
                    self.record_object_validation_drop(
                        format!("object:{}:storage", pending.node_type),
                        format!(
                            "{}: {error}; properties={}",
                            pending.name, pending.properties_json
                        ),
                    );
                    warn!(
                        source_id = pending.source_id,
                        object_type = pending.node_type,
                        object_name = pending.name,
                        error,
                        properties = %pending.properties_json,
                        "Skipping object that failed storage import"
                    );
                }
            }
        }
        Ok(())
    }

    async fn create_relationships(
        &mut self,
        edges: Vec<JsonEntry>,
        name_to_id: &HashMap<String, ObjectId>,
        id_to_type: &HashMap<ObjectId, String>,
        name_resolution_cache: &mut NameResolutionCache,
    ) -> Result<()> {
        let start = Instant::now();
        info!("Creating {} relationships...", edges.len());
        let mut pending = Vec::new();

        let prepare_start = Instant::now();
        for entry in edges {
            if let JsonEntry::Edge {
                from,
                to,
                edge_type,
            } = entry
            {
                let from_id = self.resolve_node_id(&from, name_to_id, name_resolution_cache);
                let to_id = self.resolve_node_id(&to, name_to_id, name_resolution_cache);

                match (from_id, to_id) {
                    (Some(fid), Some(tid)) => {
                        if let Err(e) = self
                            .validate_import_edge(&edge_type, fid, tid, id_to_type)
                            .await
                        {
                            let source_type = self
                                .node_type_for_id(fid, id_to_type)?
                                .unwrap_or_else(|| "<missing>".to_string());
                            let target_type = self
                                .node_type_for_id(tid, id_to_type)?
                                .unwrap_or_else(|| "<missing>".to_string());
                            let error = e.to_string();
                            self.record_edge_validation_drop(
                                format!("edge:{edge_type}"),
                                format!(
                                    "{from}({source_type}) -[{edge_type}]-> {to}({target_type}): {error}"
                                ),
                            );
                            warn!(
                                from,
                                to,
                                edge_type,
                                source_id = %fid,
                                target_id = %tid,
                                source_type,
                                target_type,
                                error,
                                "Skipping edge that failed schema validation"
                            );
                            continue;
                        }
                        pending.push(PendingImportEdge {
                            from_name: from,
                            to_name: to,
                            edge_type: edge_type.clone(),
                            edge: Edge::new(fid, tid, EdgeType::new(edge_type)),
                        });
                    }
                    _ => {
                        self.record_edge_validation_drop(
                            "edge_endpoint_exists".to_string(),
                            format!(
                                "{from} -[{edge_type}]-> {to}: from_resolved={}, to_resolved={}",
                                from_id.is_some(),
                                to_id.is_some()
                            ),
                        );
                        error!(
                            from,
                            to,
                            edge_type,
                            from_resolved = from_id.is_some(),
                            to_resolved = to_id.is_some(),
                            constraint = "edge_endpoint_exists",
                            "Skipping edge with missing node reference"
                        );
                    }
                }
            }
        }
        let prepare_duration_ms = prepare_start.elapsed().as_millis() as u64;

        let pending_edges = pending.len();
        let persist_start = Instant::now();
        self.persist_import_edges(pending)?;
        let persist_duration_ms = persist_start.elapsed().as_millis() as u64;

        info!(
            "Created {} relationships total",
            self.stats.relationships_created
        );
        info!(
            relationships_created = self.stats.relationships_created,
            edge_records_skipped = self.stats.edge_records_skipped,
            name_cache_hits = name_resolution_cache.cache_hits,
            name_storage_lookups = name_resolution_cache.storage_lookups,
            pending_edges,
            prepare_duration_ms,
            persist_duration_ms,
            duration_ms = start.elapsed().as_millis() as u64,
            "Import relationship phase finished"
        );
        Ok(())
    }

    fn persist_import_edges(&mut self, pending: Vec<PendingImportEdge>) -> Result<()> {
        if pending.is_empty() {
            return Ok(());
        }

        let edges = pending
            .iter()
            .map(|pending| pending.edge.clone())
            .collect::<Vec<_>>();
        if self.graph.connect_edges(edges).is_ok() {
            self.stats.relationships_created += pending.len();
            return Ok(());
        }

        for pending in pending {
            match self.graph.connect_objects_str(
                pending.edge.from,
                pending.edge.to,
                &pending.edge_type,
            ) {
                Ok(()) => {
                    self.stats.relationships_created += 1;
                }
                Err(e) => {
                    let error = e.to_string();
                    self.record_edge_validation_drop(
                        format!("edge:{}:storage", pending.edge_type),
                        format!(
                            "{} -[{}]-> {}: {error}",
                            pending.from_name, pending.edge_type, pending.to_name
                        ),
                    );
                    warn!(
                        from = pending.from_name,
                        to = pending.to_name,
                        edge_type = pending.edge_type,
                        source_id = %pending.edge.from,
                        target_id = %pending.edge.to,
                        error,
                        "Skipping edge that failed storage import"
                    );
                }
            }
        }
        Ok(())
    }

    fn record_object_validation_drop(&mut self, key: String, sample: String) {
        self.stats.object_records_skipped += 1;
        self.stats.validation_errors += 1;
        self.record_diagnostic("validation", &key, &sample);
        record_diagnostic_sample(&mut self.validation_diagnostics, key, sample);
    }

    fn record_edge_validation_drop(&mut self, key: String, sample: String) {
        self.stats.edge_records_skipped += 1;
        self.stats.validation_errors += 1;
        self.record_diagnostic("validation", &key, &sample);
        record_diagnostic_sample(&mut self.validation_diagnostics, key, sample);
    }

    fn record_property_drop(&mut self, key: String, sample: String) {
        self.stats.dropped_properties += 1;
        self.record_diagnostic("dropped_property", &key, &sample);
        record_diagnostic_sample(&mut self.dropped_property_diagnostics, key, sample);
    }

    fn record_reused_object(&mut self, key: String, sample: String) {
        self.stats.objects_reused += 1;
        self.record_diagnostic("reused_object", &key, &sample);
        record_diagnostic_sample(&mut self.reused_object_diagnostics, key, sample);
    }

    fn record_diagnostic(&mut self, category: &str, constraint: &str, item: &str) {
        self.diagnostic_records.push(ImportDiagnosticRecord {
            category: category.to_string(),
            constraint: constraint.to_string(),
            item: item.to_string(),
        });
    }

    fn write_import_diagnostics(&mut self, data_file: &Path) -> Result<()> {
        if self.diagnostic_records.is_empty() {
            return Ok(());
        }

        let path = import_diagnostics_path(data_file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create import diagnostics directory: {}",
                    parent.display()
                )
            })?;
        }

        let mut output = String::new();
        for record in &self.diagnostic_records {
            output.push_str(&serde_json::to_string(record)?);
            output.push('\n');
        }

        fs::write(&path, output).with_context(|| {
            format!(
                "Failed to write import diagnostics file: {}",
                path.display()
            )
        })?;
        self.stats.diagnostics_path = Some(path);
        Ok(())
    }

    fn log_import_diagnostics(&self) {
        info!(
            nodes_parsed = self.stats.nodes_parsed,
            edges_parsed = self.stats.edges_parsed,
            objects_created = self.stats.objects_created,
            objects_reused = self.stats.objects_reused,
            relationships_created = self.stats.relationships_created,
            object_records_skipped = self.stats.object_records_skipped,
            edge_records_skipped = self.stats.edge_records_skipped,
            diagnostics_path = self
                .stats
                .diagnostics_path
                .as_ref()
                .map(|path| path.display().to_string())
                .as_deref()
                .unwrap_or("<none>"),
            "Import record summary"
        );

        if self.stats.objects_reused > 0 {
            warn!(
                total = self.stats.objects_reused,
                "Import reused-object summary"
            );
            for (constraint, summary) in &self.reused_object_diagnostics {
                warn!(
                    constraint,
                    count = summary.count,
                    reused_items = ?summary.items,
                    "Import reused objects by constraint"
                );
            }
        }

        if self.stats.validation_errors > 0 {
            warn!(
                total = self.stats.validation_errors,
                object_records_skipped = self.stats.object_records_skipped,
                edge_records_skipped = self.stats.edge_records_skipped,
                "Import validation skip summary"
            );
            for (constraint, summary) in &self.validation_diagnostics {
                warn!(
                    constraint,
                    count = summary.count,
                    dropped_items = ?summary.items,
                    "Import validation skips by constraint"
                );
            }
        }

        if self.stats.dropped_properties > 0 {
            warn!(
                total = self.stats.dropped_properties,
                "Import dropped-property summary"
            );
            for (constraint, summary) in &self.dropped_property_diagnostics {
                warn!(
                    constraint,
                    count = summary.count,
                    dropped_items = ?summary.items,
                    "Import dropped properties by constraint"
                );
            }
        }
    }

    /// Resolve a node name to an ObjectId.
    ///
    /// Checks the in-session `name_to_id` map first (fast path), then falls back to a
    /// storage name-index scan for nodes created in previous import sessions (BUG-7 fix).
    fn resolve_node_id(
        &self,
        name: &str,
        name_to_id: &HashMap<String, ObjectId>,
        name_resolution_cache: &mut NameResolutionCache,
    ) -> Option<ObjectId> {
        if let Some(&id) = name_to_id.get(name) {
            return Some(id);
        }

        if let Some(cached) = name_resolution_cache.results.get(name) {
            name_resolution_cache.cache_hits += 1;
            return *cached;
        }

        name_resolution_cache.storage_lookups += 1;
        let resolved = match self.graph.find_by_name_only(name) {
            Ok(results) if !results.is_empty() => {
                if results.len() > 1 {
                    warn!(
                        "Ambiguous node name '{}' matched {} objects; using first match",
                        name,
                        results.len()
                    );
                }
                Some(results[0].id)
            }
            Ok(_) => None,
            Err(e) => {
                warn!("Storage lookup failed for node '{}': {}", name, e);
                None
            }
        };
        name_resolution_cache
            .results
            .insert(name.to_string(), resolved);
        resolved
    }

    async fn validate_import_edge(
        &mut self,
        edge_type: &str,
        from_id: ObjectId,
        to_id: ObjectId,
        id_to_type: &HashMap<ObjectId, String>,
    ) -> Result<()> {
        let (schema_match, schema_loaded) = self.schema_for_edge_type(edge_type).await?;
        if schema_loaded && schema_match.is_none() {
            bail!("Unknown edge type '{edge_type}'. Load its schema before importing data.");
        }

        let Some((_, edge_schema)) = schema_match else {
            return Ok(());
        };

        let source_type = self
            .node_type_for_id(from_id, id_to_type)?
            .ok_or_else(|| anyhow::anyhow!("Edge source node '{from_id}' does not exist"))?;
        let target_type = self
            .node_type_for_id(to_id, id_to_type)?
            .ok_or_else(|| anyhow::anyhow!("Edge target node '{to_id}' does not exist"))?;

        if !edge_schema.allowed_source_types.is_empty()
            && !edge_schema.allowed_source_types.contains(&source_type)
        {
            bail!(
                "Edge type '{}' does not allow source type '{}'. Allowed: {:?}",
                edge_type,
                source_type,
                edge_schema.allowed_source_types
            );
        }

        if !edge_schema.allowed_target_types.is_empty()
            && !edge_schema.allowed_target_types.contains(&target_type)
        {
            bail!(
                "Edge type '{}' does not allow target type '{}'. Allowed: {:?}",
                edge_type,
                target_type,
                edge_schema.allowed_target_types
            );
        }

        Ok(())
    }

    async fn schema_for_edge_type(
        &mut self,
        edge_type: &str,
    ) -> Result<(Option<(String, EdgeTypeSchema)>, bool)> {
        let cache = self.import_schema_cache().await?;
        let schema_loaded = !cache.schemas.is_empty();
        for (schema_name, schema) in &cache.schemas {
            if let Some(edge_schema) = schema.edge_types.get(edge_type) {
                return Ok((
                    Some((schema_name.clone(), edge_schema.clone())),
                    schema_loaded,
                ));
            }
        }

        Ok((None, schema_loaded))
    }

    async fn create_object_by_type(
        &mut self,
        source_id: &str,
        node_type: &str,
        properties: &Map<String, Value>,
    ) -> Result<ObjectMetadata> {
        use crate::ObjectBuilder;

        let name = properties
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let (schema_match, schema_loaded) = self.schema_for_object_type(node_type).await?;
        if schema_loaded && schema_match.is_none() {
            bail!("Unknown object type '{node_type}'. Load its schema before importing data.");
        }
        let use_original_type = schema_match.is_some();

        let mut builder = if use_original_type {
            ObjectBuilder::custom(node_type.to_string(), name.clone())
        } else {
            match node_type {
                "location" => ObjectBuilder::location(name.clone()),
                "npc" | "player_character" => ObjectBuilder::character(name.clone()),
                "faction" => ObjectBuilder::faction(name.clone()),
                "quest" | "setting_reference" | "system_reference" | "temporal" => {
                    ObjectBuilder::event(name.clone())
                }
                "artifact" | "currency" | "inventory" | "transportation" | "skills" => {
                    ObjectBuilder::item(name.clone())
                }
                _ => ObjectBuilder::custom(node_type.to_string(), name.clone()),
            }
        };

        let schema_name = schema_match
            .as_ref()
            .map(|(schema_name, _)| schema_name.clone());
        let properties = if let Some((_, schema)) = schema_match {
            self.filter_schema_properties(node_type, &name, &schema, properties)?
        } else {
            let mut properties = properties.clone();
            // Store source id for future dedup without depending on name stability
            // when no strict schema is loaded for this import.
            properties.insert(
                "_source_id".to_string(),
                Value::String(source_id.to_string()),
            );
            properties
        };

        builder = self.add_properties_to_builder(builder, &properties);
        let mut object = builder.build();
        object.schema_name = schema_name;
        Ok(object)
    }

    async fn schema_for_object_type(
        &mut self,
        node_type: &str,
    ) -> Result<(Option<(String, ObjectTypeSchema)>, bool)> {
        let cache = self.import_schema_cache().await?;
        let schema_loaded = !cache.schemas.is_empty();
        for (schema_name, schema) in &cache.schemas {
            if let Some(object_schema) = schema.object_types.get(node_type) {
                return Ok((
                    Some((schema_name.clone(), object_schema.clone())),
                    schema_loaded,
                ));
            }
        }

        Ok((None, schema_loaded))
    }

    async fn import_schema_cache(&mut self) -> Result<&ImportSchemaCache> {
        if self.schema_cache.is_none() {
            let schema_manager = self.graph.get_schema_manager();
            let mut schema_names = schema_manager.list_schemas()?;
            schema_names.sort_by_key(|name| match name.as_str() {
                "imported_schemas" => (0, name.clone()),
                "default" => (1, name.clone()),
                _ => (2, name.clone()),
            });

            let mut schemas = Vec::with_capacity(schema_names.len());
            for schema_name in schema_names {
                let schema = schema_manager.load_schema(&schema_name).await?;
                schemas.push((schema_name, schema));
            }

            self.schema_cache = Some(ImportSchemaCache { schemas });
        }

        Ok(self
            .schema_cache
            .as_ref()
            .expect("schema_cache is populated above"))
    }

    fn node_type_for_id(
        &self,
        node_id: ObjectId,
        id_to_type: &HashMap<ObjectId, String>,
    ) -> Result<Option<String>> {
        if let Some(object_type) = id_to_type.get(&node_id) {
            return Ok(Some(object_type.clone()));
        }

        Ok(self
            .graph
            .get_object(node_id)?
            .map(|object| object.object_type))
    }

    fn filter_schema_properties(
        &mut self,
        node_type: &str,
        name: &str,
        schema: &ObjectTypeSchema,
        properties: &Map<String, Value>,
    ) -> Result<Map<String, Value>> {
        let mut filtered = Map::new();

        for (key, value) in properties {
            if key == "name" || schema.properties.contains_key(key) {
                filtered.insert(key.clone(), value.clone());
            } else {
                let value_json = compact_json_value(value);
                self.record_property_drop(
                    format!("object:{node_type}:property:{key}"),
                    format!("{name}.{key}={value_json}"),
                );
                warn!(
                    object_type = node_type,
                    object_name = name,
                    property = key,
                    value = %value_json,
                    constraint = "declared_schema_property",
                    "Dropping import property not declared in schema"
                );
            }
        }

        for required in &schema.required_properties {
            if required == "name" {
                if name.is_empty() {
                    bail!("Required property 'name' is missing for {node_type}");
                }
                continue;
            }

            if !filtered.contains_key(required)
                || filtered.get(required).is_some_and(|value| value.is_null())
            {
                bail!("Required property '{required}' is missing for {node_type} '{name}'");
            }
        }

        Ok(filtered)
    }

    fn add_properties_to_builder(
        &self,
        mut builder: crate::ObjectBuilder,
        properties: &Map<String, Value>,
    ) -> crate::ObjectBuilder {
        for (key, value) in properties {
            // "name" is already set as the object's canonical name field.
            if key == "name" {
                continue;
            }
            // All schema properties — including "description" and "tags" — are
            // stored uniformly in the properties JSON blob.
            match value {
                Value::String(s) => builder = builder.with_property(key.clone(), s.clone()),
                other => builder = builder.with_json_property(key.clone(), other.clone()),
            }
        }
        builder
    }
}

fn compact_json_object(properties: &Map<String, Value>) -> String {
    compact_json_value(&Value::Object(properties.clone()))
}

fn compact_json_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn import_diagnostics_path(data_file: &Path) -> PathBuf {
    let stem = data_file
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("import");
    let safe_stem: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    data_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".u-forge-import-diagnostics")
        .join(format!("{safe_stem}-{timestamp}.jsonl"))
}

fn record_diagnostic_sample(
    diagnostics: &mut BTreeMap<String, ImportDiagnosticSummary>,
    key: String,
    item: String,
) {
    let summary = diagnostics.entry(key).or_default();
    summary.count += 1;
    summary.items.push(item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EdgeTypeSchema, ObjectTypeSchema, PropertySchema, SchemaDefinition};
    use serde_json::json;
    use tempfile::TempDir;

    fn create_test_graph() -> (TempDir, KnowledgeGraph) {
        let temp_dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp_dir.path()).unwrap();
        (temp_dir, graph)
    }

    #[tokio::test]
    async fn test_json_parsing() {
        let json_data = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"location","properties":{"name":"Test Location","description":"A place"}}
{"entitytype":"edge","from":"Location A","to":"Location B","edgeType":"connects_to"}"#;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for line in json_data.lines() {
            if let Ok(entry) = serde_json::from_str::<JsonEntry>(line) {
                match entry {
                    JsonEntry::Node { .. } => nodes.push(entry),
                    JsonEntry::Edge { .. } => edges.push(entry),
                }
            }
        }

        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn test_properties_parsing() {
        let (_temp_dir, graph) = create_test_graph();
        let ingestion = DataIngestion::new(&graph);

        let mut props = Map::new();
        props.insert("name".to_string(), json!("Test"));
        props.insert("description".to_string(), json!("A test location"));
        props.insert("tags".to_string(), json!(["tag1", "tag2"]));
        props.insert("goals".to_string(), json!(["goal1", "goal2"]));
        props.insert("status".to_string(), json!("Active"));

        let builder = crate::ObjectBuilder::location("Test".to_string());
        let builder = ingestion.add_properties_to_builder(builder, &props);
        let object = builder.build();

        let tags = object
            .get_json_property("tags")
            .and_then(|v| v.as_array())
            .unwrap();
        assert!(tags.iter().any(|v| v.as_str() == Some("tag1")));
        assert!(tags.iter().any(|v| v.as_str() == Some("tag2")));
        assert_eq!(
            object.get_property("description").as_deref(),
            Some("A test location")
        );
        assert_eq!(
            object.properties.get("status").and_then(|v| v.as_str()),
            Some("Active")
        );
        // Array property stored as JSON
        assert!(object.properties.get("goals").is_some());
    }

    #[tokio::test]
    async fn test_import_roundtrip() {
        let (_temp_dir, graph) = create_test_graph();
        let mut ingestion = DataIngestion::new(&graph);

        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"location","properties":{"name":"Terminus","description":"A frontier world","tags":["planet","foundation"]}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"npc","properties":{"name":"Hari Seldon","role":"Mathematician","currentLocation":"Terminus"}}
{"entitytype":"edge","from":"Hari Seldon","to":"Terminus","edgeType":"located_in"}"#;

        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.nodes_parsed, 2);
        assert_eq!(stats.edges_parsed, 1);
        assert_eq!(stats.objects_created, 2);
        assert_eq!(stats.relationships_created, 1);
        assert_eq!(stats.parse_errors, 0);
        assert!(stats.diagnostics_path.is_none());
    }

    #[tokio::test]
    async fn test_schema_import_drops_unknown_properties() {
        let (_temp_dir, graph) = create_test_graph();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "npc".to_string(),
            ObjectTypeSchema::new("npc".to_string(), "NPC".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_property("role".to_string(), PropertySchema::string("role"))
                .with_required_property("name".to_string()),
        );
        graph
            .get_schema_manager()
            .save_schema(&schema)
            .await
            .unwrap();

        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon","role":"Mathematician","secret":"psychohistory"}}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.objects_created, 1);
        assert_eq!(stats.dropped_properties, 1);
        let object = graph
            .find_by_name("npc", "Hari Seldon")
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(object.schema_name.as_deref(), Some("imported_schemas"));
        assert_eq!(
            object.get_property("role").as_deref(),
            Some("Mathematician")
        );
        assert!(object.get_json_property("secret").is_none());
    }

    #[tokio::test]
    async fn test_schema_import_skips_object_missing_required_property() {
        let (_temp_dir, graph) = create_test_graph();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "npc".to_string(),
            ObjectTypeSchema::new("npc".to_string(), "NPC".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_property("role".to_string(), PropertySchema::string("role"))
                .with_required_property("name".to_string())
                .with_required_property("role".to_string()),
        );
        graph
            .get_schema_manager()
            .save_schema(&schema)
            .await
            .unwrap();

        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon"}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"npc","properties":{"name":"Salvor Hardin","role":"Mayor"}}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.objects_created, 1);
        assert_eq!(stats.validation_errors, 1);
        assert_eq!(stats.object_records_skipped, 1);
        assert_eq!(stats.edge_records_skipped, 0);
        assert!(graph.find_by_name("npc", "Hari Seldon").unwrap().is_empty());
        assert_eq!(graph.get_stats().unwrap().node_count, 1);
    }

    #[tokio::test]
    async fn test_schema_import_skips_unknown_node_type() {
        let (_temp_dir, graph) = create_test_graph();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "location".to_string(),
            ObjectTypeSchema::new("location".to_string(), "Location".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_required_property("name".to_string()),
        );
        graph
            .get_schema_manager()
            .save_schema(&schema)
            .await
            .unwrap();

        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon"}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"location","properties":{"name":"Terminus"}}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.objects_created, 1);
        assert_eq!(stats.validation_errors, 1);
        assert_eq!(stats.object_records_skipped, 1);
        assert_eq!(stats.edge_records_skipped, 0);
        assert!(graph.find_by_name("npc", "Hari Seldon").unwrap().is_empty());
        assert_eq!(graph.get_stats().unwrap().node_count, 1);
    }

    #[tokio::test]
    async fn test_schema_import_skips_unknown_edge_type() {
        let (_temp_dir, graph) = create_test_graph();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "npc".to_string(),
            ObjectTypeSchema::new("npc".to_string(), "NPC".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_required_property("name".to_string()),
        );
        schema.add_edge_type(
            "knows".to_string(),
            EdgeTypeSchema::new("knows".to_string(), "knows".to_string())
                .with_source_types(vec!["npc".to_string()])
                .with_target_types(vec!["npc".to_string()]),
        );
        graph
            .get_schema_manager()
            .save_schema(&schema)
            .await
            .unwrap();

        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon"}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"npc","properties":{"name":"Salvor Hardin"}}
{"entitytype":"edge","from":"Hari Seldon","to":"Salvor Hardin","edgeType":"invented"}
{"entitytype":"edge","from":"Hari Seldon","to":"Salvor Hardin","edgeType":"knows"}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.relationships_created, 1);
        assert_eq!(stats.validation_errors, 1);
        assert_eq!(stats.object_records_skipped, 0);
        assert_eq!(stats.edge_records_skipped, 1);
        assert_eq!(graph.get_stats().unwrap().edge_count, 1);
    }

    #[tokio::test]
    async fn test_schema_import_skips_disallowed_edge_endpoint() {
        let (_temp_dir, graph) = create_test_graph();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "npc".to_string(),
            ObjectTypeSchema::new("npc".to_string(), "NPC".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_required_property("name".to_string()),
        );
        schema.add_object_type(
            "location".to_string(),
            ObjectTypeSchema::new("location".to_string(), "Location".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_required_property("name".to_string()),
        );
        schema.add_edge_type(
            "located_in".to_string(),
            EdgeTypeSchema::new("located_in".to_string(), "location".to_string())
                .with_source_types(vec!["npc".to_string()])
                .with_target_types(vec!["location".to_string()]),
        );
        graph
            .get_schema_manager()
            .save_schema(&schema)
            .await
            .unwrap();

        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon"}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"location","properties":{"name":"Terminus"}}
{"entitytype":"edge","from":"Terminus","to":"Hari Seldon","edgeType":"located_in"}
{"entitytype":"edge","from":"Hari Seldon","to":"Terminus","edgeType":"located_in"}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.relationships_created, 1);
        assert_eq!(stats.validation_errors, 1);
        assert_eq!(stats.object_records_skipped, 0);
        assert_eq!(stats.edge_records_skipped, 1);
        assert_eq!(graph.get_stats().unwrap().edge_count, 1);
    }

    #[tokio::test]
    async fn test_import_counts_missing_endpoint_edge_as_skipped() {
        let (_temp_dir, graph) = create_test_graph();
        let mut ingestion = DataIngestion::new(&graph);
        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"npc","properties":{"name":"Hari Seldon"}}
{"entitytype":"edge","from":"Hari Seldon","to":"Missing Planet","edgeType":"located_in"}"#;
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.nodes_parsed, 1);
        assert_eq!(stats.edges_parsed, 1);
        assert_eq!(stats.objects_created, 1);
        assert_eq!(stats.relationships_created, 0);
        assert_eq!(stats.validation_errors, 1);
        assert_eq!(stats.object_records_skipped, 0);
        assert_eq!(stats.edge_records_skipped, 1);
        let diagnostics_path = stats.diagnostics_path.as_ref().unwrap();
        let diagnostics = std::fs::read_to_string(diagnostics_path).unwrap();
        assert!(diagnostics.contains("\"category\":\"validation\""));
        assert!(diagnostics.contains("\"constraint\":\"edge_endpoint_exists\""));
        assert!(diagnostics.contains("Missing Planet"));
    }
}
