//! High-level ingestion pipeline.
//!
//! [`setup_and_index`] is the canonical way to bootstrap a [`KnowledgeGraph`]
//! with schemas, data, and FTS5 text chunks in a single call.

use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::ingest::DataIngestion;
use crate::schema::SchemaIngestion;
use crate::types::{ChunkType, ObjectId};
use crate::KnowledgeGraph;

/// Outcome of a [`setup_and_index`] call.
#[derive(Debug)]
pub struct SetupResult {
    /// `true` when data was actually imported (vs. loaded from an existing DB).
    pub fresh_import: bool,
    /// Number of objects created during import (0 when `fresh_import` is false).
    pub objects_created: usize,
    /// Number of relationships created during import.
    pub relationships_created: usize,
    /// Number of FTS5 text chunks indexed.
    pub chunks_indexed: usize,
}

/// Import data and index for FTS5 — schema loading is intentionally omitted.
///
/// Use this when schemas are already present. Unlike [`setup_and_index`] this
/// always runs (no `node_count > 0` guard) so the caller controls whether to
/// clear first.
pub async fn import_data_only(graph: &KnowledgeGraph, data_file: &str) -> Result<SetupResult> {
    info!(data_file, "Importing data (schema-independent)");
    let mut ingestion = DataIngestion::new(graph);
    ingestion.import_json_data(data_file).await?;
    let stats = ingestion.get_stats();
    let objects_created = stats.objects_created;
    let relationships_created = stats.relationships_created;
    info!(objects_created, relationships_created, "Data imported");

    info!("Indexing text for full-text search");
    let all_objects = graph.get_all_objects()?;
    let id_to_name: HashMap<ObjectId, String> =
        all_objects.iter().map(|o| (o.id, o.name.clone())).collect();
    let mut chunks_indexed = 0usize;
    for obj in &all_objects {
        let edges = graph.get_relationships(obj.id).unwrap_or_default();
        let edge_lines: Vec<String> = edges
            .iter()
            .filter_map(|e| {
                let from_name = id_to_name.get(&e.from)?;
                let to_name = id_to_name.get(&e.to)?;
                Some(format!("{} {} {}", from_name, e.edge_type.as_str(), to_name))
            })
            .collect();
        let text = obj.flatten_for_embedding(&edge_lines);
        chunks_indexed += graph
            .add_text_chunk(obj.id, text, ChunkType::Imported)?
            .len();
    }
    info!(chunks_indexed, "FTS5 indexing complete");

    Ok(SetupResult {
        fresh_import: true,
        objects_created,
        relationships_created,
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
            relationships_created: 0,
            chunks_indexed: 0,
        });
    }

    // ── Schemas ──────────────────────────────────────────────────────────────

    info!(schema_dir, "Loading schemas");
    match SchemaIngestion::load_schemas_from_directory(schema_dir, "imported_schemas", "1.0.0") {
        Ok(schema_def) => {
            let mgr = graph.get_schema_manager();
            match mgr.save_schema(&schema_def).await {
                Ok(()) => {
                    info!(count = schema_def.object_types.len(), "Schema types loaded");
                    // Remove the hardcoded "default" placeholder (character, location…)
                    // so it doesn't pollute the agent's schema summary alongside the
                    // real imported types (npc, player_character…).
                    let _ = mgr.delete_schema("default");
                }
                Err(e) => warn!(%e, "Could not save schemas"),
            }
        }
        Err(e) => warn!(%e, schema_dir, "Could not load schemas"),
    }

    // ── Data import ─────────────────────────────────────────────────────────

    info!(data_file, "Importing data");
    let mut ingestion = DataIngestion::new(graph);
    ingestion.import_json_data(data_file).await?;
    let stats = ingestion.get_stats();
    let objects_created = stats.objects_created;
    let relationships_created = stats.relationships_created;
    info!(objects_created, relationships_created, "Data imported");

    // ── FTS5 text indexing ───────────────────────────────────────────────────
    //
    // Flatten every object (plus its edge labels) into a single text chunk
    // suitable for full-text search.  We build a name lookup map up-front so
    // edge endpoint resolution is O(1) per edge rather than O(N) get_object
    // calls.

    info!("Indexing text for full-text search");
    let all_objects = graph.get_all_objects()?;
    let id_to_name: HashMap<ObjectId, String> =
        all_objects.iter().map(|o| (o.id, o.name.clone())).collect();
    let mut chunks_indexed = 0usize;

    for obj in &all_objects {
        let edges = graph.get_relationships(obj.id).unwrap_or_default();
        let edge_lines: Vec<String> = edges
            .iter()
            .filter_map(|e| {
                let from_name = id_to_name.get(&e.from)?;
                let to_name = id_to_name.get(&e.to)?;
                Some(format!("{} {} {}", from_name, e.edge_type.as_str(), to_name))
            })
            .collect();
        let text = obj.flatten_for_embedding(&edge_lines);
        chunks_indexed += graph
            .add_text_chunk(obj.id, text, ChunkType::Imported)?
            .len();
    }
    info!(chunks_indexed, "FTS5 indexing complete");

    Ok(SetupResult {
        fresh_import: true,
        objects_created,
        relationships_created,
        chunks_indexed,
    })
}
