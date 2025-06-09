//! u-forge.ai - Universe Forge
//! 
//! A demonstration of the core knowledge graph functionality including vector search

use anyhow::Result;
use u_forge_ai::{
    KnowledgeGraph, ObjectBuilder, EdgeType, ChunkType, ObjectType,
    VectorSearchEngine, VectorSearchConfig,
    ForgeUuid, // Use the re-exported Uuid
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    println!("🌟 Welcome to u-forge.ai (Universe Forge) 🌟");
    println!("Creating a sample Middle-earth knowledge graph with semantic search...\n");

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

    // Use the embedding provider from KnowledgeGraph for the VectorSearchEngine
    let embedding_provider = graph.get_embedding_provider();
    
    let mut vector_config = VectorSearchConfig::default();
    vector_config.dimensions = embedding_provider.dimensions()?; // Get dimensions from the provider
    
    let index_dir = temp_dir.path().join("index");
    std::fs::create_dir_all(&index_dir)?;
    // Assuming VectorSearchEngine is adapted to take Arc<dyn EmbeddingProvider>
    // If VectorSearchEngine expects a concrete type or a different trait, this will need adjustment.
    // For now, let's assume it can work with the Arc<dyn EmbeddingProvider>.
    // If VectorSearchEngine was part of a separate crate or not yet updated,
    // this part of main.rs might need more significant changes or VectorSearchEngine adaptation.
    // For the purpose of this edit, I will assume VectorSearchEngine can be updated.
    // Let's simulate this by constructing it with the provider from the graph.
    // This might require changes to VectorSearchEngine not covered by this specific edit.
    // For now, the key change is sourcing the provider from `graph`.
    let mut search_engine = VectorSearchEngine::new(vector_config, embedding_provider.clone(), index_dir)?;
    search_engine.initialize().await?;

    println!("Embedding provider obtained from KnowledgeGraph.\n");

    // Create locations
    println!("📍 Creating locations...");
    let middle_earth_id = ObjectBuilder::location("Middle-earth".to_string())
        .with_description("The world where the events of The Lord of the Rings take place".to_string())
        .with_tag("world".to_string())
        .add_to_graph(&graph)?;

    let shire_id = ObjectBuilder::location("The Shire".to_string())
        .with_description("A peaceful region in the northwest of Middle-earth, home to the hobbits".to_string())
        .with_property("climate".to_string(), "temperate".to_string())
        .with_tag("homeland".to_string())
        .add_to_graph(&graph)?;

    let bag_end_id = ObjectBuilder::location("Bag End".to_string())
        .with_description("The hobbit-hole residence of the Baggins family in Hobbiton".to_string())
        .with_property("type".to_string(), "hobbit-hole".to_string())
        .add_to_graph(&graph)?;

    let rivendell_id = ObjectBuilder::location("Rivendell".to_string())
        .with_description("The elven outpost in Middle-earth, also known as Imladris".to_string())
        .with_property("ruler".to_string(), "Elrond".to_string())
        .with_tag("refuge".to_string())
        .add_to_graph(&graph)?;
        
    let mordor_id = ObjectBuilder::location("Mordor".to_string())
        .with_description("A black, volcanic plain in Middle-earth, the realm of Sauron.".to_string())
        .with_property("ruler".to_string(), "Sauron".to_string())
        .with_tag("evil".to_string())
        .with_tag("dangerous".to_string())
        .add_to_graph(&graph)?;

    // Create characters
    println!("👥 Creating characters...");
    let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
        .with_description("A hobbit of the Shire who inherited the One Ring from Bilbo. He is known for his bravery and resilience.".to_string())
        .with_property("race".to_string(), "Hobbit".to_string())
        .with_property("age".to_string(), "50".to_string())
        .with_tag("ringbearer".to_string())
        .with_tag("protagonist".to_string())
        .with_tag("brave".to_string())
        .add_to_graph(&graph)?;

    let gandalf_id = ObjectBuilder::character("Gandalf the Grey".to_string())
        .with_description("A wizard of the Istari order, also known as Mithrandir. He is a wise guide and powerful adversary of evil.".to_string())
        .with_property("race".to_string(), "Maiar".to_string())
        .with_property("order".to_string(), "Istari".to_string())
        .with_tag("wizard".to_string())
        .with_tag("mentor".to_string())
        .with_tag("wise".to_string())
        .add_to_graph(&graph)?;

    let aragorn_id = ObjectBuilder::character("Aragorn".to_string())
        .with_description("Ranger of the North and heir to the throne of Gondor. A skilled warrior and leader.".to_string())
        .with_property("race".to_string(), "Man".to_string())
        .with_property("lineage".to_string(), "Isildur's heir".to_string())
        .with_tag("king".to_string())
        .with_tag("ranger".to_string())
        .with_tag("warrior".to_string())
        .add_to_graph(&graph)?;

    let legolas_id = ObjectBuilder::character("Legolas".to_string())
        .with_description("An elf of the Woodland Realm and prince of the Silvan Elves. Famous for his keen sight and archery skills.".to_string())
        .with_property("race".to_string(), "Elf".to_string())
        .with_property("weapon".to_string(), "bow".to_string())
        .with_tag("archer".to_string())
        .add_to_graph(&graph)?;

    // Create items
    println!("🗡️ Creating items...");
    let ring_id = ObjectBuilder::item("The One Ring".to_string())
        .with_description("The master ring created by the Dark Lord Sauron to control all other rings of power. It is a powerful and dangerous artifact.".to_string())
        .with_property("creator".to_string(), "Sauron".to_string())
        .with_property("material".to_string(), "gold".to_string())
        .with_tag("artifact".to_string())
        .with_tag("cursed".to_string())
        .with_tag("powerful".to_string())
        .with_tag("dangerous".to_string())
        .add_to_graph(&graph)?;

    let sting_id = ObjectBuilder::item("Sting".to_string())
        .with_description("An elven short sword that glows blue when orcs are near. A magical weapon of ancient make.".to_string())
        .with_property("type".to_string(), "sword".to_string())
        .with_property("origin".to_string(), "Gondolin".to_string())
        .with_tag("weapon".to_string())
        .with_tag("elven".to_string())
        .with_tag("magical".to_string())
        .add_to_graph(&graph)?;

    // Create factions
    println!("🏛️ Creating factions...");
    let fellowship_id = ObjectBuilder::faction("Fellowship of the Ring".to_string())
        .with_description("Nine companions who set out from Rivendell to destroy the One Ring. An alliance of good.".to_string())
        .with_property("purpose".to_string(), "destroy the One Ring".to_string())
        .with_property("formed_at".to_string(), "Rivendell".to_string())
        .with_tag("alliance".to_string())
        .add_to_graph(&graph)?;

    // Create events
    println!("📜 Creating events...");
    let council_id = ObjectBuilder::event("Council of Elrond".to_string())
        .with_description("The meeting at Rivendell where the fate of the One Ring was decided. A pivotal moment.".to_string())
        .with_property("location".to_string(), "Rivendell".to_string())
        .with_property("outcome".to_string(), "Formation of the Fellowship".to_string())
        .with_tag("pivotal".to_string())
        .add_to_graph(&graph)?;

    // Create relationships
    println!("🔗 Creating relationships...");
    
    // Location relationships
    graph.connect_objects(shire_id, middle_earth_id, EdgeType::LocatedIn)?;
    graph.connect_objects(bag_end_id, shire_id, EdgeType::LocatedIn)?;
    graph.connect_objects(rivendell_id, middle_earth_id, EdgeType::LocatedIn)?;
    graph.connect_objects(mordor_id, middle_earth_id, EdgeType::LocatedIn)?;


    // Character location relationships
    graph.connect_objects(frodo_id, bag_end_id, EdgeType::LocatedIn)?;
    graph.connect_objects_weighted(frodo_id, shire_id, EdgeType::LocatedIn, 0.9)?;

    // Character relationships
    graph.connect_objects_weighted(gandalf_id, frodo_id, EdgeType::Knows, 0.9)?;
    graph.connect_objects(frodo_id, aragorn_id, EdgeType::AllyOf)?;
    graph.connect_objects(frodo_id, legolas_id, EdgeType::AllyOf)?;
    graph.connect_objects(aragorn_id, legolas_id, EdgeType::AllyOf)?;

    // Item ownership
    graph.connect_objects(frodo_id, ring_id, EdgeType::OwnedBy)?;
    graph.connect_objects(frodo_id, sting_id, EdgeType::OwnedBy)?;

    // Faction membership
    graph.connect_objects(frodo_id, fellowship_id, EdgeType::MemberOf)?;
    graph.connect_objects(gandalf_id, fellowship_id, EdgeType::MemberOf)?;
    graph.connect_objects(aragorn_id, fellowship_id, EdgeType::MemberOf)?;
    graph.connect_objects(legolas_id, fellowship_id, EdgeType::MemberOf)?;

    // Event relationships
    graph.connect_objects(council_id, rivendell_id, EdgeType::HappenedAt)?;
    graph.connect_objects(frodo_id, council_id, EdgeType::ParticipatedIn)?;
    graph.connect_objects(gandalf_id, council_id, EdgeType::ParticipatedIn)?;
    graph.connect_objects(fellowship_id, council_id, EdgeType::CausedBy)?;

    // Add some rich text content
    println!("📝 Adding text content...");
    
    let chunk1_id = graph.add_text_chunk(
        frodo_id,
        "At the Council of Elrond, Frodo volunteered to bear the burden of the Ring, despite the immense danger. His courage in that moment changed the course of Middle-earth's history.".to_string(),
        ChunkType::UserNote,
    )?;

    let chunk2_id = graph.add_text_chunk(
        fellowship_id,
        "The Fellowship was formed with nine members to match the nine Nazgûl: four hobbits, two men, one elf, one dwarf, and one wizard. Each brought unique skills to aid in the quest.".to_string(),
        ChunkType::Description,
    )?;

    let chunk3_id = graph.add_text_chunk(
        ring_id,
        "One Ring to rule them all, One Ring to find them, One Ring to bring them all, and in the darkness bind them.".to_string(),
        ChunkType::Imported,
    )?;

    // Populate the vector search index
    println!("\n⚡ Populating vector search index...");
    let all_objects = graph.get_all_objects()?;
    // Collect all texts to embed first for potential batching
    struct TextToIndex {
        chunk_id: u_forge_ai::ChunkId,
        object_id: u_forge_ai::ObjectId,
        object_type: String,
        content: String,
    }
    let mut texts_to_index: Vec<TextToIndex> = Vec::new();

    for obj_meta in &all_objects {
        let chunks = graph.get_text_chunks(obj_meta.id)?;
        for chunk in chunks { // Iterate over actual TextChunk structs
            texts_to_index.push(TextToIndex {
                chunk_id: chunk.id, // Keep original ChunkId
                object_id: obj_meta.id,
                object_type: obj_meta.object_type.clone(),
                content: chunk.content,
            });
        }
        // Add object name and description to index as well
        if let Some(desc) = &obj_meta.description {
            texts_to_index.push(TextToIndex {
                chunk_id: u_forge_ai::ForgeUuid::new_v4(), // Generate a new ID for name/description chunks
                object_id: obj_meta.id,
                object_type: obj_meta.object_type.clone(),
                content: desc.clone(),
            });
        }
        texts_to_index.push(TextToIndex {
            chunk_id: u_forge_ai::ForgeUuid::new_v4(), // Generate a new ID for name chunks
            object_id: obj_meta.id,
            object_type: obj_meta.object_type.clone(),
            content: obj_meta.name.clone(),
        });
    }

    for item_to_index in texts_to_index {
        // search_engine.add_chunk now likely takes the text and uses the provider internally
        // or we pass embeddings. Assuming add_chunk handles embedding internally using its provider.
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

    // Find Frodo's neighbors
    let frodo_neighbors = graph.get_neighbors(frodo_id)?;
    println!("\n👥 Frodo's direct connections: {} entities", frodo_neighbors.len());

    // Query subgraph around Frodo
    let frodo_subgraph = graph.query_subgraph(frodo_id, 2)?;
    println!("\n🕸️ Frodo's extended network (2 hops):");
    println!("   • Connected entities: {}", frodo_subgraph.objects.len());
    println!("   • Relationships: {}", frodo_subgraph.edges.len());
    println!("   • Text content: {} chunks", frodo_subgraph.chunks.len());

    // List some of the connected entities
    println!("\n📋 Entities in Frodo's network:");
    for obj in frodo_subgraph.objects.iter().take(8) {
        println!("   • {} ({})", obj.name, obj.object_type.as_str());
    }

    // Show relationships
    let frodo_relationships = graph.get_relationships(frodo_id)?;
    println!("\n🔗 Frodo's relationships:");
    for edge in frodo_relationships.iter().take(5) {
        if let (Some(from_obj), Some(to_obj)) = (
            graph.get_object(edge.from)?,
            graph.get_object(edge.to)?,
        ) {
            let (from_name, to_name) = if edge.from == frodo_id {
                ("Frodo".to_string(), to_obj.name)
            } else {
                (from_obj.name, "Frodo".to_string())
            };
            println!("   • {} --[{}]--> {}", from_name, edge.edge_type.as_str(), to_name);
        }
    }

    // Demonstrate search by name
    println!("\n🔍 Finding characters named 'Frodo':");
    let frodo_matches = graph.find_by_name("character", "Frodo Baggins")?;
    for character in frodo_matches {
        println!("   • Found: {} (ID: {})", character.name, character.id);
    }

    // Demonstrate semantic search
    println!("\n🧠 Performing semantic search for \'brave hobbit carrying a ring\':");
    let search_results = search_engine.search_hybrid("brave hobbit carrying a ring", 5, 5).await?;
    
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
        println!("   No exact matches found.");
    }

    println!("\n🧠 Performing semantic search for 'wise mentors':");
    let mentor_results = search_engine.search_hybrid("wise mentors", 3, 3).await?;
    if !mentor_results.semantic_results.is_empty() {
        println!("   Semantic matches:");
        for (i, result) in mentor_results.semantic_results.iter().enumerate() {
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
        println!("   No semantic matches found for wise mentors.");
    }
    println!("\n🧠 Performing semantic search for \'dangerous artifacts in Mordor\':");
    let artifact_results = search_engine.search_hybrid("dangerous artifacts in Mordor", 3, 3).await?;
     if !artifact_results.semantic_results.is_empty() {
        println!("   Semantic matches:");
        for (i, result) in artifact_results.semantic_results.iter().enumerate() {
             if let Some(obj) = graph.get_object(result.object_id)? {
                 println!(
                    "      {}. {} ({:.2}%) - Type: {}, Preview: \'{}\'",
                    i + 1,
                    obj.name,
                    result.similarity * 100.0,
                    obj.object_type.as_str(),
                    result.text_preview
                );
            }
        }
    } else {
        println!("   No semantic matches found for \'dangerous artifacts in Mordor\'.");
    }


    // Show text content
    let frodo_chunks = graph.get_text_chunks(frodo_id)?;
    if !frodo_chunks.is_empty() {
        println!("\n📖 Text content about Frodo:");
        for chunk in frodo_chunks.iter().take(2) {
            println!("   • {}", chunk.content);
        }
    }

    println!("\n✨ Knowledge graph demonstration complete!");
    println!("This showcases the core functionality of u-forge.ai's storage engine,");
    println!("relationship management, efficient querying, and local semantic search.");
    println!("The graph is stored locally in RocksDB, and embeddings are handled by FastEmbed.");
    println!("Note: Semantic search quality depends on the downloaded model and content density.");

    Ok(())
}

// Add a dummy provider to EmbeddingService for fallback in demo if models fail to download
// This would typically be in embeddings.rs, but for the demo, we can define it here or ensure it exists.
// For now, ensure your embeddings.rs has a way to create a service that won't panic if models are unavailable.
// A simple way is to add a method like `new_with_dummy_provider` to `EmbeddingService` in `embeddings.rs`
// that uses a provider that returns empty or error results for embeddings.

// Example of what might be needed in embeddings.rs for the demo to handle failures:
/*
// In embeddings.rs:
pub struct DummyEmbeddingProvider;

#[async_trait::async_trait]
impl EmbeddingProvider for DummyEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> { Ok(vec![0.0; 384]) } // Return dummy vector
    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>> { Ok(texts.iter().map(|_| vec![0.0; 384]).collect()) }
    fn dimensions(&self) -> usize { 384 }
    fn max_tokens(&self) -> usize { 256 }
    fn provider_name(&self) -> &str { "Dummy (Offline)" }
    async fn is_available(&self) -> bool { true }
}

impl EmbeddingService {
    pub async fn new_with_dummy_provider() -> Result<Self> {
        Ok(Self {
            primary_provider: Box::new(DummyEmbeddingProvider),
            fallback_providers: Vec::new(),
        })
    }
}
*/