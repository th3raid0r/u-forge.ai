// Integration tests for KnowledgeGraph facade and ObjectBuilder.
// This file is included from lib.rs via #[cfg(test)] #[path = "lib_tests.rs"] mod tests;

use tempfile::TempDir;

use crate::graph::MAX_CHUNK_TOKENS;
use crate::schema::ValidationRule;
use crate::types::{ChunkType, EdgeType};
use crate::{
    EdgeTypeSchema, EmbeddingTarget, GraphChange, KnowledgeGraph, ObjectBuilder, ObjectTypeSchema,
    PropertySchema,
};

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
    assert_eq!(rels[0].edge_type, EdgeType::new("knows"));

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
    assert_eq!(rels[0].edge_type, EdgeType::new("enemy_of"));
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

    // Generate content that is 3× the token limit so it must split into ≥3 chunks.
    // "word" is 1 token; with spaces each repetition stays ~1 token.
    let word_repeats = MAX_CHUNK_TOKENS * 3 + 1;
    let long_content = "word ".repeat(word_repeats);
    let chunk_ids = graph
        .add_text_chunk(obj_id, long_content.clone(), ChunkType::Description)
        .unwrap();

    assert!(
        chunk_ids.len() >= 3,
        "expected ≥3 chunks for {}-token content, got {}",
        word_repeats,
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
async fn registered_object_type_is_immediately_visible_and_enforced() {
    let (graph, _tmp) = create_test_graph_async().await;

    let spell_schema = ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
        .with_property(
            "level".to_string(),
            PropertySchema::number("Spell level")
                .with_validation(ValidationRule::new().with_value_range(Some(1.0), Some(5.0))),
        )
        .with_property(
            "school".to_string(),
            PropertySchema::string("School of magic"),
        )
        .with_required_property("level".to_string());

    graph
        .register_object_type("spell", spell_schema)
        .await
        .unwrap();

    assert!(graph.get_schema_manager().is_valid_object_type("spell"));

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

    let spell_id = graph.add_object(spell).unwrap();
    let retrieved = graph.get_object(spell_id).unwrap().unwrap();
    assert_eq!(retrieved.name, "Fireball");
    assert_eq!(retrieved.object_type, "spell");

    let unknown = ObjectBuilder::custom("incantation".to_string(), "Unknown".to_string()).build();
    let error = graph.add_object(unknown).unwrap_err().to_string();
    assert!(
        error.contains("Unknown object type 'incantation'"),
        "{error}"
    );

    let undeclared = ObjectBuilder::custom("spell".to_string(), "Wild Magic".to_string())
        .with_json_property("level".to_string(), serde_json::json!(1))
        .with_property("damage".to_string(), "8d6".to_string())
        .build();
    let error = graph.add_object(undeclared).unwrap_err().to_string();
    assert!(
        error.contains("Property 'damage' is not defined"),
        "{error}"
    );

    let missing = ObjectBuilder::custom("spell".to_string(), "Cantrip".to_string()).build();
    let error = graph.add_object(missing).unwrap_err().to_string();
    assert!(
        error.contains("Missing required property: level"),
        "{error}"
    );

    let coercible = ObjectBuilder::custom("spell".to_string(), "String Level".to_string())
        .with_property("level".to_string(), "3".to_string())
        .build();
    let error = graph.add_object(coercible).unwrap_err().to_string();
    assert!(error.contains("Expected: number, Got: string"), "{error}");

    let out_of_range = ObjectBuilder::custom("spell".to_string(), "Impossible".to_string())
        .with_json_property("level".to_string(), serde_json::json!(9))
        .build();
    let error = graph.add_object(out_of_range).unwrap_err().to_string();
    assert!(error.contains("maximum value is 5"), "{error}");

    let stats = graph.get_schema_stats("default").await.unwrap();
    assert!(stats.object_type_count >= 7); // 6 built-in + "spell"
}

#[tokio::test]
async fn registered_edge_type_is_immediately_visible_and_enforced() {
    let (graph, _tmp) = create_test_graph_async().await;

    graph
        .register_edge_type(
            "protects",
            EdgeTypeSchema::new("protects".to_string(), "Guards a place".to_string())
                .with_source_types(vec!["character".to_string()])
                .with_target_types(vec!["location".to_string()]),
        )
        .await
        .unwrap();

    assert!(graph.get_schema_manager().is_valid_edge_type("protects"));

    let guardian = graph
        .add_object(ObjectBuilder::character("Guardian".to_string()).build())
        .unwrap();
    let location = graph
        .add_object(
            ObjectBuilder::location("Sanctum".to_string())
                .with_property("type".to_string(), "temple".to_string())
                .build(),
        )
        .unwrap();
    let intruder = graph
        .add_object(ObjectBuilder::character("Intruder".to_string()).build())
        .unwrap();

    let error = graph
        .connect_objects_str(guardian, location, "invented_edge")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Unknown edge type 'invented_edge'"),
        "{error}"
    );

    let error = graph
        .connect_objects_str(guardian, intruder, "protects")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("does not allow target type 'character'"),
        "{error}"
    );

    graph
        .connect_objects_str(guardian, location, "protects")
        .unwrap();
    let relationships = graph.get_relationships(guardian).unwrap();
    assert_eq!(relationships.len(), 1);
    assert_eq!(relationships[0].edge_type.as_str(), "protects");
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

#[tokio::test]
async fn mutation_boundary_rejects_unknown_properties_once_schema_is_loaded() {
    let (graph, _tmp) = create_test_graph_async().await;
    graph
        .get_schema_manager()
        .load_schema("default")
        .await
        .unwrap();

    let object = ObjectBuilder::character("Strict".to_string())
        .with_property("not_declared".to_string(), "value".to_string())
        .build();
    let error = graph.add_object(object).unwrap_err().to_string();
    assert!(error.contains("not defined in schema"), "{error}");
}

#[tokio::test]
async fn committed_mutations_emit_graph_changes() {
    let (graph, _tmp) = create_test_graph_async().await;
    let mut changes = graph.subscribe_changes();
    let object = ObjectBuilder::character("Observed".to_string()).build();
    let id = graph.add_object(object).unwrap();

    assert_eq!(
        changes.recv().await.unwrap(),
        GraphChange::ObjectUpserted { id, created: true }
    );
}

#[tokio::test]
async fn mutation_boundary_rejects_unknown_edges_once_schema_is_loaded() {
    let (graph, _tmp) = create_test_graph_async().await;
    graph
        .get_schema_manager()
        .load_schema("default")
        .await
        .unwrap();
    let first = graph
        .add_object(ObjectBuilder::character("First".to_string()).build())
        .unwrap();
    let second = graph
        .add_object(ObjectBuilder::character("Second".to_string()).build())
        .unwrap();

    let error = graph
        .connect_objects_str(first, second, "invented_edge")
        .unwrap_err()
        .to_string();
    assert!(error.contains("Unknown edge type"), "{error}");
}

#[test]
fn embedding_lane_rejects_a_different_model_fingerprint() {
    let (graph, _tmp) = create_test_graph();
    graph
        .ensure_embedding_space(EmbeddingTarget::Standard, "model-a@768")
        .unwrap();
    graph
        .ensure_embedding_space(EmbeddingTarget::Standard, "model-a@768")
        .unwrap();

    let error = graph
        .ensure_embedding_space(EmbeddingTarget::Standard, "model-b@768")
        .unwrap_err()
        .to_string();
    assert!(error.contains("embedding space mismatch"), "{error}");
}
