//! Shared helpers for u-forge.ai CLI examples.
//!
//! Included via `#[path = "common/mod.rs"] mod common;` in each example.
//! Contains the orchestration glue that both `cli_demo` and `cli_chat` repeat:
//! config loading, argument resolution, knowledge graph setup + data ingestion,
//! HQ embedding queue construction, and chunk embedding.

use anyhow::Result;
use std::io::Write as _;
use std::sync::Arc;
use u_forge_core::{
    ai::embeddings::LemonadeProvider,
    config::AppConfig,
    ingest::DataIngestion,
    lemonade::LemonadeModelRegistry,
    queue::{InferenceQueue, InferenceQueueBuilder},
    ChunkType, EmbeddingProvider, KnowledgeGraph, SchemaIngestion, HIGH_QUALITY_EMBEDDING_DIMENSIONS,
};

// ── Config ────────────────────────────────────────────────────────────────────

/// Database path + clear-on-start flag shared by both demo config files.
#[derive(serde::Deserialize, Default)]
pub struct DatabaseConfig {
    /// Path to the persistent database directory.  Relative paths are resolved
    /// from the current working directory.  Defaults to `./demo_data/kg`.
    pub path: Option<String>,
    /// When `true`, clear all data from the database before loading.
    #[serde(default)]
    pub clear: bool,
}

/// Load a TOML file into any `serde::Deserialize` type.
///
/// Both examples define their own top-level config struct (with different
/// optional sections) but share the same file-read + parse logic.
pub fn load_toml_config<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read config file '{path}': {e}"))?;
    toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Could not parse config file '{path}': {e}"))
}

// ── Argument parsing ──────────────────────────────────────────────────────────

/// Resolved CLI arguments common to both demo examples.
pub struct DemoArgs {
    /// Path to the JSONL data file.
    pub data_file: String,
    /// Path to the schema directory.
    pub schema_dir: String,
    /// Path to the TOML demo config file, if one was found.
    pub config_path: Option<String>,
    /// True when `--help` / `-h` was passed.
    pub help_requested: bool,
}

/// Resolve the standard argument set used by both demo examples.
///
/// Priority for each value (first wins):
/// 1. Positional CLI argument
/// 2. Environment variable (`UFORGE_DATA_FILE` / `UFORGE_SCHEMA_DIR`)
/// 3. Path relative to `CARGO_MANIFEST_DIR` (compile-time default)
///
/// Config path priority:
/// 1. `--config <path>` flag
/// 2. `UFORGE_DEMO_CONFIG` env var
/// 3. `defaults/demo_config.toml` next to the crate manifest (if it exists)
pub fn resolve_demo_args() -> DemoArgs {
    let args: Vec<String> = std::env::args().collect();

    let help_requested = args.iter().any(|a| a == "--help" || a == "-h");

    let data_file = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("UFORGE_DATA_FILE").ok())
        .unwrap_or_else(|| {
            format!(
                "{}/../../defaults/data/memory.json",
                env!("CARGO_MANIFEST_DIR")
            )
        });

    let schema_dir = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("UFORGE_SCHEMA_DIR").ok())
        .unwrap_or_else(|| format!("{}/../../defaults/schemas", env!("CARGO_MANIFEST_DIR")));

    let default_config_path = format!(
        "{}/../../defaults/demo_config.toml",
        env!("CARGO_MANIFEST_DIR")
    );

    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| w[1].clone())
        .or_else(|| std::env::var("UFORGE_DEMO_CONFIG").ok())
        .or_else(|| {
            if std::path::Path::new(&default_config_path).exists() {
                Some(default_config_path)
            } else {
                None
            }
        });

    DemoArgs {
        data_file,
        schema_dir,
        config_path,
        help_requested,
    }
}

// ── Knowledge graph setup + ingestion ────────────────────────────────────────

