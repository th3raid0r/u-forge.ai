//! u-forge.ai - Universe Forge
//! 
//! A demonstration of the core knowledge graph functionality including vector search

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use u_forge_ai::{
    KnowledgeGraph, ObjectBuilder,
    VectorSearchEngine, VectorSearchConfig, SchemaIngestion,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum JsonEntry {
    #[serde(rename = "node")]
    Node {
        name: String,
        #[serde(rename = "nodeType")]
        node_type: String,
        metadata: Vec<String>,
    },
    #[serde(rename = "edge")]
    Edge {
        from: String,
        to: String,
        #[serde(rename = "edgeType")]
        edge_type: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    let data_file = if args.len() > 1 {
        args[1].clone()
    } else {
        "./examples/data/memory.json".to_string()
    };
    
    println!("🌟 Welcome to u-forge.ai (Universe Forge) 🌟");
    println!("Loading data from: {}", data_file);
    println!("Creating knowledge graph from JSON data...\n");

    // Create a temporary knowledge graph for demonstration
    let temp_dir = tempfile::TempDir::new()?;
    let db_path = temp_dir.path().join("db");
    std::fs::create_dir_all(&db_path)?;
    let embedding_cache_dir = temp_dir.path().join("embedding_cache");
    std::fs::create_dir_all(&embedding_cache_dir)?;

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

    // Load schemas from directory
    let schema_dir = "./examples/schemas";
    if Path::new(schema_dir).exists() {
        println!("\n📚 Loading Custom Schemas");
        println!("=========================");
        
        match SchemaIngestion::load_schemas_from_directory(schema_dir, "loaded_schemas", "1.0.0") {
            Ok(schema_definition) => {
                let schema_manager = graph.get_schema_manager();
                schema_manager.save_schema(&schema_definition).await?;
                
                println!("✅ Successfully loaded schema with {} object types:", schema_definition.object_types.len());
                for (type_name, _) in &schema_definition.object_types {
                    println!("   • {}", type_name);
                }
                println!();
            }
            Err(e) => {
                eprintln!("❌ Failed to load schemas from directory: {}", e);
                eprintln!("Continuing with default schemas...\n");
            }
        }
    }

    // Use the embedding provider from KnowledgeGraph for the VectorSearchEngine
    let embedding_provider = graph.get_embedding_provider();
    
    let mut vector_config = VectorSearchConfig::default();
    vector_config.dimensions = embedding_provider.dimensions()?; // Get dimensions from the provider
    
    let index_dir = temp_dir.path().join("index");
    std::fs::create_dir_all(&index_dir)?;
    let mut search_engine = VectorSearchEngine::new(vector_config, embedding_provider.clone(), index_dir)?;
    search_engine.initialize().await?;

    println!("Embedding provider obtained from KnowledgeGraph.\n");

    // Load and parse JSON data
    println!("📄 Loading JSON data from {}...", data_file);
    let file_content = fs::read_to_string(&data_file)?;
    
    let mut nodes: Vec<JsonEntry> = Vec::new();
    let mut edges: Vec<JsonEntry> = Vec::new();
    let mut name_to_id: HashMap<String, u_forge_ai::ObjectId> = HashMap::new();

    // Parse line-delimited JSON
    let mut parse_errors = 0;
    for (line_num, line) in file_content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        
        match serde_json::from_str::<JsonEntry>(line) {
            Ok(entry) => {
                match entry {
                    JsonEntry::Node { .. } => nodes.push(entry),
                    JsonEntry::Edge { .. } => edges.push(entry),
                }
            }
            Err(e) => {
                parse_errors += 1;
                eprintln!("⚠️ Line {}: Failed to parse JSON: {}", line_num + 1, e);
                if line.len() > 100 {
                    eprintln!("   Content preview: {}...", &line[..100]);
                } else {
                    eprintln!("   Content: {}", line);
                }
            }
        }
    }
    
    if parse_errors > 0 {
        eprintln!("⚠️ Total parse errors: {}", parse_errors);
    }

    println!("📊 Loaded {} nodes and {} edges from JSON", nodes.len(), edges.len());

    // Create nodes first
    println!("\n📍 Creating objects...");
    let mut created_count = 0;
    
    for entry in nodes {
        if let JsonEntry::Node { name, node_type, metadata } = entry {
            let object_id = match node_type.as_str() {
                "location" => {
                    let mut builder = ObjectBuilder::location(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "npc" => {
                    let mut builder = ObjectBuilder::character(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "player_character" => {
                    let mut builder = ObjectBuilder::character(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "faction" => {
                    let mut builder = ObjectBuilder::faction(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "quest" => {
                    let mut builder = ObjectBuilder::event(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "artifact" => {
                    let mut builder = ObjectBuilder::item(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "currency" => {
                    let mut builder = ObjectBuilder::item(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "inventory" => {
                    let mut builder = ObjectBuilder::item(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "transportation" => {
                    let mut builder = ObjectBuilder::item(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "setting_reference" => {
                    let mut builder = ObjectBuilder::event(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "system_reference" => {
                    let mut builder = ObjectBuilder::event(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "skills" => {
                    let mut builder = ObjectBuilder::item(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                "temporal" => {
                    let mut builder = ObjectBuilder::event(name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
                _ => {
                    // Default to a generic object for unknown types
                    let mut builder = ObjectBuilder::custom(node_type.clone(), name.clone());
                    builder = add_metadata_to_builder(builder, &metadata);
                    builder.add_to_graph(&graph)?
                }
            };
            
            name_to_id.insert(name, object_id);
            created_count += 1;
            
            if created_count % 50 == 0 {
                println!("   Created {} objects...", created_count);
            }
        }
    }
    
    println!("✅ Created {} objects total", created_count);

    // Create edges
    println!("\n🔗 Creating relationships...");
    let mut edge_count = 0;
    
    for entry in edges {
        if let JsonEntry::Edge { from, to, edge_type } = entry {
            if let (Some(&from_id), Some(&to_id)) = (name_to_id.get(&from), name_to_id.get(&to)) {
                // Use the edge type directly as a string - no mapping needed!
                graph.connect_objects_str(from_id, to_id, &edge_type)?;
                edge_count += 1;
                
                if edge_count % 100 == 0 {
                    println!("   Created {} relationships...", edge_count);
                }
            } else {
                if edge_count < 10 {  // Only show first 10 missing edge warnings to avoid spam
                    eprintln!("⚠️ Could not find objects for edge: {} -> {} ({})", from, to, edge_type);
                }
            }
        }
    }
    
    println!("✅ Created {} relationships total", edge_count);

    // Populate the vector search index
    println!("\n⚡ Populating vector search index...");
    let all_objects = graph.get_all_objects()?;
    
    struct TextToIndex {
        chunk_id: u_forge_ai::ChunkId,
        object_id: u_forge_ai::ObjectId,
        object_type: String,
        content: String,
    }
    let mut texts_to_index: Vec<TextToIndex> = Vec::new();

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
    println!("Vector search index populated and FST built.\n");

    // Demonstrate querying capabilities
    println!("\n🔍 Querying the knowledge graph...");
    
    // Get statistics
    let stats = graph.get_stats()?;
    println!("📊 Graph Statistics:");
    println!("   • Nodes: {}", stats.node_count);
    println!("   • Edges: {}", stats.edge_count);
    println!("   • Text chunks: {}", stats.chunk_count);
    println!("   • Total tokens: {}", stats.total_tokens);

    // Find a character to explore (try to find Frodo, or the first character)
    let sample_character = all_objects.iter()
        .find(|obj| obj.name.contains("Frodo") || obj.object_type == "character")
        .or_else(|| all_objects.iter().find(|obj| obj.object_type == "npc"))
        .or_else(|| all_objects.iter().find(|obj| obj.object_type == "player_character"));

    if let Some(character) = sample_character {
        let character_id = character.id;
        let character_name = &character.name;
        
        // Find character's neighbors
        let neighbors = graph.get_neighbors(character_id)?;
        println!("\n👥 {}'s direct connections: {} entities", character_name, neighbors.len());

        // Query subgraph around character
        let subgraph = graph.query_subgraph(character_id, 2)?;
        println!("\n🕸️ {}'s extended network (2 hops):", character_name);
        println!("   • Connected entities: {}", subgraph.objects.len());
        println!("   • Relationships: {}", subgraph.edges.len());
        println!("   • Text content: {} chunks", subgraph.chunks.len());

        // List some of the connected entities
        println!("\n📋 Entities in {}'s network:", character_name);
        for obj in subgraph.objects.iter().take(8) {
            println!("   • {} ({})", obj.name, obj.object_type.as_str());
        }

        // Show relationships
        let relationships = graph.get_relationships(character_id)?;
        println!("\n🔗 {}'s relationships:", character_name);
        for edge in relationships.iter().take(5) {
            if let (Some(from_obj), Some(to_obj)) = (
                graph.get_object(edge.from)?,
                graph.get_object(edge.to)?,
            ) {
                let (from_name, to_name) = if edge.from == character_id {
                    (character_name.clone(), to_obj.name)
                } else {
                    (from_obj.name, character_name.clone())
                };
                println!("   • {} --[{}]--> {}", from_name, edge.edge_type.as_str(), to_name);
            }
        }
    }

    // Demonstrate semantic search
    println!("\n🧠 Performing semantic search for 'emperor galactic empire':");
    let search_results = search_engine.search_hybrid("emperor galactic empire", 5, 5).await?;
    
    if !search_results.semantic_results.is_empty() {
        println!("   Semantic matches:");
        for (i, result) in search_results.semantic_results.iter().enumerate() {
            if let Some(obj) = graph.get_object(result.object_id)? {
                println!(
                    "      {}. {} ({:.2}%) - Type: {}, Preview: '{}'",
                    i + 1,
                    obj.name,
                    result.similarity * 100.0,
                    obj.object_type.as_str(),
                    result.text_preview
                );
            }
        }
    } else {
        println!("   No semantic matches found.");
    }

    if !search_results.exact_results.is_empty() {
        println!("   Exact matches (from FST):");
        for (i, result) in search_results.exact_results.iter().enumerate() {
            println!(
                "      {}. {} - Type: {}",
                i + 1,
                result.name,
                result.object_type.as_str()
            );
        }
    }

    println!("\n🧠 Performing semantic search for 'foundation terminus':");
    let foundation_results = search_engine.search_hybrid("foundation terminus", 3, 3).await?;
    if !foundation_results.semantic_results.is_empty() {
        println!("   Semantic matches:");
        for (i, result) in foundation_results.semantic_results.iter().enumerate() {
            if let Some(obj) = graph.get_object(result.object_id)? {
                println!(
                    "      {}. {} ({:.2}%) - Type: {}, Preview: '{}'",
                    i + 1,
                    obj.name,
                    result.similarity * 100.0,
                    obj.object_type.as_str(),
                    result.text_preview
                );
            }
        }
    } else {
        println!("   No semantic matches found for foundation terminus.");
    }

    println!("\n🧠 Performing semantic search for 'locations planets systems':");
    let location_results = search_engine.search_hybrid("locations planets systems", 3, 3).await?;
    if !location_results.semantic_results.is_empty() {
        println!("   Semantic matches:");
        for (i, result) in location_results.semantic_results.iter().enumerate() {
            if let Some(obj) = graph.get_object(result.object_id)? {
                println!(
                    "      {}. {} ({:.2}%) - Type: {}, Preview: '{}'",
                    i + 1,
                    obj.name,
                    result.similarity * 100.0,
                    obj.object_type.as_str(),
                    result.text_preview
                );
            }
        }
    } else {
        println!("   No semantic matches found for locations.");
    }

    println!("\n✨ Knowledge graph demonstration complete!");
    println!("📈 Summary:");
    println!("   • Loaded {} objects and {} relationships from JSON data", created_count, edge_count);
    println!("   • Schema types loaded: {}", all_objects.iter().map(|o| &o.object_type).collect::<std::collections::HashSet<_>>().len());
    println!("   • Vector embeddings: {} indexed", texts_count);
    println!("   • Storage: RocksDB with FastEmbed semantic search");
    println!("\nThis showcases u-forge.ai's ability to load large TTRPG datasets and perform intelligent search.");
    println!("Try running semantic searches with different queries to explore the {} universe!", 
             if data_file.contains("memory.json") { "Foundation" } else { "loaded" });

    Ok(())
}

fn add_metadata_to_builder(mut builder: ObjectBuilder, metadata: &[String]) -> ObjectBuilder {
    for meta_item in metadata {
        if let Some((key, value)) = meta_item.split_once(": ") {
            let key = key.trim();
            let value = value.trim();
            
            match key.to_lowercase().as_str() {
                "description" => {
                    builder = builder.with_description(value.to_string());
                }
                "background" => {
                    builder = builder.with_description(format!("Background: {}", value));
                }
                "content" => {
                    builder = builder.with_description(value.to_string());
                }
                _ => {
                    builder = builder.with_property(key.to_string(), value.to_string());
                }
            }
        } else {
            // If no colon separator, treat as a tag or additional description
            if meta_item.len() < 50 {
                builder = builder.with_tag(meta_item.clone());
            } else {
                builder = builder.with_description(meta_item.clone());
            }
        }
    }
    builder
}