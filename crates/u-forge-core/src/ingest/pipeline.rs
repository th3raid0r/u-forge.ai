//! High-level ingestion pipeline.
//!
//! [`setup_and_index`] is the canonical way to bootstrap a [`KnowledgeGraph`]
//! with schemas, data, and FTS5 text chunks in a single call.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

use crate::KnowledgeGraph;
use crate::ingest::DataIngestion;
use crate::schema::SchemaIngestion;
use crate::types::{ChunkType, ObjectId};

/// Outcome of a [`setup_and_index`] call.
#[derive(Debug)]
pub struct SetupResult {
    /// `true` when data was actually imported (vs. loaded from an existing DB).
    pub fresh_import: bool,
    /// Number of objects created during import (0 when `fresh_import` is false).
    pub objects_created: usize,
    /// Number of parsed node records that matched existing objects and were reused.
    pub objects_reused: usize,
    /// Number of relationships created during import.
    pub relationships_created: usize,
    /// Number of node records skipped during import.
    pub object_records_skipped: usize,
    /// Number of edge records skipped during import.
    pub edge_records_skipped: usize,
    /// Number of JSON properties dropped because they were not declared in the loaded schema.
    pub dropped_properties: usize,
    /// Number of import records skipped because they failed loaded-schema validation.
    pub validation_errors: usize,
    /// JSONL file containing all skipped, reused, and dropped import diagnostics.
    pub diagnostics_path: Option<PathBuf>,
    /// Number of FTS5 text chunks indexed.
    pub chunks_indexed: usize,
}

/// Import data and index for FTS5 — schema loading is intentionally omitted.
///
/// Use this when schemas are already present. Unlike [`setup_and_index`] this
/// always runs (no `node_count > 0` guard) so the caller controls whether to
/// clear first.
pub async fn import_data_only<P: AsRef<Path>>(
    graph: &KnowledgeGraph,
    data_file: P,
) -> Result<SetupResult> {
    let data_file = data_file.as_ref();
    info!(
        data_file = %data_file.display(),
        "Importing data (schema-independent)"
    );
    import_loaded_data(graph, data_file).await
}

/// Load schemas, import data, and index for FTS5 without the populated-graph guard.
///
/// Use this for user-initiated imports where the selected schema directory is
/// authoritative and must be loaded before validating the data file.
pub async fn import_schemas_and_data(
    graph: &KnowledgeGraph,
    schema_dir: &Path,
    data_file: &Path,
) -> Result<SetupResult> {
    load_schemas_into_graph(graph, schema_dir, true).await?;
    import_loaded_data(graph, data_file).await
}

