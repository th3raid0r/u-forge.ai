//! u-forge.ai - Universe Forge
//! 
//! A demonstration of the core knowledge graph functionality including vector search

use anyhow::Result;
use std::env;
use u_forge_ai::{
    KnowledgeGraph,
    VectorSearchEngine, VectorSearchConfig,
    data_ingestion::DataIngestion,
    SchemaIngestion,
};

#[derive(Debug)]
struct TextToIndex {
    chunk_id: u_forge_ai::ForgeUuid,
    object_id: u_forge_ai::ObjectId,
    object_type: String,
    content: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    
    // Show usage if --help is provided
    if args.contains(&"--help".to_string()) || args.contains(&"-h".to_string()) {
        println!("🌟 u-forge.ai CLI Demo 🌟");
        println!();
        println!("Usage: {} [DATA_FILE] [SCHEMA_DIR]", args[0]);
        println!("       {} --help", args[0]);
        println!();
        println!("Arguments:");
        println!("  DATA_FILE   Path to JSON data file (default: ./defaults/data/memory.json)");
        println!("  SCHEMA_DIR  Path to schema directory (default: ./defaults/schemas)");
        println!();
        println!("Examples:");
        println!("  {}                                    # Use defaults", args[0]);
        println!("  {} custom.json                       # Custom data file", args[0]);
        println!("  {} custom.json ./schemas             # Custom data and schema", args[0]);
        println!("  {} ../defaults/data/memory.json ../defaults/schemas  # Relative paths", args[0]);
        return Ok(());
    }
    
    let data_file = if args.len() > 1 {
        args[1].clone()
    } else {
        std::env::var("UFORGE_DATA_FILE").unwrap_or_else(|_| "./defaults/data/memory.json".to_string())
    };
    
    let schema_dir = if args.len() > 2 {
        args[2].clone()
    } else {
        std::env::var("UFORGE_SCHEMA_DIR").unwrap_or_else(|_| "./defaults/schemas".to_string())
    };
    
    println!("🌟 Welcome to u-forge.ai (Universe Forge) 🌟");
    println!("Loading data from: {}", data_file);
    println!("Loading schemas from: {}", schema_dir);
    println!("Creating knowledge graph from JSON data...\n");

    // Create a temporary knowledge graph for demonstration
    let temp_dir = tempfile::TempDir::new()?;
    let db_path = temp_dir.path().join("db");
    std::fs::create_dir_all(&db_path)?;
    
    // Use persistent cache directory from environment variable instead of temp dir
    let embedding_cache_dir = std::env::var("FASTEMBED_CACHE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let path = std::path::PathBuf::from("./defaults/default_model_cache");
            std::fs::create_dir_all(&path).expect("Failed to create default model cache dir");
            path
        });
    
    println!("📦 Using embedding cache directory: {}", embedding_cache_dir.display());

    println!("🧠 Initializing KnowledgeGraph with local embedding models (FastEmbed)...");
    let graph_result = KnowledgeGraph::new(&db_path, Some(&embedding_cache_dir));
    
    let graph = match graph_result {
        Ok(g) => g,
        Err(e) => {
            eprintln!("🚨 Failed to initialize KnowledgeGraph: {}", e);
            eprintln!("This may be due to model download issues or other initialization errors.");
            eprintln!("The application will now exit.");
            return Err(e);
        }
    };
    println!("KnowledgeGraph initialized successfully.");

    // Use the new ingestion system
    let mut ingestion = DataIngestion::new(&graph);
    
    // Load schemas from directory using SchemaIngestion
    println!("\n📚 Loading Custom Schemas");
    println!("=========================");
    let schema_name = "imported_schemas";
    let schema_version = "1.0.0";
    
    match SchemaIngestion::load_schemas_from_directory(&schema_dir, schema_name, schema_version) {
        Ok(schema_definition) => {
            let schema_manager = graph.get_schema_manager();
            match schema_manager.save_schema(&schema_definition).await {
                Ok(()) => {
                    println!("✅ Successfully loaded {} object types from {}", 
                             schema_definition.object_types.len(), schema_dir);
                    for (type_name, _) in &schema_definition.object_types {
                        println!("   • {}", type_name);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Failed to save schema: {}", e);
                    eprintln!("Continuing with default schemas...\n");
                }
            }
        }
        Err(e) => {
            eprintln!("❌ Failed to load schemas from {}: {}", schema_dir, e);
            eprintln!("Continuing with default schemas...\n");
        }
    }

    // Import JSON data
    println!("\n📄 Importing JSON Data");
    println!("======================");
    if let Err(e) = ingestion.import_json_data(&data_file).await {
        eprintln!("❌ Failed to import data: {}", e);
        return Err(e);
    }

    let stats = ingestion.get_stats();
    println!("\n📊 Import Summary");
    println!("=================");
    println!("✅ Objects created: {}", stats.objects_created);
    println!("✅ Relationships created: {}", stats.relationships_created);
    println!("✅ Schema system: Enabled with validation");
    if stats.parse_errors > 0 {
        println!("⚠️  Parse errors: {}", stats.parse_errors);
    }

    // Set up vector search engine
    let embedding_provider = graph.get_embedding_provider();
    let dimensions = embedding_provider.dimensions()?;
    let mut vector_config = VectorSearchConfig::default();
    vector_config.dimensions = dimensions;
    
    let index_dir = temp_dir.path().join("index");
    std::fs::create_dir_all(&index_dir)?;
    let mut search_engine = VectorSearchEngine::new(vector_config, embedding_provider.clone(), index_dir)?;
    search_engine.initialize().await?;

    println!("\n🔍 Vector Search Ready");
    println!("======================");
    println!("Embedding provider initialized with {} dimensions", dimensions);

    // Demo some searches
    demo_searches(&graph, &mut search_engine).await?;

    Ok(())
}

