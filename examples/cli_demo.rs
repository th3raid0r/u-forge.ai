//! u-forge.ai — CLI demo
//!
//! Loads the Foundation universe sample data and schemas, then demonstrates
//! graph queries and FTS5 full-text search.  No embedding model or Lemonade
//! Server is required to run this demo.
//!
//! Usage:
//!   cargo run --example cli_demo
//!   cargo run --example cli_demo [DATA_FILE] [SCHEMA_DIR]

use anyhow::Result;
use std::env;
use u_forge_ai::{
    data_ingestion::DataIngestion, ChunkType, KnowledgeGraph, ObjectBuilder, SchemaIngestion,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise structured logging (RUST_LOG controls verbosity)
    tracing_subscriber::fmt::init();

    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage(&args[0]);
        return Ok(());
    }

    let data_file = args
        .get(1)
        .cloned()
        .or_else(|| env::var("UFORGE_DATA_FILE").ok())
        .unwrap_or_else(|| "./defaults/data/memory.json".to_string());

    let schema_dir = args
        .get(2)
        .cloned()
        .or_else(|| env::var("UFORGE_SCHEMA_DIR").ok())
        .unwrap_or_else(|| "./defaults/schemas".to_string());

    println!("🌟 u-forge.ai — Universe Forge 🌟");
    println!("   Data   : {data_file}");
    println!("   Schemas: {schema_dir}");
    println!("   Storage: SQLite (bundled, no system libs required)\n");

    // ── Database ──────────────────────────────────────────────────────────────

    let temp_dir = tempfile::TempDir::new()?;
    let db_path = temp_dir.path().join("kg");
    std::fs::create_dir_all(&db_path)?;

    println!("🗄️  Opening knowledge graph…");
    let graph = KnowledgeGraph::new(&db_path)?;
    println!("    ✅ Ready\n");

    // ── Schemas ───────────────────────────────────────────────────────────────

    println!("📚 Loading schemas from {schema_dir}");
    match SchemaIngestion::load_schemas_from_directory(&schema_dir, "imported_schemas", "1.0.0") {
        Ok(schema_def) => {
            let mgr = graph.get_schema_manager();
            match mgr.save_schema(&schema_def).await {
                Ok(()) => {
                    println!(
                        "    ✅ Loaded {} object types:",
                        schema_def.object_types.len()
                    );
                    let mut names: Vec<_> = schema_def.object_types.keys().collect();
                    names.sort();
                    for name in names {
                        println!("       • {name}");
                    }
                }
                Err(e) => eprintln!("    ⚠️  Could not save schemas: {e}"),
            }
        }
        Err(e) => eprintln!("    ⚠️  Could not load schemas from {schema_dir}: {e}"),
    }
    println!();

    // ── Data import ───────────────────────────────────────────────────────────

    println!("📄 Importing data from {data_file}");
    let mut ingestion = DataIngestion::new(&graph);
    if let Err(e) = ingestion.import_json_data(&data_file).await {
        eprintln!("    ❌ Import failed: {e}");
        return Err(e);
    }

    let stats = ingestion.get_stats();
    println!("    ✅ Objects   : {}", stats.objects_created);
    println!("    ✅ Edges     : {}", stats.relationships_created);
    if stats.parse_errors > 0 {
        println!("    ⚠️  Parse errors: {}", stats.parse_errors);
    }
    println!();

    // ── Index text chunks for FTS5 ────────────────────────────────────────────
    // Walk every imported object and add its name + description as a searchable
    // chunk so that the FTS demo below returns useful results.

    println!("🔍 Indexing text for full-text search…");
    let all_objects = graph.get_all_objects()?;
    let mut indexed = 0usize;

    for obj in &all_objects {
        // Name
        graph.add_text_chunk(obj.id, obj.name.clone(), ChunkType::Description)?;
        indexed += 1;

        // Description (if present)
        if let Some(desc) = &obj.description {
            if !desc.is_empty() {
                graph.add_text_chunk(obj.id, desc.clone(), ChunkType::Description)?;
                indexed += 1;
            }
        }

        // String-valued properties
        if let Some(props) = obj.properties.as_object() {
            for (_key, val) in props {
                if let Some(s) = val.as_str() {
                    if !s.is_empty() {
                        graph.add_text_chunk(obj.id, s.to_string(), ChunkType::Imported)?;
                        indexed += 1;
                    }
                }
            }
        }
    }

    println!("    ✅ {indexed} text chunks indexed\n");

    // ── Graph statistics ──────────────────────────────────────────────────────

    let gs = graph.get_stats()?;
    println!("📊 Graph statistics");
    println!("   Nodes   : {}", gs.node_count);
    println!("   Edges   : {}", gs.edge_count);
    println!("   Chunks  : {}", gs.chunk_count);
    println!("   Tokens  : {}", gs.total_tokens);
    println!();

    // ── FTS5 search demo ──────────────────────────────────────────────────────

    println!("🔎 Full-text search demos (SQLite FTS5)");
    println!("========================================\n");

    let queries = [
        "empire",
        "foundation",
        "terminus",
        "psychohistory",
        "robot",
        "galaxy",
    ];

    for query in &queries {
        println!("  Query: \"{query}\"");
        let results = graph.search_chunks_fts(query, 3)?;
        if results.is_empty() {
            println!("    (no matches)\n");
            continue;
        }
        for (i, (_chunk_id, obj_id, snippet)) in results.iter().enumerate() {
            let label = graph
                .get_object(*obj_id)?
                .map(|o| format!("{} [{}]", o.name, o.object_type))
                .unwrap_or_else(|| obj_id.to_string());
            let preview = if snippet.len() > 80 {
                format!("{}…", &snippet[..77])
            } else {
                snippet.clone()
            };
            println!("    {}. {} — \"{}\"", i + 1, label, preview);
        }
        println!();
    }

    // ── Relationship exploration ───────────────────────────────────────────────

    // Find the first character and explore their neighbourhood.
    let sample = all_objects.iter().find(|o| {
        o.object_type == "npc"
            || o.object_type == "character"
            || o.object_type == "player_character"
    });

    if let Some(character) = sample {
        println!("👥 Exploring connections for '{}'", character.name);
        println!("   Type: {}", character.object_type);

        let neighbours = graph.get_neighbors(character.id)?;
        println!("   Direct connections: {}", neighbours.len());

        let edges = graph.get_relationships(character.id)?;
        println!("   Relationships ({} total):", edges.len());
        for edge in edges.iter().take(8) {
            let from_name = graph
                .get_object(edge.from)?
                .map(|o| o.name)
                .unwrap_or_else(|| "?".to_string());
            let to_name = graph
                .get_object(edge.to)?
                .map(|o| o.name)
                .unwrap_or_else(|| "?".to_string());
            println!(
                "      {} --[{}]--> {}",
                from_name,
                edge.edge_type.as_str(),
                to_name
            );
        }
        if edges.len() > 8 {
            println!("      … and {} more", edges.len() - 8);
        }
        println!();

        // 2-hop subgraph
        let subgraph = graph.query_subgraph(character.id, 2)?;
        println!(
            "   2-hop subgraph: {} objects, {} edges, {} chunks",
            subgraph.objects.len(),
            subgraph.edges.len(),
            subgraph.chunks.len(),
        );
        println!();
    }

    // ── ObjectBuilder demo ────────────────────────────────────────────────────

    println!("🛠️  ObjectBuilder demo");
    let custom_id = ObjectBuilder::character("Hari Seldon".to_string())
        .with_description("Mathematician and founder of psychohistory.".to_string())
        .with_property("affiliation".to_string(), "Galactic Empire".to_string())
        .with_tag("mathematician".to_string())
        .with_tag("founder".to_string())
        .add_to_graph(&graph)?;

    let retrieved = graph.get_object(custom_id)?.unwrap();
    println!(
        "   Created : {} [{}]",
        retrieved.name, retrieved.object_type
    );
    println!("   Tags    : {}", retrieved.tags.join(", "));
    println!(
        "   Property: affiliation = {}",
        retrieved.get_property("affiliation").unwrap_or_default()
    );
    println!();

    println!("✨ Demo complete.");
    println!("   Storage: SQLite — no RocksDB, no FastEmbed, no gcc-13 required.");
    println!("   Embeddings: connect Lemonade Server (set LEMONADE_URL) for semantic search.");

    Ok(())
}

fn print_usage(prog: &str) {
    println!("u-forge.ai CLI Demo");
    println!();
    println!("Usage:");
    println!("  {prog} [DATA_FILE] [SCHEMA_DIR]");
    println!();
    println!("Arguments:");
    println!("  DATA_FILE   JSONL data file  (default: ./defaults/data/memory.json)");
    println!("  SCHEMA_DIR  schema directory (default: ./defaults/schemas)");
    println!();
    println!("Environment:");
    println!("  UFORGE_DATA_FILE   override DATA_FILE");
    println!("  UFORGE_SCHEMA_DIR  override SCHEMA_DIR");
    println!("  LEMONADE_URL       Lemonade Server URL for semantic embeddings");
    println!("  RUST_LOG           log level (error/warn/info/debug/trace)");
}
