// Integration tests for KnowledgeGraph facade and ObjectBuilder.
// This file is included from lib.rs via #[cfg(test)] #[path = "lib_tests.rs"] mod tests;

use tempfile::TempDir;

use crate::graph::MAX_CHUNK_TOKENS;
use crate::text::split_text;
use crate::types::{ChunkType, EdgeType};
use crate::{KnowledgeGraph, ObjectBuilder, ObjectTypeSchema, PropertySchema, SchemaStats, ValidationResult};

fn create_test_graph() -> (KnowledgeGraph, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(temp_dir.path()).unwrap();
    (graph, temp_dir)
}

async fn create_test_graph_async() -> (KnowledgeGraph, TempDir) {
    create_test_graph()
}

// ── Basic CRUD ────────────────────────────────────────────────────────────

#[test]
fn test_basic_graph_operations() {
    let (graph, _tmp) = create_test_graph();

    let gandalf_id = ObjectBuilder::character("Gandalf".to_string())
        .with_description("A wise wizard of great power".to_string())
        .with_property("race".to_string(), "Maiar".to_string())
        .with_tag("wizard".to_string())
        .add_to_graph(&graph)
        .unwrap();

    let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
        .with_description("A brave hobbit from the Shire".to_string())
        .with_property("race".to_string(), "Hobbit".to_string())
        .with_tag("ringbearer".to_string())
        .add_to_graph(&graph)
        .unwrap();

    graph
        .connect_objects_str(gandalf_id, frodo_id, "knows")
        .unwrap();

    let gandalf = graph.get_object(gandalf_id).unwrap().unwrap();
    assert_eq!(gandalf.name, "Gandalf");
    assert_eq!(gandalf.object_type, "character");

    let frodo = graph.get_object(frodo_id).unwrap().unwrap();
    assert_eq!(frodo.name, "Frodo Baggins");

    // Relationship
    let rels = graph.get_relationships(gandalf_id).unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].to, frodo_id);
    assert_eq!(rels[0].edge_type, EdgeType::from_str("knows"));

    // Neighbours
    let neighbours = graph.get_neighbors(gandalf_id).unwrap();
    assert_eq!(neighbours.len(), 1);
    assert_eq!(neighbours[0], frodo_id);

    // Text chunk
    let chunk_ids = graph
        .add_text_chunk(
            gandalf_id,
            "Gandalf appeared at Bilbo's birthday party.".to_string(),
            ChunkType::UserNote,
        )
        .unwrap();
    assert_eq!(chunk_ids.len(), 1);
    let chunk_id = chunk_ids[0];
    let chunks = graph.get_text_chunks(gandalf_id).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, chunk_id);

    // Subgraph
    let sg = graph.query_subgraph(gandalf_id, 1).unwrap();
    assert_eq!(sg.objects.len(), 2);
    assert_eq!(sg.edges.len(), 1);
    assert_eq!(sg.chunks.len(), 1);

    // Stats
    let stats = graph.get_stats().unwrap();
    assert_eq!(stats.node_count, 2);
    assert_eq!(stats.edge_count, 1);
    assert_eq!(stats.chunk_count, 1);
    assert!(stats.total_tokens > 0);
}

#[test]
fn test_find_by_name() {
    let (graph, _tmp) = create_test_graph();
    ObjectBuilder::character("Gandalf".to_string())
        .add_to_graph(&graph)
        .unwrap();

    let found = graph.find_by_name("character", "Gandalf").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Gandalf");

    // find_by_name_only (type-agnostic)
    let found_any = graph.find_by_name_only("Gandalf").unwrap();
    assert_eq!(found_any.len(), 1);
}

#[test]
fn test_weighted_relationships() {
    let (graph, _tmp) = create_test_graph();

    let sauron_id = ObjectBuilder::character("Sauron".to_string())
        .add_to_graph(&graph)
        .unwrap();
    let frodo_id = ObjectBuilder::character("Frodo".to_string())
        .add_to_graph(&graph)
        .unwrap();

    graph
        .connect_objects_weighted_str(sauron_id, frodo_id, "enemy_of", 0.9)
        .unwrap();

    let rels = graph.get_relationships(sauron_id).unwrap();
    assert_eq!(rels.len(), 1);
    assert!((rels[0].weight - 0.9).abs() < 1e-6);
    assert_eq!(rels[0].edge_type, EdgeType::from_str("enemy_of"));
}