/// Open the knowledge graph, optionally clear it, load schemas and data,
/// and index all text chunks for FTS5.
///
/// Returns `(graph, fresh_import)` where `fresh_import` is `true` when data
/// was actually imported (vs. loaded from an existing database on disk).
///
/// Prints progress to stdout. Errors during schema/data loading are printed
/// as warnings rather than propagated — the graph is still returned so callers
/// can degrade gracefully.
pub async fn setup_knowledge_graph(
    db_path: &str,
    clear: bool,
    schema_dir: &str,
    data_file: &str,
) -> Result<(KnowledgeGraph, bool)> {
    println!("   Opening knowledge graph at {db_path}…");
    let graph = KnowledgeGraph::new(db_path)?;

    if clear {
        println!("   Clearing existing data…");
        graph.clear_all()?;
    }

    let pre_stats = graph.get_stats()?;
    let data_already_loaded = pre_stats.node_count > 0;

    if data_already_loaded && !clear {
        println!(
            "   ✅ Loaded from disk ({} nodes, {} chunks)\n",
            pre_stats.node_count, pre_stats.chunk_count
        );
        return Ok((graph, false));
    }

    // Load schemas.
    println!("   Loading schemas from {schema_dir}…");
    match SchemaIngestion::load_schemas_from_directory(schema_dir, "imported_schemas", "1.0.0") {
        Ok(schema_def) => {
            let mgr = graph.get_schema_manager();
            match mgr.save_schema(&schema_def).await {
                Ok(()) => println!(
                    "   ✅ {} schema types loaded",
                    schema_def.object_types.len()
                ),
                Err(e) => eprintln!("   ⚠️  Could not save schemas: {e}"),
            }
        }
        Err(e) => eprintln!("   ⚠️  Could not load schemas from {schema_dir}: {e}"),
    }

    // Import data.
    println!("   Importing data from {data_file}…");
    let mut ingestion = DataIngestion::new(&graph);
    if let Err(e) = ingestion.import_json_data(data_file).await {
        eprintln!("   ❌ Import failed: {e}");
        return Err(e);
    }
    let stats = ingestion.get_stats();
    println!(
        "   ✅ {} objects, {} edges imported",
        stats.objects_created, stats.relationships_created
    );

    // Index text chunks for FTS5.
    println!("   Indexing text for full-text search…");
    let all_objects = graph.get_all_objects()?;
    let id_to_name: std::collections::HashMap<u_forge_core::types::ObjectId, String> =
        all_objects.iter().map(|o| (o.id, o.name.clone())).collect();
    let mut indexed = 0usize;

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
        indexed += graph
            .add_text_chunk(obj.id, text, ChunkType::Imported)?
            .len();
    }
    println!("   ✅ {indexed} text chunks indexed\n");

    Ok((graph, true))
}

// ── HQ embedding queue ────────────────────────────────────────────────────────

/// Build a single-worker InferenceQueue for the high-quality (4096-dim) embedding
/// model, if the registry advertises one and HQ embedding is enabled in config.
///
/// Returns `None` when HQ embedding is disabled, no suitable model is registered,
/// the model fails to load, or the model's dimensions don't match
/// `HIGH_QUALITY_EMBEDDING_DIMENSIONS`.
pub async fn build_hq_embed_queue(
    registry: &LemonadeModelRegistry,
    app_cfg: &AppConfig,
) -> Option<InferenceQueue> {
    if !app_cfg.embedding.high_quality_embedding {
        return None;
    }
    let hq_model = registry.hq_embedding_model(true)?;
    let hq_model_id = hq_model.id.clone();
    let hq_load_opts = app_cfg.models.load_options_for(&hq_model_id);

    print!("   HQ embed  : loading {hq_model_id}…");
    std::io::stdout().flush().ok();

    let provider = match LemonadeProvider::new_with_load(
        &registry.base_url,
        &hq_model_id,
        &hq_load_opts,
    )
    .await
    {
        Err(e) => {
            println!(" ⚠️  {e}");
            return None;
        }
        Ok(p) => p,
    };

    let dims = provider.dimensions().unwrap_or(0);
    if dims != HIGH_QUALITY_EMBEDDING_DIMENSIONS {
        println!(
            " ⚠️  {dims}-dim ≠ {HIGH_QUALITY_EMBEDDING_DIMENSIONS}-dim, skipped"
        );
        return None;
    }

    println!(" ✅ ({dims}-dim)");
    Some(
        InferenceQueueBuilder::new()
            .with_config(app_cfg.clone())
            .with_embedding_provider_weighted(
                Arc::new(provider),
                format!("hq({hq_model_id})"),
                app_cfg.embedding.gpu_weight,
            )
            .build(),
    )
}

// ── Chunk embedding ───────────────────────────────────────────────────────────

/// Embed all un-embedded chunks in the graph using the given queue.
///
/// Skips silently when the queue has no embedding worker or all chunks are
/// already embedded. Errors during individual upserts are counted and reported
/// but do not abort the run.
pub async fn embed_all_chunks(graph: &KnowledgeGraph, queue: &InferenceQueue) -> Result<()> {
    let stats = graph.get_stats()?;
    let needs_embedding = stats.chunk_count > stats.embedded_count;

    if !queue.has_embedding() || !needs_embedding {
        if queue.has_embedding() && !needs_embedding {
            println!(
                "   ℹ️  Embedding skipped — all {} chunks already embedded.\n",
                stats.chunk_count
            );
        }
        return Ok(());
    }

    println!("   Embedding chunks for semantic search…");
    let chunks_to_embed = graph.get_all_objects().and_then(|objs| {
        let mut all = Vec::new();
        for obj in &objs {
            for chunk in graph.get_text_chunks(obj.id)? {
                all.push(chunk);
            }
        }
        Ok(all)
    })?;

    let texts: Vec<String> = chunks_to_embed.iter().map(|c| c.content.clone()).collect();
    match queue.embed_many(texts).await {
        Err(e) => eprintln!("   ⚠️  Embedding failed: {e}"),
        Ok(vecs) => {
            let mut stored = 0usize;
            for (chunk, vec) in chunks_to_embed.iter().zip(vecs.iter()) {
                if graph.upsert_chunk_embedding(chunk.id, vec).is_ok() {
                    stored += 1;
                }
            }
            println!(
                "   ✅ {stored}/{} chunks embedded\n",
                chunks_to_embed.len()
            );
        }
    }
    Ok(())
}