async fn import_loaded_data<P: AsRef<Path>>(
    graph: &KnowledgeGraph,
    data_file: P,
) -> Result<SetupResult> {
    let data_file = data_file.as_ref();
    info!(data_file = %data_file.display(), "Importing data");
    let import_start = Instant::now();
    let mut ingestion = DataIngestion::new(graph);
    ingestion.import_json_data(data_file).await?;
    let import_duration_ms = import_start.elapsed().as_millis() as u64;
    let stats = ingestion.get_stats();
    let objects_created = stats.objects_created;
    let objects_reused = stats.objects_reused;
    let relationships_created = stats.relationships_created;
    let object_records_skipped = stats.object_records_skipped;
    let edge_records_skipped = stats.edge_records_skipped;
    let dropped_properties = stats.dropped_properties;
    let validation_errors = stats.validation_errors;
    let diagnostics_path = stats.diagnostics_path.clone();
    info!(
        objects_created,
        objects_reused,
        relationships_created,
        object_records_skipped,
        edge_records_skipped,
        dropped_properties,
        validation_errors,
        diagnostics_path = diagnostics_path
            .as_ref()
            .map(|path| path.display().to_string())
            .as_deref()
            .unwrap_or("<none>"),
        duration_ms = import_duration_ms,
        "Data imported"
    );

    info!("Indexing text for full-text search");
    let index_start = Instant::now();
    let load_objects_start = Instant::now();
    let all_objects = graph.get_all_objects()?;
    let load_objects_duration_ms = load_objects_start.elapsed().as_millis() as u64;

    let edge_context_start = Instant::now();
    let id_to_name: HashMap<ObjectId, String> =
        all_objects.iter().map(|o| (o.id, o.name.clone())).collect();
    let mut edges_by_object: HashMap<ObjectId, Vec<String>> = HashMap::new();
    let all_edges = graph.get_all_edges()?;
    let all_edges_count = all_edges.len();
    for edge in all_edges {
        let Some(from_name) = id_to_name.get(&edge.from) else {
            continue;
        };
        let Some(to_name) = id_to_name.get(&edge.to) else {
            continue;
        };
        let line = format!("{} {} {}", from_name, edge.edge_type.as_str(), to_name);
        edges_by_object
            .entry(edge.from)
            .or_default()
            .push(line.clone());
        edges_by_object.entry(edge.to).or_default().push(line);
    }
    let edge_context_duration_ms = edge_context_start.elapsed().as_millis() as u64;

    let flatten_start = Instant::now();
    let mut chunks_indexed = 0usize;
    let mut chunk_inputs = Vec::with_capacity(all_objects.len());
    for obj in &all_objects {
        let edge_lines = edges_by_object.remove(&obj.id).unwrap_or_default();
        let text = obj.flatten_for_embedding(&edge_lines);
        chunk_inputs.push((obj.id, text, ChunkType::Imported));
    }
    let flatten_duration_ms = flatten_start.elapsed().as_millis() as u64;

    let chunk_write_start = Instant::now();
    chunks_indexed += graph.add_text_chunks(chunk_inputs)?.len();
    let chunk_write_duration_ms = chunk_write_start.elapsed().as_millis() as u64;
    let index_duration_ms = index_start.elapsed().as_millis() as u64;
    info!(
        chunks_indexed,
        objects_indexed = all_objects.len(),
        edges_indexed = all_edges_count,
        load_objects_duration_ms,
        edge_context_duration_ms,
        flatten_duration_ms,
        chunk_write_duration_ms,
        duration_ms = index_duration_ms,
        "FTS indexing complete"
    );

    Ok(SetupResult {
        fresh_import: true,
        objects_created,
        objects_reused,
        relationships_created,
        object_records_skipped,
        edge_records_skipped,
        dropped_properties,
        validation_errors,
        diagnostics_path,
        chunks_indexed,
    })
}

/// Load schemas, import data, and index all objects for FTS5 full-text search.
///
/// The caller is responsible for opening the [`KnowledgeGraph`] and calling
/// [`KnowledgeGraph::clear_all`] beforehand if a fresh start is desired.
///
/// If the graph already contains data (`node_count > 0`), the import is skipped
/// and `SetupResult::fresh_import` is `false`.
///
/// Schema load failures are logged as warnings and do not abort the pipeline.
/// Data import failures propagate as errors.
pub async fn setup_and_index(
    graph: &KnowledgeGraph,
    schema_dir: &str,
    data_file: &str,
) -> Result<SetupResult> {
    let pre_stats = graph.get_stats()?;
    if pre_stats.node_count > 0 {
        info!(
            nodes = pre_stats.node_count,
            chunks = pre_stats.chunk_count,
            "Graph already populated — skipping import"
        );
        return Ok(SetupResult {
            fresh_import: false,
            objects_created: 0,
            objects_reused: 0,
            relationships_created: 0,
            object_records_skipped: 0,
            edge_records_skipped: 0,
            dropped_properties: 0,
            validation_errors: 0,
            diagnostics_path: None,
            chunks_indexed: 0,
        });
    }

    // ── Schemas ──────────────────────────────────────────────────────────────

    load_schemas_into_graph(graph, Path::new(schema_dir), false).await?;

    // ── Data import ─────────────────────────────────────────────────────────

    import_loaded_data(graph, data_file).await
}

async fn load_schemas_into_graph(
    graph: &KnowledgeGraph,
    schema_dir: &Path,
    require_success: bool,
) -> Result<()> {
    info!(schema_dir = %schema_dir.display(), "Loading schemas");
    match SchemaIngestion::load_schemas_from_directory(schema_dir, "imported_schemas", "1.0.0") {
        Ok(schema_def) => {
            let mgr = graph.get_schema_manager();
            // Remove the hardcoded "default" placeholder (character, location...)
            // before saving the real imported schema set.
            let _ = mgr.delete_schema("default");
            match mgr.save_schema(&schema_def).await {
                Ok(()) => {
                    info!(count = schema_def.object_types.len(), "Schema types loaded");
                }
                Err(e) if require_success => return Err(e),
                Err(e) => warn!(%e, "Could not save schemas"),
            }
        }
        Err(e) if require_success => return Err(e),
        Err(e) => warn!(%e, schema_dir = %schema_dir.display(), "Could not load schemas"),
    }
    Ok(())
}