async fn demo_searches(graph: &KnowledgeGraph, search_engine: &mut VectorSearchEngine) -> Result<()> {
    println!("\n🔍 Search Demonstrations");
    println!("========================\n");

    // Get all objects for indexing
    let all_objects = graph.get_all_objects()?;
    
    // Populate the vector search index
    println!("⚡ Populating vector search index...");
    let mut texts_to_index = Vec::new();
    
    for obj_meta in &all_objects {
        let chunks = graph.get_text_chunks(obj_meta.id)?;
        for chunk in chunks {
            texts_to_index.push(TextToIndex {
                chunk_id: chunk.id,
                object_id: obj_meta.id,
                object_type: obj_meta.object_type.clone(),
                content: chunk.content,
            });
        }
        
        // Add object name and description to index as well
        if let Some(desc) = &obj_meta.description {
            texts_to_index.push(TextToIndex {
                chunk_id: u_forge_ai::ForgeUuid::new_v4(),
                object_id: obj_meta.id,
                object_type: obj_meta.object_type.clone(),
                content: desc.clone(),
            });
        }
        texts_to_index.push(TextToIndex {
            chunk_id: u_forge_ai::ForgeUuid::new_v4(),
            object_id: obj_meta.id,
            object_type: obj_meta.object_type.clone(),
            content: obj_meta.name.clone(),
        });
    }

    let texts_count = texts_to_index.len();
    
    for item_to_index in texts_to_index {
        search_engine.add_chunk(
            item_to_index.chunk_id,
            item_to_index.object_id,
            &item_to_index.content,
        ).await?;
    }
    
    // Rebuild name index for FST
    let names_for_fst: Vec<(u_forge_ai::ObjectId, String, String)> = all_objects.iter()
        .map(|obj| (obj.id, obj.name.clone(), obj.object_type.clone()))
        .collect();
    search_engine.rebuild_name_index(names_for_fst)?;
    println!("Vector search index populated with {} items.\n", texts_count);

    // Get statistics
    let stats = graph.get_stats()?;
    println!("📊 Graph Statistics:");
    println!("   • Nodes: {}", stats.node_count);
    println!("   • Edges: {}", stats.edge_count);
    println!("   • Text chunks: {}", stats.chunk_count);
    println!("   • Total tokens: {}", stats.total_tokens);

    // Example searches
    let search_queries = vec![
        "ancient temple",
        "magical artifact",
        "political intrigue",
        "dangerous creature",
        "emperor galactic empire",
        "foundation terminus",
    ];

    for query in search_queries {
        println!("\n🔎 Searching for: '{}'", query);
        
        // Hybrid search combining exact and semantic matching
        let search_results = search_engine.search_hybrid(query, 3, 3).await?;
        
        if !search_results.semantic_results.is_empty() {
            println!("   Semantic matches:");
            for (i, result) in search_results.semantic_results.iter().enumerate() {
                if let Some(obj) = graph.get_object(result.object_id)? {
                    println!(
                        "      {}. {} ({:.1}%) - {}",
                        i + 1,
                        obj.name,
                        result.similarity * 100.0,
                        obj.object_type.as_str()
                    );
                }
            }
        }
        
        if !search_results.exact_results.is_empty() {
            println!("   Exact matches:");
            for (i, result) in search_results.exact_results.iter().enumerate() {
                println!(
                    "      {}. {} - {}",
                    i + 1,
                    result.name,
                    result.object_type.as_str()
                );
            }
        }
        
        if search_results.semantic_results.is_empty() && search_results.exact_results.is_empty() {
            println!("   No matches found.");
        }
    }

    // Find a sample character to explore relationships
    let sample_character = all_objects.iter()
        .find(|obj| obj.object_type == "character" || obj.object_type == "npc" || obj.object_type == "player_character");

    if let Some(character) = sample_character {
        let character_id = character.id;
        let character_name = &character.name;
        
        println!("\n👥 Exploring {}'s connections:", character_name);
        
        // Find character's neighbors
        let neighbors = graph.get_neighbors(character_id)?;
        println!("   • Direct connections: {} entities", neighbors.len());

        // Show some relationships
        let relationships = graph.get_relationships(character_id)?;
        println!("   • Relationships: {}", relationships.len());
        for edge in relationships.iter().take(5) {
            if let (Some(from_obj), Some(to_obj)) = (
                graph.get_object(edge.from).ok().flatten(),
                graph.get_object(edge.to).ok().flatten(),
            ) {
                let (from_name, to_name) = if edge.from == character_id {
                    (character_name.clone(), to_obj.name)
                } else {
                    (from_obj.name, character_name.clone())
                };
                println!("      • {} --[{}]--> {}", from_name, edge.edge_type.as_str(), to_name);
            }
        }
    }

    println!("\n✨ Knowledge graph demonstration complete!");
    println!("📈 Summary:");
    println!("   • Objects: {}", stats.node_count);
    println!("   • Relationships: {}", stats.edge_count);
    println!("   • Vector embeddings: {} indexed", texts_count);
    println!("   • Storage: RocksDB with FastEmbed semantic search");
    println!("\nThis showcases u-forge.ai's ability to load TTRPG datasets and perform intelligent search.");

    Ok(())
}