#[test]
fn test_complex_world_scenario() {
    let (graph, _tmp) = create_test_graph();

    let shire_id = ObjectBuilder::location("The Shire".to_string())
        .add_to_graph(&graph)
        .unwrap();
    let bag_end_id = ObjectBuilder::location("Bag End".to_string())
        .add_to_graph(&graph)
        .unwrap();
    let frodo_id = ObjectBuilder::character("Frodo Baggins".to_string())
        .add_to_graph(&graph)
        .unwrap();
    let ring_id = ObjectBuilder::item("The One Ring".to_string())
        .add_to_graph(&graph)
        .unwrap();
    let fellowship_id = ObjectBuilder::faction("Fellowship of the Ring".to_string())
        .add_to_graph(&graph)
        .unwrap();

    graph
        .connect_objects_str(bag_end_id, shire_id, "located_in")
        .unwrap();
    graph
        .connect_objects_str(frodo_id, bag_end_id, "located_in")
        .unwrap();
    graph
        .connect_objects_str(frodo_id, ring_id, "owned_by")
        .unwrap();
    graph
        .connect_objects_str(frodo_id, fellowship_id, "member_of")
        .unwrap();

    let frodo_world = graph.query_subgraph(frodo_id, 2).unwrap();
    assert_eq!(frodo_world.objects.len(), 5);
    assert!(frodo_world.edges.len() >= 4);

    let stats = graph.get_stats().unwrap();
    assert_eq!(stats.node_count, 5);
    assert_eq!(stats.edge_count, 4);
}

#[test]
fn test_fts_search() {
    let (graph, _tmp) = create_test_graph();

    let obj_id = ObjectBuilder::character("Saruman".to_string())
        .add_to_graph(&graph)
        .unwrap();

    graph
        .add_text_chunk(
            obj_id,
            "Saruman the White was the head of the Istari order.".to_string(),
            ChunkType::Description,
        )
        .unwrap();

    // FTS5 exact-word search
    let results = graph.search_chunks_fts("Istari", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1, obj_id);
    assert!(results[0].2.contains("Istari"));

    // No match
    let empty = graph.search_chunks_fts("dragon", 5).unwrap();
    assert!(empty.is_empty());
}

// ── split_text (via add_text_chunk) ──────────────────────────────────────

#[test]
fn test_add_text_chunk_long_content_stored_as_multiple_chunks() {
    let (graph, _tmp) = create_test_graph();
    let obj_id = ObjectBuilder::character("Verbose".to_string())
        .add_to_graph(&graph)
        .unwrap();

    // 5000-char content: ~1250 tokens → must split into at least 3 chunks
    let long_content = "word ".repeat(1000); // 5000 chars
    let chunk_ids = graph
        .add_text_chunk(obj_id, long_content.clone(), ChunkType::Description)
        .unwrap();

    assert!(
        chunk_ids.len() >= 3,
        "expected ≥3 chunks for 5000-char content, got {}",
        chunk_ids.len()
    );

    // All chunks must be retrievable and within the token budget.
    let stored = graph.get_text_chunks(obj_id).unwrap();
    assert_eq!(stored.len(), chunk_ids.len());
    for chunk in &stored {
        assert!(
            chunk.token_count <= MAX_CHUNK_TOKENS,
            "chunk exceeds MAX_CHUNK_TOKENS: {} tokens",
            chunk.token_count
        );
    }

    // The concatenated content must cover all original words.
    let original_words: Vec<_> = long_content.split_whitespace().collect();
    let stored_words: Vec<_> = stored
        .iter()
        .flat_map(|c| c.content.split_whitespace())
        .collect();
    assert_eq!(
        original_words, stored_words,
        "stored chunks must cover all original words in order"
    );
}

// ── Schema integration ────────────────────────────────────────────────────

#[tokio::test]
async fn test_schema_integration() {
    let (graph, _tmp) = create_test_graph_async().await;

    let spell_schema =
        ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
            .with_property("level".to_string(), PropertySchema::number("Spell level"))
            .with_property(
                "school".to_string(),
                PropertySchema::string("School of magic"),
            )
            .with_required_property("level".to_string());

    graph
        .register_object_type("spell", spell_schema)
        .await
        .unwrap();

    let spell = ObjectBuilder::custom("spell".to_string(), "Fireball".to_string())
        .with_json_property(
            "level".to_string(),
            serde_json::Value::Number(serde_json::Number::from(3)),
        )
        .with_json_property(
            "school".to_string(),
            serde_json::Value::String("Evocation".to_string()),
        )
        .build();

    let validation = graph.validate_object(&spell).await.unwrap();
    assert!(
        validation.valid,
        "Expected valid spell: {:?}",
        validation.errors
    );

    let spell_id = graph.add_object_validated(spell).await.unwrap();
    let retrieved = graph.get_object(spell_id).unwrap().unwrap();
    assert_eq!(retrieved.name, "Fireball");
    assert_eq!(retrieved.object_type, "spell");

    let stats = graph.get_schema_stats("default").await.unwrap();
    assert!(stats.object_type_count >= 7); // 6 built-in + "spell"
}

#[tokio::test]
async fn test_validation_failure() {
    let (graph, _tmp) = create_test_graph_async().await;

    use crate::types::ObjectMetadata;
    let bad = ObjectMetadata::new("unknown_type_xyz".to_string(), "Test".to_string());
    let result = graph.validate_object(&bad).await.unwrap();
    assert!(!result.valid);
    assert!(!result.errors.is_empty());

    let insert_result = graph.add_object_validated(bad).await;
    assert!(insert_result.is_err());
}
