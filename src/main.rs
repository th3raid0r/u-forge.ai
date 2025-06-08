//! u-forge.ai - Universe Forge
//! 
//! A demonstration of the core knowledge graph functionality

use anyhow::Result;
use u_forge_ai::{KnowledgeGraph, ObjectBuilder, EdgeType, ChunkType};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    println!("🌟 Welcome to u-forge.ai (Universe Forge) 🌟");
    println!("Creating a sample Middle-earth knowledge graph...\n");

    // Create a temporary knowledge graph for demonstration
    let temp_dir = tempfile::TempDir::new()?;
    let graph = KnowledgeGraph::new(temp_dir.path())?;

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

    // Create characters
    println!("👥 Creating characters...");
    let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
        .with_description("A hobbit of the Shire who inherited the One Ring from Bilbo".to_string())
        .with_property("race".to_string(), "Hobbit".to_string())
        .with_property("age".to_string(), "50".to_string())
        .with_tag("ringbearer".to_string())
        .with_tag("protagonist".to_string())
        .add_to_graph(&graph)?;

    let gandalf_id = ObjectBuilder::character("Gandalf the Grey".to_string())
        .with_description("A wizard of the Istari order, also known as Mithrandir".to_string())
        .with_property("race".to_string(), "Maiar".to_string())
        .with_property("order".to_string(), "Istari".to_string())
        .with_tag("wizard".to_string())
        .with_tag("mentor".to_string())
        .add_to_graph(&graph)?;

    let aragorn_id = ObjectBuilder::character("Aragorn".to_string())
        .with_description("Ranger of the North and heir to the throne of Gondor".to_string())
        .with_property("race".to_string(), "Man".to_string())
        .with_property("lineage".to_string(), "Isildur's heir".to_string())
        .with_tag("king".to_string())
        .with_tag("ranger".to_string())
        .add_to_graph(&graph)?;

    let legolas_id = ObjectBuilder::character("Legolas".to_string())
        .with_description("An elf of the Woodland Realm and prince of the Silvan Elves".to_string())
        .with_property("race".to_string(), "Elf".to_string())
        .with_property("weapon".to_string(), "bow".to_string())
        .with_tag("archer".to_string())
        .add_to_graph(&graph)?;

    // Create items
    println!("🗡️ Creating items...");
    let ring_id = ObjectBuilder::item("The One Ring".to_string())
        .with_description("The master ring created by the Dark Lord Sauron to control all other rings of power".to_string())
        .with_property("creator".to_string(), "Sauron".to_string())
        .with_property("material".to_string(), "gold".to_string())
        .with_tag("artifact".to_string())
        .with_tag("cursed".to_string())
        .add_to_graph(&graph)?;

    let sting_id = ObjectBuilder::item("Sting".to_string())
        .with_description("An elven short sword that glows blue when orcs are near".to_string())
        .with_property("type".to_string(), "sword".to_string())
        .with_property("origin".to_string(), "Gondolin".to_string())
        .with_tag("weapon".to_string())
        .with_tag("elven".to_string())
        .add_to_graph(&graph)?;

    // Create factions
    println!("🏛️ Creating factions...");
    let fellowship_id = ObjectBuilder::faction("Fellowship of the Ring".to_string())
        .with_description("Nine companions who set out from Rivendell to destroy the One Ring".to_string())
        .with_property("purpose".to_string(), "destroy the One Ring".to_string())
        .with_property("formed_at".to_string(), "Rivendell".to_string())
        .with_tag("alliance".to_string())
        .add_to_graph(&graph)?;

    // Create events
    println!("📜 Creating events...");
    let council_id = ObjectBuilder::event("Council of Elrond".to_string())
        .with_description("The meeting at Rivendell where the fate of the One Ring was decided".to_string())
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
    
    graph.add_text_chunk(
        frodo_id,
        "At the Council of Elrond, Frodo volunteered to bear the burden of the Ring, despite the immense danger. His courage in that moment changed the course of Middle-earth's history.".to_string(),
        ChunkType::UserNote,
    )?;

    graph.add_text_chunk(
        fellowship_id,
        "The Fellowship was formed with nine members to match the nine Nazgûl: four hobbits, two men, one elf, one dwarf, and one wizard. Each brought unique skills to aid in the quest.".to_string(),
        ChunkType::Description,
    )?;

    graph.add_text_chunk(
        ring_id,
        "One Ring to rule them all, One Ring to find them, One Ring to bring them all, and in the darkness bind them.".to_string(),
        ChunkType::Imported,
    )?;

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
    let frodo_matches = graph.find_by_name(u_forge_ai::ObjectType::Character, "Frodo Baggins")?;
    for character in frodo_matches {
        println!("   • Found: {} (ID: {})", character.name, character.id);
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
    println!("This showcases the core functionality of u-forge.ai's storage engine.");
    println!("The graph is stored locally in RocksDB with full CRUD operations,");
    println!("relationship management, and efficient querying capabilities.");

    Ok(())
}